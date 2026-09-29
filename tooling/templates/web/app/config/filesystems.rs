//! Disks, read once at startup from `.env` and the environment.
//! `.env.example` lists every key.

use std::collections::HashMap;

use rustclamp::config::Config;
use rustclamp::storage::{Disk, Settings};

/// Where the app keeps files. Change the default in `.env`; add disks here.
pub fn filesystems(config: &Config) -> Settings {
    Settings {
        // `FILESYSTEM_DISK`: the disk `request.storage().put(...)` writes to.
        default: config.get("FILESYSTEM_DISK").unwrap_or("local").into(),
        disks: HashMap::from([
            // The app's own files, never served.
            ("local".into(), Disk::new("storage/app/private")),
            // Files anyone may download, served at /storage.
            (
                "public".into(),
                Disk::new("storage/app/public").with_url("/storage"),
            ),
        ]),
        // Made at startup, like `php artisan storage:link`.
        links: vec![("public/storage".into(), "storage/app/public".into())],
    }
}
