//! Helpers for an app's `build.rs`. Enabled by the `build` feature, used as a
//! build dependency; std only.
//!
//! In `build.rs`, `fn main` calls:
//!
//! ```no_run
//! rustclamp::build::migrations("app/database/migrations");
//! ```
//!
//! ```ignore
//! // app/lib.rs
//! pub mod database {
//!     include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
//! }
//! // database::migrations() now lists every file in the folder;
//! // rustclamp::build::seeders works the same for seeders.rs.
//! ```

use std::fs;
use std::path::Path;

/// Writes `$OUT_DIR/migrations.rs`: a module for every `.rs` file in
/// `folder`, sorted by file name, and `migrations()` returning them in that
/// order, like Laravel's `database/migrations/`. A file named
/// `2026_09_29_000001_create_posts.rs` must define `pub struct CreatePosts`
/// implementing `Migration`: the name without its leading numbers, in
/// `UpperCamelCase`. A missing folder means no migrations.
///
/// # Panics
///
/// When `OUT_DIR` is unset (not run from a build script), a file name is not
/// letters, digits and `_`, or the output cannot be written.
pub fn migrations(folder: &str) {
    write(folder, "migrations", "Migration");
}

/// Writes `$OUT_DIR/seeders.rs` the same way, with `seeders()`: a file named
/// `posts_seeder.rs` defines `pub struct PostsSeeder` implementing `Seeder`.
///
/// # Panics
///
/// As [`migrations`].
pub fn seeders(folder: &str) {
    write(folder, "seeders", "Seeder");
}

fn write(folder: &str, function: &str, kind: &str) {
    println!("cargo:rerun-if-changed={folder}");
    let out = std::env::var("OUT_DIR").expect("run from build.rs, where OUT_DIR is set");
    let source = source(Path::new(folder), function, kind);
    fs::write(Path::new(&out).join(format!("{function}.rs")), source)
        .unwrap_or_else(|error| panic!("cannot write {function}.rs to OUT_DIR: {error}"));
}

fn source(folder: &Path, function: &str, kind: &str) -> String {
    let mut stems: Vec<String> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
                .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
                .collect()
        })
        .unwrap_or_default();
    stems.sort();
    let folder = fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let mut modules = String::new();
    let mut list = String::new();
    for stem in &stems {
        assert!(
            stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "{kind} file {stem}.rs: use only letters, digits and _"
        );
        let path = folder.join(format!("{stem}.rs"));
        modules.push_str(&format!(
            "#[path = {path:?}]\nmod {function}_{stem};\n",
            path = path.display().to_string()
        ));
        list.push_str(&format!(
            "        &{function}_{stem}::{},\n",
            struct_name(stem)
        ));
    }
    format!(
        "{modules}\n/// Every {kind} in its folder, in file name order.\n\
         pub fn {function}() -> Vec<&'static dyn ::rustclamp::db::{kind}> {{\n    vec![\n{list}    ]\n}}\n"
    )
}

/// `2026_09_29_000001_create_posts` → `CreatePosts`.
fn struct_name(stem: &str) -> String {
    stem.split('_')
        .skip_while(|part| part.chars().all(|c| c.is_ascii_digit()))
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_names_drop_the_date() {
        assert_eq!(struct_name("2026_09_29_000001_create_posts"), "CreatePosts");
        assert_eq!(struct_name("0002_add_tags_to_posts"), "AddTagsToPosts");
    }

    #[test]
    fn lists_rust_files_sorted() {
        let folder =
            std::env::temp_dir().join(format!("rustclamp-migrations-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        for file in ["0002_create_tags.rs", "0001_create_posts.rs", "notes.md"] {
            fs::write(folder.join(file), "").unwrap();
        }
        let source = source(&folder, "migrations", "Migration");
        let posts = source
            .find("&migrations_0001_create_posts::CreatePosts")
            .unwrap();
        let tags = source
            .find("&migrations_0002_create_tags::CreateTags")
            .unwrap();
        assert!(posts < tags);
        assert!(!source.contains("notes"));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn missing_folder_means_no_migrations() {
        let source = source(Path::new("no/such/folder"), "seeders", "Seeder");
        assert!(source.contains("vec![\n    ]"));
    }
}
