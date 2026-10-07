//! `clamp make:*`: new files from stubs in a `clamp init --web` project.
//! Migrations and seeders are picked up by `build.rs`; controllers,
//! middleware, form requests and models are added to the module map in
//! `app/lib.rs`.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Words a module or function cannot be named.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Writes the file for `kind` named `name` under `root`, adds it to
/// `app/lib.rs` when it is a module, and says what it did.
pub fn make(root: &Path, kind: &str, name: Option<&String>) -> Result<String, String> {
    let given = name.ok_or(format!("usage: clamp make:{kind} NAME"))?;
    let name = &snake(given).ok_or(format!(
        "{given}: use a Rust name, such as create_posts, posts or PostController"
    ))?;
    if !root.join("app/lib.rs").exists() {
        return Err("run it in a project made with `clamp init NAME --web`".into());
    }
    let title = struct_name(name);
    // (folder, file, stub, the `pub mod` block in lib.rs and its lines, what to do next)
    let (folder, file, source, module, next): (_, _, _, Option<(&str, Vec<String>)>, String) =
        match kind {
            "migration" => {
                let stem = format!(
                    "{}_{name}",
                    next_stamp(&root.join("app/database/migrations"))
                );
                let file = format!("{stem}.rs");
                let source = migration(name);
                ("app/database/migrations", file, source, None, String::new())
            }
            "seeder" => {
                let stem = if name.ends_with("_seeder") {
                    name.clone()
                } else {
                    format!("{name}_seeder")
                };
                let source = seeder(&stem);
                let file = format!("{stem}.rs");
                ("app/database/seeders", file, source, None, String::new())
            }
            "controller" => (
                "app/http/controllers",
                format!("{name}.rs"),
                controller(name),
                Some(("controllers", vec![format!("pub mod {name};")])),
                format!(
                    "Route it in app/routes/web.rs: .get(\"/{name}\", crate::http::controllers::{name}::index)"
                ),
            ),
            "middleware" => (
                "app/http/middleware",
                format!("{name}.rs"),
                middleware(name),
                Some((
                    "middleware",
                    vec![format!("mod {name};"), format!("pub use {name}::{name};")],
                )),
                format!(
                    "Add it in app/http/kernel.rs: .middleware(crate::http::middleware::{name})"
                ),
            ),
            "request" => {
                let title = match name.ends_with("_request") {
                    true => title,
                    false => format!("{title}Request"),
                };
                (
                    "app/http/requests",
                    format!("{name}.rs"),
                    request(&title),
                    Some((
                        "requests",
                        vec![format!("mod {name};"), format!("pub use {name}::{title};")],
                    )),
                    format!(
                        "Validate in a controller: crate::http::requests::{title}::validate(request)"
                    ),
                )
            }
            "model" => (
                "app/models",
                format!("{name}.rs"),
                model(&title, name),
                Some((
                    "models",
                    vec![format!("mod {name};"), format!("pub use {name}::{title};")],
                )),
                format!(
                    "Check the table name in app/models/{name}.rs: #[model(table = \"{}\")]",
                    plural(name)
                ),
            ),
            _ => {
                return Err(format!(
                    "unknown make:{kind}; try make:migration, make:seeder, make:controller, make:middleware, make:request or make:model"
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
    let mut message = format!("Created {}", path.display());
    if let Some((module, lines)) = module {
        let lib = root.join("app/lib.rs");
        let source = fs::read_to_string(&lib).unwrap_or_default();
        match register(&source, module, &lines) {
            Some(source) => {
                fs::write(&lib, source)
                    .map_err(|error| format!("cannot write {}: {error}", lib.display()))?;
                message += &format!("\nAdded {} to app/lib.rs", lines.join(" "));
            }
            None => {
                message += &format!(
                    "\nAdd {} inside `pub mod {module}` in app/lib.rs",
                    lines.join(" ")
                );
            }
        }
    }
    if !next.is_empty() {
        message += &format!("\n{next}");
    }
    Ok(message)
}

/// `PostController` or `post_controller` → `post_controller`; `None` unless
/// it is an ASCII name that is not a Rust keyword.
fn snake(name: &str) -> Option<String> {
    let mut snake = String::new();
    let mut previous = '_';
    for c in name.chars() {
        if c.is_ascii_uppercase() && (previous.is_ascii_lowercase() || previous.is_ascii_digit()) {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
        previous = c;
    }
    let valid = snake.starts_with(|c: char| c.is_ascii_lowercase())
        && snake
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.contains(&snake.as_str());
    valid.then_some(snake)
}

/// `lib` with `lines` added at the end of the `pub mod {module} { ... }` that
/// starts a line, or `None` when there is no such block.
fn register(lib: &str, module: &str, lines: &[String]) -> Option<String> {
    let open = format!("pub mod {module} {{");
    let (at, indent) = lib.match_indices(&open).find_map(|(at, _)| {
        let start = lib[..at].rfind('\n').map_or(0, |newline| newline + 1);
        let indent = &lib[start..at];
        indent.trim().is_empty().then_some((at, indent))
    })?;
    let body: String = lines
        .iter()
        .map(|line| format!("{indent}    {line}\n"))
        .collect();
    let after = at + open.len();
    Some(if lib[after..].starts_with('}') {
        // `pub mod models {}` → a block with the new lines.
        format!("{}\n{body}{indent}{}", &lib[..after], &lib[after..])
    } else {
        // The block's own `}` is the first one at its indent; nested ones are deeper.
        let end = after + lib[after..].find(&format!("\n{indent}}}"))? + 1;
        format!("{}{body}{}", &lib[..end], &lib[end..])
    })
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

/// A controller with one action; more are more functions.
fn controller(name: &str) -> String {
    format!(
        "use super::controller::*;\n\n/// `GET /{name}`\npub fn index(_request: &Request) -> Response {{\n    Response::text(200, \"{name}\")\n}}\n"
    )
}

/// Middleware that lets every request through until filled in.
fn middleware(name: &str) -> String {
    format!(
        "use rustclamp::web::{{Next, Request, Response}};\n\n/// Runs around the routes it wraps; add it in `app/http/kernel.rs`. To stop\n/// a request, return a response without calling `next`, such as\n/// `rustclamp::web::error(403)`.\npub fn {name}(request: &Request, next: Next) -> Response {{\n    next(request)\n}}\n"
    )
}

/// Typed form input, checked with `Request::validate` and its rules.
fn request(title: &str) -> String {
    format!(
        "use rustclamp::web::{{Invalid, Request}};\n\n/// The input a form sends, trimmed and valid.\n#[derive(Debug)]\npub struct {title} {{\n    pub name: String,\n}}\n\nimpl {title} {{\n    /// The input, or `Invalid`, whose `.back(request, \"/form\")` redirects\n    /// with the errors and input kept. Rules: `required`, `min:N`, `max:N`,\n    /// `email`, `integer`, `in:a,b`, `confirmed`, `unique:table,column`, ...\n    pub fn validate(request: &Request) -> Result<Self, Invalid> {{\n        let form = request.validate(&[(\"name\", \"required|max:255\")])?;\n        Ok(Self {{\n            name: form.get(\"name\").to_owned(),\n        }})\n    }}\n}}\n"
    )
}

/// The English plural of a snake_case name, for its table: `category` →
/// `categories`, `box` → `boxes`, `post` → `posts`.
fn plural(name: &str) -> String {
    // ponytail: regular rules only (`person` → `persons`); the make output says to check it.
    if let Some(stem) = name.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        return format!("{stem}ies");
    }
    if name.ends_with(['s', 'x', 'z']) || name.ends_with("ch") || name.ends_with("sh") {
        return format!("{name}es");
    }
    format!("{name}s")
}

/// A struct read from a table row by column name.
fn model(title: &str, name: &str) -> String {
    let table = plural(name);
    format!(
        "use rustclamp::db::Model;\n\n/// A row of `{table}`, one field per column: `{title}::all(request.db())`,\n/// `{title}::find(request.db(), id)`, `{title}::query(request.db())`.\n#[derive(Debug, Model)]\n#[model(table = \"{table}\")]\npub struct {title} {{\n    pub id: i64,\n}}\n"
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

    #[test]
    fn names_become_snake_case_modules() {
        assert_eq!(snake("PostController").as_deref(), Some("post_controller"));
        assert_eq!(snake("Post2Tag").as_deref(), Some("post2_tag"));
        assert_eq!(snake("posts").as_deref(), Some("posts"));
        for bad in ["", "Create-Posts", "9lives", "_x", "type", "Self", "naïve"] {
            assert_eq!(snake(bad), None, "{bad}");
        }
    }

    #[test]
    fn registers_modules_in_the_map() {
        let lib = include_str!("../templates/web/app/lib.rs");
        let lib = register(lib, "controllers", &["pub mod posts;".into()]).unwrap();
        assert!(lib.contains("        pub mod home;\n        pub mod posts;\n    }"));
        let lines = ["mod post;".into(), "pub use post::Post;".into()];
        let lib = register(&lib, "models", &lines).unwrap();
        assert!(lib.contains("pub mod models {\n    mod post;\n    pub use post::Post;\n}"));
        let lib = register(&lib, "models", &["mod tag;".into()]).unwrap();
        assert!(lib.contains("    pub use post::Post;\n    mod tag;\n}"));
        assert_eq!(register("// pub mod models {}\n", "models", &[]), None);
    }

    #[test]
    fn http_files_go_into_the_map() {
        let root = std::env::temp_dir().join(format!("clamp-make-http-{}", std::process::id()));
        fs::create_dir_all(root.join("app")).unwrap();
        let lib = root.join("app/lib.rs");
        fs::write(&lib, include_str!("../templates/web/app/lib.rs")).unwrap();
        let make = |kind: &str, name: &str| make(&root, kind, Some(&name.to_owned()));
        let made = make("controller", "PostController").unwrap();
        assert!(
            made.contains("app/http/controllers/post_controller.rs"),
            "{made}"
        );
        assert!(
            made.contains("controllers::post_controller::index"),
            "{made}"
        );
        assert!(
            make("controller", "post_controller").is_err(),
            "never overwrites"
        );
        make("middleware", "EnsureAdmin").unwrap();
        make("request", "contact").unwrap();
        make("model", "Post").unwrap();
        assert!(make("widget", "thing").is_err());
        assert!(make("model", "fn").is_err());
        let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
        let map = read("app/lib.rs");
        for line in [
            "pub mod post_controller;",
            "mod ensure_admin;\n        pub use ensure_admin::ensure_admin;",
            "mod contact;\n        pub use contact::ContactRequest;",
            "pub mod models {\n    mod post;\n    pub use post::Post;\n}",
        ] {
            assert!(map.contains(line), "{line} in\n{map}");
        }
        assert!(read("app/http/middleware/ensure_admin.rs").contains("pub fn ensure_admin("));
        assert!(read("app/http/requests/contact.rs").contains("pub struct ContactRequest {"));
        let model = read("app/models/post.rs");
        assert!(model.contains("#[model(table = \"posts\")]\npub struct Post {"));
        for (name, table) in [
            ("category", "categories"),
            ("day", "days"),
            ("box", "boxes"),
            ("match", "matches"),
            ("blog_post", "blog_posts"),
        ] {
            assert_eq!(plural(name), table);
        }
        // No block to add to: the file is still made, with the lines to add.
        fs::write(&lib, "").unwrap();
        let made = make("model", "tag").unwrap();
        assert!(made.contains("Add mod tag; pub use tag::Tag; inside `pub mod models`"));
        fs::remove_dir_all(root).unwrap();
    }
}
