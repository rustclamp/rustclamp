//! `clamp make:migration` and `clamp make:seeder`: new files from stubs in a
//! `clamp init --web` project, which `build.rs` then picks up.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Writes the file for `kind` (`migration` or `seeder`) named `name` under
/// `root`, and returns its path.
pub fn make(root: &Path, kind: &str, name: Option<&String>) -> Result<String, String> {
    let name = name.ok_or(format!("usage: clamp make:{kind} NAME"))?;
    if name.is_empty()
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "{name}: use snake_case, such as create_posts or posts"
        ));
    }
    if !root.join("app/lib.rs").exists() {
        return Err("run it in a project made with `clamp init NAME --web`".into());
    }
    let (folder, file, source) = match kind {
        "migration" => {
            let stem = format!(
                "{}_{name}",
                next_stamp(&root.join("app/database/migrations"))
            );
            let file = format!("{stem}.rs");
            ("app/database/migrations", file, migration(name))
        }
        "seeder" => {
            let stem = if name.ends_with("_seeder") {
                name.clone()
            } else {
                format!("{name}_seeder")
            };
            let source = seeder(&stem);
            ("app/database/seeders", format!("{stem}.rs"), source)
        }
        _ => {
            return Err(format!(
                "unknown make:{kind}; try make:migration or make:seeder"
            ));
        }
    };
    let folder = root.join(folder);
    fs::create_dir_all(&folder)
        .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
    let path = folder.join(&file);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    fs::write(&path, source)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path.display().to_string())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// A stamp for now, or one second after the newest existing migration, so a
/// new migration always sorts last, even when two are made in one second.
fn next_stamp(folder: &Path) -> String {
    let newest = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.get(..17))
                .and_then(seconds_of)
        })
        .max();
    stamp(newest.map_or(now(), |newest| now().max(newest + 1)))
}

/// The seconds a `YYYY_MM_DD_HHMMSS` stamp stands for, if it is one.
fn seconds_of(stamp: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(13..15)?, number(15..17)?);
    // Civil date to days since 1970-01-01 (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

/// `YYYY_MM_DD_HHMMSS` in UTC, the prefix that orders migrations.
fn stamp(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}_{month:02}_{day:02}_{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// `create_posts` → `CreatePosts`, as `rustclamp::build` expects.
fn struct_name(stem: &str) -> String {
    stem.split('_')
        .skip_while(|part| part.chars().all(|c| c.is_ascii_digit()))
        .filter(|part| !part.is_empty())
        .map(|part| part[..1].to_ascii_uppercase() + &part[1..])
        .collect()
}

/// A `create_{table}` migration creates the table; `add_{column}_to_{table}`
/// adds a column; any other name gets an alter stub to fill in.
fn migration(name: &str) -> String {
    let name_struct = struct_name(name);
    let head = format!(
        "use rustclamp::db::{{Migration, Schema, migration_name}};\n\npub struct {name_struct};\n\nimpl Migration for {name_struct} {{\n    fn name(&self) -> &'static str {{\n        migration_name(file!())\n    }}\n\n"
    );
    let body = match name.strip_prefix("create_") {
        Some(table) => {
            let table = table.strip_suffix("_table").unwrap_or(table);
            format!(
                "    fn up(&self) -> String {{\n        Schema::create(\"{table}\", |table| {{\n            table.id();\n            table.timestamps();\n        }})\n    }}\n\n    fn down(&self) -> String {{\n        Schema::drop(\"{table}\")\n    }}\n}}\n"
            )
        }
        None => {
            let (column, table) = name
                .strip_prefix("add_")
                .and_then(|rest| rest.rsplit_once("_to_"))
                .unwrap_or(("COLUMN", "TABLE"));
            format!(
                "    fn up(&self) -> String {{\n        Schema::table(\"{table}\", |table| {{\n            table.string(\"{column}\").nullable();\n        }})\n    }}\n\n    fn down(&self) -> String {{\n        \"ALTER TABLE {table} DROP COLUMN {column};\".into()\n    }}\n}}\n"
            )
        }
    };
    head + &body
}

/// A seeder that does nothing until filled in; `posts_seeder` suggests the
/// `posts` table.
fn seeder(stem: &str) -> String {
    let name_struct = struct_name(stem);
    let table = stem.strip_suffix("_seeder").unwrap_or(stem);
    format!(
        "use rustclamp::db::{{Db, SeedError, Seeder}};\n\npub struct {name_struct};\n\nimpl Seeder for {name_struct} {{\n    fn run(&self, db: &Db) -> Result<(), SeedError> {{\n        // db.table(\"{table}\")\n        //     .insert(&[\"title\"], rustclamp::db::sqlite::params![\"Hello\"])?;\n        let _ = db;\n        Ok(())\n    }}\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(stamp(0), "1970_01_01_000000");
        assert_eq!(stamp(1_790_700_000), "2026_09_29_164000");
        assert_eq!(stamp(951_782_400), "2000_02_29_000000", "leap day");
        for seconds in [0, 951_782_400, 1_790_700_000, 4_102_444_799] {
            assert_eq!(seconds_of(&stamp(seconds)), Some(seconds));
        }
        assert_eq!(seconds_of("notes"), None);
    }

    #[test]
    fn stubs_name_their_struct_like_build_rs() {
        assert!(migration("create_posts").contains("pub struct CreatePosts;"));
        assert!(migration("create_posts_table").contains("Schema::create(\"posts\""));
        let add = migration("add_slug_to_posts");
        assert!(add.contains("Schema::table(\"posts\"") && add.contains("table.string(\"slug\")"));
        assert!(add.contains("DROP COLUMN slug"));
        assert!(migration("tweak_things").contains("Schema::table(\"TABLE\""));
        let posts = seeder("posts_seeder");
        assert!(posts.contains("pub struct PostsSeeder;") && posts.contains("db.table(\"posts\")"));
    }

    #[test]
    fn writes_into_a_web_project_only() {
        let root = std::env::temp_dir().join(format!("clamp-make-{}", std::process::id()));
        fs::create_dir_all(root.join("app")).unwrap();
        let name = "create_posts".to_owned();
        assert!(
            make(&root, "migration", Some(&name)).is_err(),
            "no app/lib.rs"
        );
        fs::write(root.join("app/lib.rs"), "").unwrap();
        let path = make(&root, "migration", Some(&name)).unwrap();
        assert!(path.ends_with("_create_posts.rs"));
        let add = "add_slug_to_posts".to_owned();
        let second = make(&root, "migration", Some(&add)).unwrap();
        assert!(second > path, "made in the same second, still sorts after");
        let posts = "posts".to_owned();
        let seeder = make(&root, "seeder", Some(&posts)).unwrap();
        assert!(seeder.ends_with("app/database/seeders/posts_seeder.rs"));
        assert!(
            make(&root, "seeder", Some(&posts)).is_err(),
            "never overwrites"
        );
        let bad = "Create-Posts".to_owned();
        assert!(make(&root, "migration", Some(&bad)).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
