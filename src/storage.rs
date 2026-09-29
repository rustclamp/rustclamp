//! Files on named disks, like Laravel's `Storage`.
//!
//! Enabled by the `storage` feature (the `web` feature includes it). A disk
//! is a folder; paths on it are relative and can never leave it. An app
//! lists its disks in `app/config/filesystems.rs`: `local`
//! (`storage/app/private`) for the app's own files and `public`
//! (`storage/app/public`), served at `/storage` through the `public/storage`
//! link that [`Storage::link`] makes.
//!
//! ```
//! use std::collections::HashMap;
//! use rustclamp::storage::{Disk, Settings, Storage};
//!
//! let root = std::env::temp_dir().join("rustclamp-storage-doc");
//! let storage = Storage::new(Settings {
//!     default: "local".into(),
//!     disks: HashMap::from([("local".into(), Disk::new(&root))]),
//!     links: Vec::new(),
//! });
//! storage.put("notes/hello.txt", "Hello").unwrap();
//! assert_eq!(storage.get("notes/hello.txt").unwrap(), b"Hello");
//! assert!(storage.put("../escape.txt", "no").is_err());
//! storage.delete("notes/hello.txt").unwrap();
//! assert!(!storage.exists("notes/hello.txt"));
//! ```

use std::collections::HashMap;
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};

/// A folder files are kept in.
#[derive(Debug, Clone)]
pub struct Disk {
    /// The folder, created on the first write.
    pub root: PathBuf,
    /// Where the web serves the disk from, such as `/storage`; `None` when it
    /// is not public.
    pub url: Option<String>,
}

impl Disk {
    /// A private disk at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            url: None,
        }
    }

    /// The same disk, served at `url`.
    pub fn with_url(self, url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            ..self
        }
    }

    /// Where `path` lives on this disk.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when `path` is empty or absolute or has `..`: it could
    /// leave the disk, so it is refused rather than cleaned up.
    pub fn path(&self, path: &str) -> io::Result<PathBuf> {
        let relative = Path::new(path);
        let safe = !path.is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
        if !safe {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("storage path {path:?} is not a relative path inside the disk"),
            ));
        }
        Ok(self.root.join(relative))
    }

    /// Writes `contents` to `path`, creating its folders and replacing any
    /// file there.
    pub fn put(&self, path: &str, contents: impl AsRef<[u8]>) -> io::Result<()> {
        let file = self.path(path)?;
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder)?;
        }
        // ponytail: a crash mid-write leaves a partial file; write to a temp
        // file and rename when an app stores something it cannot re-create
        fs::write(file, contents)
    }

    /// The contents of `path`.
    pub fn get(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(path)?)
    }

    /// Whether `path` is a file on this disk.
    pub fn exists(&self, path: &str) -> bool {
        self.path(path).is_ok_and(|file| file.is_file())
    }

    /// Removes `path`. A file that is already gone is not an error.
    pub fn delete(&self, path: &str) -> io::Result<()> {
        match fs::remove_file(self.path(path)?) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    /// The URL `path` is served at, when the disk is public.
    pub fn url(&self, path: &str) -> Option<String> {
        let base = self.url.as_deref()?;
        Some(format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        ))
    }
}

/// The app's disks. An app builds it in `app/config/filesystems.rs`.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The disk [`Storage`]'s own methods use.
    pub default: String,
    /// Disks by name.
    pub disks: HashMap<String, Disk>,
    /// Links [`Storage::link`] makes, as `(link, target)`, such as
    /// `("public/storage", "storage/app/public")`.
    pub links: Vec<(PathBuf, PathBuf)>,
}

/// The app's disks. It derefs to the default disk, so `storage.put(...)`
/// writes there and `storage.disk("public").put(...)` writes elsewhere.
#[derive(Debug, Clone)]
pub struct Storage {
    settings: Settings,
}

impl Storage {
    /// The disks in `settings`.
    ///
    /// # Panics
    ///
    /// When the default disk is not one of them, naming it: the app should
    /// stop at startup rather than lose writes.
    pub fn new(settings: Settings) -> Self {
        assert!(
            settings.disks.contains_key(&settings.default),
            "config key FILESYSTEM_DISK is {}, which is not a disk",
            settings.default
        );
        Self { settings }
    }

    /// The disk called `name`.
    ///
    /// # Panics
    ///
    /// When there is no such disk: a typo in the app, not a runtime state.
    pub fn disk(&self, name: &str) -> &Disk {
        self.settings.disks.get(name).unwrap_or_else(|| {
            panic!("no storage disk {name}; add it in app/config/filesystems.rs")
        })
    }

    /// Makes each link, like `php artisan storage:link`, so the web serves
    /// the public disk. An existing link to another place is replaced, so
    /// moving the app is enough; anything else in the way is an error.
    pub fn link(&self) -> io::Result<()> {
        for (link, target) in &self.settings.links {
            let target = std::path::absolute(target)?;
            fs::create_dir_all(&target)?;
            if let Ok(meta) = fs::symlink_metadata(link) {
                if !meta.is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} exists and is not a link", link.display()),
                    ));
                }
                if fs::read_link(link)? == target {
                    continue;
                }
                fs::remove_file(link)?;
            }
            if let Some(folder) = link.parent() {
                fs::create_dir_all(folder)?;
            }
            symlink(&target, link)?;
        }
        Ok(())
    }
}

impl Deref for Storage {
    type Target = Disk;

    fn deref(&self) -> &Disk {
        self.disk(&self.settings.default)
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn symlink(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(feature = "web")]
impl crate::web::Request {
    /// The app's disks, added with `Router::state(storage)`.
    ///
    /// # Panics
    ///
    /// When the router has no [`Storage`] state: that is a wiring bug in the app.
    pub fn storage(&self) -> &Storage {
        self.state::<Storage>()
            .expect("no storage: add .state(storage) to the router")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustclamp-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn refuses_paths_that_leave_the_disk() {
        let disk = Disk::new("root");
        for path in ["", "/etc/passwd", "../x", "a/../../x", "a/.."] {
            assert!(disk.path(path).is_err(), "{path} was accepted");
        }
        assert_eq!(disk.path("./a/b.txt").unwrap(), Path::new("root/a/b.txt"));
    }

    #[test]
    fn urls_only_for_public_disks() {
        assert_eq!(Disk::new("x").url("a.png"), None);
        let public = Disk::new("x").with_url("/storage/");
        assert_eq!(public.url("/a.png").as_deref(), Some("/storage/a.png"));
    }

    #[test]
    #[should_panic(expected = "FILESYSTEM_DISK is s3")]
    fn unknown_default_disk_stops_startup() {
        Storage::new(Settings {
            default: "s3".into(),
            disks: HashMap::new(),
            links: Vec::new(),
        });
    }

    #[cfg(unix)]
    #[test]
    fn link_serves_the_public_disk_and_follows_a_move() {
        let dir = temp("link");
        let (link, old, new) = (dir.join("public/storage"), dir.join("old"), dir.join("new"));
        let storage = |target: &Path| {
            Storage::new(Settings {
                default: "public".into(),
                disks: HashMap::from([("public".into(), Disk::new(target))]),
                links: vec![(link.clone(), target.to_owned())],
            })
        };
        storage(&old).link().unwrap();
        storage(&old).link().unwrap();
        storage(&old).put("a.txt", "one").unwrap();
        assert_eq!(fs::read(link.join("a.txt")).unwrap(), b"one");
        storage(&new).link().unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), new);

        let blocked = dir.join("blocked");
        fs::create_dir_all(&blocked).unwrap();
        let in_the_way = Storage::new(Settings {
            default: "public".into(),
            disks: HashMap::from([("public".into(), Disk::new(&old))]),
            links: vec![(blocked, old.clone())],
        });
        assert!(in_the_way.link().is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
