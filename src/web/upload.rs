//! Files sent by `<form method="post" enctype="multipart/form-data">`.
//!
//! [`Request::file`] reads one; [`Request::form`] and [`Form`](super::Form)
//! validation read the text fields of the same form, CSRF token included.
//! [`UploadedFile::store`] saves a file on a [`Disk`] under a new name: the
//! name the browser sent is never used as a path.
//!
//! ```
//! use rustclamp::web::Request;
//!
//! let body = "--b\r\n\
//!     Content-Disposition: form-data; name=\"title\"\r\n\r\nHoliday\r\n\
//!     --b\r\n\
//!     Content-Disposition: form-data; name=\"photo\"; filename=\"Beach.JPG\"\r\n\
//!     Content-Type: image/jpeg\r\n\r\n<jpeg bytes>\r\n--b--\r\n";
//! let request = Request::post("/photos")
//!     .with_header("content-type", "multipart/form-data; boundary=b")
//!     .with_body(body);
//! assert_eq!(request.form("title").as_deref(), Some("Holiday"));
//! let photo = request.file("photo").unwrap();
//! assert_eq!(photo.extension().as_deref(), Some("jpg"));
//! assert_eq!(photo.bytes, b"<jpeg bytes>");
//! ```

use std::io;

use super::Request;
use crate::storage::Disk;

/// Largest accepted `multipart/form-data` body, in bytes. Other bodies stay
/// under [`MAX_BODY`](super::MAX_BODY). Larger bodies get `413`.
pub const MAX_UPLOAD: usize = 10 * 1024 * 1024;

/// A file from a multipart form.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UploadedFile {
    /// The form field, such as `photo`.
    pub field: String,
    /// The file name the browser sent. Untrusted: show it, never use it as a
    /// path.
    pub name: String,
    /// The type the browser claimed, such as `image/png`. Untrusted.
    pub content_type: String,
    /// The file's contents.
    pub bytes: Vec<u8>,
}

impl UploadedFile {
    /// The lowercase extension of [`name`](Self::name), such as `jpg`, when it
    /// is 1 to 10 letters and digits.
    pub fn extension(&self) -> Option<String> {
        let (_, extension) = self.name.rsplit_once('.')?;
        let valid = (1..=10).contains(&extension.len())
            && extension.bytes().all(|byte| byte.is_ascii_alphanumeric());
        valid.then(|| extension.to_ascii_lowercase())
    }

    /// The size in bytes.
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// Saves the file in `folder` on `disk` as a new UUIDv7 name with the
    /// file's extension, and returns its path on the disk, such as
    /// `photos/0192….jpg`. Only extensions in `allowed` are kept: a public
    /// disk serves `.html` or `.svg` as pages, so an unchecked upload would
    /// let anyone put script on the site.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when the extension is missing or not in `allowed`;
    /// otherwise whatever writing the file returns.
    pub fn store(&self, disk: &Disk, folder: &str, allowed: &[&str]) -> io::Result<String> {
        let extension = self
            .extension()
            .filter(|extension| allowed.iter().any(|ok| ok.eq_ignore_ascii_case(extension)))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{:?} is not a {} file", self.name, allowed.join(", ")),
                )
            })?;
        let name = format!("{}.{extension}", crate::uuid::Uuid::v7());
        let folder = folder.trim_matches('/');
        let path = if folder.is_empty() {
            name
        } else {
            format!("{folder}/{name}")
        };
        disk.put(&path, &self.bytes)?;
        Ok(path)
    }
}

impl Request {
    /// The file sent in field `field`, when the form is multipart and a file
    /// was chosen.
    pub fn file(&self, field: &str) -> Option<UploadedFile> {
        self.files(field).into_iter().next()
    }

    /// Every file sent in field `field`, for `<input type="file" multiple>`.
    pub fn files(&self, field: &str) -> Vec<UploadedFile> {
        parts(self)
            .unwrap_or_default()
            .into_iter()
            .filter(|part| part.name == field)
            .filter_map(|part| {
                let name = part.filename?;
                // A file input left empty still sends a part, with no name.
                (!name.is_empty()).then(|| UploadedFile {
                    field: part.name,
                    name,
                    content_type: part.content_type,
                    bytes: part.data.to_vec(),
                })
            })
            .collect()
    }
}

/// The text field `key` when `request` is multipart, `None` when it is not.
pub(super) fn form_field(request: &Request, key: &str) -> Option<Option<String>> {
    let parts = parts(request)?;
    Some(
        parts
            .into_iter()
            .find(|part| part.filename.is_none() && part.name == key)
            .map(|part| String::from_utf8_lossy(part.data).into_owned()),
    )
}

/// Whether `content_type` is `multipart/form-data`.
pub(super) fn is_multipart(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("multipart/form-data"))
}

struct Part<'a> {
    name: String,
    filename: Option<String>,
    content_type: String,
    data: &'a [u8],
}

/// The parts of a multipart body; `None` when the request is not multipart.
/// A malformed body yields the parts before the damage.
fn parts(request: &Request) -> Option<Vec<Part<'_>>> {
    let content_type = request.header("content-type")?;
    if !is_multipart(content_type) {
        return None;
    }
    let boundary = param(content_type, "boundary")?;
    let delimiter = [b"\r\n--", boundary.as_bytes()].concat();
    // The first delimiter may start the body, without the line break before it.
    let body = [b"\r\n".as_slice(), &request.body].concat();
    let mut rest = &request.body[..0];
    if let Some(start) = find(&body, &delimiter) {
        let offset = start + delimiter.len();
        // `body` is the original with two bytes in front.
        rest = &request.body[offset.saturating_sub(2)..];
    }
    let mut parts = Vec::new();
    while let Some(after) = rest.strip_prefix(b"\r\n") {
        let Some(head_end) = find(after, b"\r\n\r\n") else {
            break;
        };
        let head = String::from_utf8_lossy(&after[..head_end]);
        let content = &after[head_end + 4..];
        let Some(end) = find(content, &delimiter) else {
            break;
        };
        let mut part = Part {
            name: String::new(),
            filename: None,
            content_type: String::new(),
            data: &content[..end],
        };
        for line in head.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("content-disposition") {
                part.name = param(value, "name").unwrap_or_default();
                part.filename = param(value, "filename");
            } else if key.trim().eq_ignore_ascii_case("content-type") {
                part.content_type = value.trim().to_owned();
            }
        }
        parts.push(part);
        // After a delimiter: `\r\n` starts the next part, `--` ends the body.
        rest = &content[end + delimiter.len()..];
    }
    Some(parts)
}

/// The `key=value` parameter of a header value, quotes removed.
fn param(header: &str, key: &str) -> Option<String> {
    // ponytail: a `;` inside a quoted value splits it; only file names could
    // carry one, and they are used for their extension alone
    header.split(';').skip(1).find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        name.trim()
            .eq_ignore_ascii_case(key)
            .then(|| value.to_owned())
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    // ponytail: naive scan, fine up to MAX_UPLOAD; memchr-style search if
    // uploads grow much larger
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Disk;

    fn request(body: &str) -> Request {
        Request::post("/")
            .with_header("Content-Type", "multipart/form-data; boundary=\"XyZ\"")
            .with_body(body)
    }

    const BODY: &str = "preamble\r\n--XyZ\r\n\
        Content-Disposition: form-data; name=\"_token\"\r\n\r\nabc\r\n\
        --XyZ\r\n\
        content-disposition: form-data; name=\"docs\"; filename=\"a.PDF\"\r\n\
        Content-Type: application/pdf\r\n\r\nline one\r\nline two --XyZ\r\n\
        --XyZ\r\n\
        Content-Disposition: form-data; name=\"docs\"; filename=\"b.txt\"\r\n\r\n\r\n\
        --XyZ\r\n\
        Content-Disposition: form-data; name=\"empty\"; filename=\"\"\r\n\r\n\r\n\
        --XyZ--\r\n";

    #[test]
    fn reads_text_fields_and_files() {
        let request = request(BODY);
        assert_eq!(request.form("_token").as_deref(), Some("abc"));
        assert_eq!(request.form("docs"), None);
        let docs = request.files("docs");
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].name, "a.PDF");
        assert_eq!(docs[0].content_type, "application/pdf");
        assert_eq!(docs[0].bytes, b"line one\r\nline two --XyZ");
        assert_eq!(docs[1].bytes, b"");
        assert_eq!(request.file("empty"), None);
    }

    #[test]
    fn url_encoded_forms_are_unchanged() {
        let request = Request::post("/").with_body("a=1");
        assert_eq!(request.form("a").as_deref(), Some("1"));
        assert_eq!(request.file("a"), None);
    }

    #[test]
    fn truncated_bodies_keep_the_complete_parts() {
        let cut = &BODY[..BODY.find("b.txt").unwrap()];
        let request = request(cut);
        assert_eq!(request.form("_token").as_deref(), Some("abc"));
        assert_eq!(request.files("docs").len(), 1);
    }

    #[test]
    fn extension_is_checked_not_trusted() {
        let file = |name: &str| UploadedFile {
            field: "f".into(),
            name: name.into(),
            content_type: String::new(),
            bytes: b"x".to_vec(),
        };
        assert_eq!(file("a.tar.GZ").extension().as_deref(), Some("gz"));
        assert_eq!(file("noext").extension(), None);
        assert_eq!(file("x.p/ng").extension(), None);

        let root = std::env::temp_dir().join(format!("rustclamp-upload-{}", std::process::id()));
        let disk = Disk::new(&root);
        let saved = file("../../evil.PNG")
            .store(&disk, "/avatars/", &["png"])
            .unwrap();
        assert!(saved.starts_with("avatars/") && saved.ends_with(".png"));
        assert_eq!(disk.get(&saved).unwrap(), b"x");
        let refused = file("page.html").store(&disk, "avatars", &["png", "jpg"]);
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(root).unwrap();
    }
}
