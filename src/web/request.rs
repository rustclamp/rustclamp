use std::any::Any;
use std::fmt;
use std::io::{BufRead, Read};
use std::net::IpAddr;
use std::sync::Arc;

use super::{Response, Session, error};

/// Longest accepted request line or header line, in bytes.
const MAX_LINE: u64 = 8 * 1024;
/// Most headers accepted on one request.
const MAX_HEADERS: usize = 100;
/// Largest accepted request body, in bytes, except multipart forms (see
/// [`MAX_UPLOAD`](super::MAX_UPLOAD)). Larger bodies get `413`.
pub const MAX_BODY: usize = 1024 * 1024;

/// An incoming HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Request {
    /// The request method, such as `GET`.
    pub method: String,
    /// The request path without its query string or fragment.
    pub path: String,
    /// The raw query string, without the leading `?`.
    pub query: String,
    /// Header names (lowercased) and values, in arrival order.
    pub headers: Vec<(String, String)>,
    /// The request body.
    pub body: Vec<u8>,
    /// The address of the connecting client, when known.
    pub peer: Option<IpAddr>,
    /// Set by [`Sessions::middleware`](super::Sessions::middleware).
    pub(super) session: Option<Session>,
    /// `{name}` route segments, set when the route matches.
    pub(super) params: Vec<(String, String)>,
    /// Values from [`Router::state`](super::Router::state).
    pub(super) state: State,
    /// Whether the client lets the connection stay open for another request:
    /// HTTP/1.1 without `Connection: close`.
    pub(super) keep_alive: bool,
}

/// Values the app shares with every handler, such as its database.
#[derive(Clone, Default)]
pub(super) struct State(pub(super) Arc<Vec<Arc<dyn Any + Send + Sync>>>);

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State").finish_non_exhaustive()
    }
}

// Requests compare by what the client sent; shared state is not part of that.
impl PartialEq for State {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for State {}

impl Request {
    /// Creates a request, for example to call routes from a test. A `?` in
    /// `target` starts the query string.
    pub fn new(method: &str, target: &str) -> Self {
        let target = target.split('#').next().unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        Self {
            method: method.into(),
            path: path.into(),
            query: query.into(),
            headers: Vec::new(),
            body: Vec::new(),
            peer: None,
            session: None,
            params: Vec::new(),
            state: State::default(),
            keep_alive: false,
        }
    }

    /// A `GET` request for `target`.
    pub fn get(target: &str) -> Self {
        Self::new("GET", target)
    }

    /// A `POST` request for `target`.
    pub fn post(target: &str) -> Self {
        Self::new("POST", target)
    }

    /// Adds a header, as a client would send it.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    /// Sets the body.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Sets the client address.
    #[must_use]
    pub fn with_peer(mut self, peer: IpAddr) -> Self {
        self.peer = Some(peer);
        self
    }

    /// The first value of header `name`, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The decoded query parameter `key`.
    pub fn query(&self, key: &str) -> Option<String> {
        field(&self.query, key)
    }

    /// The decoded field `key` of a URL-encoded or multipart form body.
    pub fn form(&self, key: &str) -> Option<String> {
        if let Some(value) = super::upload::form_field(self, key) {
            return value;
        }
        field(&String::from_utf8_lossy(&self.body), key)
    }

    /// The route segment `{name}`, such as `slug` in `/blog/{slug}`. Not decoded.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The visitor's session, when the route runs behind
    /// [`Sessions::middleware`](super::Sessions::middleware).
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// The value of type `T` the app added with
    /// [`Router::state`](super::Router::state).
    pub fn state<T: Any>(&self) -> Option<&T> {
        self.state.0.iter().find_map(|value| value.downcast_ref())
    }

    /// The value of cookie `name`.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == "cookie")
            .flat_map(|(_, value)| value.split(';'))
            .find_map(|pair| {
                let (key, value) = pair.trim().split_once('=')?;
                (key == name).then_some(value)
            })
    }
}

/// Reads one request: request line, headers, then a `Content-Length` body.
/// A malformed or oversized request is answered with the returned error.
pub(super) fn parse(reader: &mut impl BufRead, peer: Option<IpAddr>) -> Result<Request, Response> {
    let line = read_line(reader)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(error(400));
    };
    let mut request = Request::new(method, target);
    request.peer = peer;
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        if request.headers.len() == MAX_HEADERS {
            return Err(error(431));
        }
        let (name, value) = line.split_once(':').ok_or_else(|| error(400))?;
        request = request.with_header(name.trim(), value.trim());
    }
    request.keep_alive = version == "HTTP/1.1"
        && !request
            .header("connection")
            .is_some_and(|value| value.eq_ignore_ascii_case("close"));
    // ponytail: no chunked bodies; clients that stream must send Content-Length
    if request.header("transfer-encoding").is_some() {
        return Err(error(411));
    }
    if let Some(length) = request.header("content-length") {
        let length: usize = length.parse().map_err(|_| error(400))?;
        let multipart = request
            .header("content-type")
            .is_some_and(super::upload::is_multipart);
        let max = if multipart {
            super::MAX_UPLOAD
        } else {
            MAX_BODY
        };
        if length > max {
            return Err(error(413));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).map_err(|_| error(400))?;
        request.body = body;
    }
    Ok(request)
}

/// One line without its line ending. Longer than [`MAX_LINE`] is `431`.
fn read_line(reader: &mut impl BufRead) -> Result<String, Response> {
    let mut line = Vec::new();
    reader
        .take(MAX_LINE + 1)
        .read_until(b'\n', &mut line)
        .map_err(|_| error(400))?;
    if line.len() as u64 > MAX_LINE {
        return Err(error(431));
    }
    let line = String::from_utf8(line).map_err(|_| error(400))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// The decoded value of `key` in `a=1&b=2`-style `pairs`.
pub(super) fn field(pairs: &str, key: &str) -> Option<String> {
    pairs.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(name) == key).then(|| decode(value))
    })
}

/// Percent-encodes `text` for a form body or query string.
pub(super) fn encode(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Percent-decodes `text`, reading `+` as a space. Invalid escapes stay as they are.
pub(super) fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_raw(raw: &str) -> Result<Request, u16> {
        parse(&mut raw.as_bytes(), None).map_err(|response| response.status)
    }

    #[test]
    fn parses_line_headers_query_and_body() {
        let request = parse_raw(
            "POST /contact?ref=home%20page HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: a=1; session=abc\r\nContent-Length: 20\r\n\r\nname=Neo+C&msg=hi%21extra",
        )
        .unwrap();
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/contact")
        );
        assert_eq!(request.query("ref").as_deref(), Some("home page"));
        assert_eq!(request.header("HOST"), Some("x"));
        assert_eq!(request.cookie("session"), Some("abc"));
        assert_eq!(request.body, b"name=Neo+C&msg=hi%21");
        assert_eq!(request.form("name").as_deref(), Some("Neo C"));
        assert_eq!(request.form("msg").as_deref(), Some("hi!"));
    }

    #[test]
    fn refuses_malformed_and_oversized_requests() {
        assert_eq!(parse_raw("").unwrap_err(), 400);
        assert_eq!(parse_raw("GET /\r\n\r\n").unwrap_err(), 400);
        assert_eq!(
            parse_raw("GET / HTTP/1.1\r\nno colon\r\n\r\n").unwrap_err(),
            400
        );
        assert_eq!(
            parse_raw("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap_err(),
            411
        );
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(parse_raw(&big).unwrap_err(), 413);
        // Multipart forms may be larger, up to MAX_UPLOAD; this body is
        // missing, so it fails later with 400.
        let upload = |length| {
            format!(
                "POST / HTTP/1.1\r\nContent-Type: multipart/form-data; boundary=b\r\nContent-Length: {length}\r\n\r\n"
            )
        };
        assert_eq!(parse_raw(&upload(MAX_BODY + 1)).unwrap_err(), 400);
        assert_eq!(
            parse_raw(&upload(super::super::MAX_UPLOAD + 1)).unwrap_err(),
            413
        );
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_LINE as usize));
        assert_eq!(parse_raw(&long).unwrap_err(), 431);
        let many = format!(
            "GET / HTTP/1.1\r\n{}\r\n",
            "X: 1\r\n".repeat(MAX_HEADERS + 1)
        );
        assert_eq!(parse_raw(&many).unwrap_err(), 431);
        assert_eq!(
            parse_raw("POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab").unwrap_err(),
            400
        );
    }

    #[test]
    fn decode_handles_bad_escapes() {
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%41"), "%zzA");
        assert_eq!(decode("a+b%2Bc"), "a b+c");
    }
}
