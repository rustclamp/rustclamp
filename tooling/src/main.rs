//! Command-line interface for resolved Clamp process inspection.

mod envfile;
mod make;
mod mcp;

use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    process::Command,
    process::ExitCode,
};

use rustclamp_tooling::{
    doctor_text, graph_text, inspect_text, tree_text, validate_document, why_text,
};

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return match welcome() {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                eprintln!("clamp: {message}");
                ExitCode::from(2)
            }
        };
    }
    match run(args) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("clamp: {message}\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

fn welcome() -> Result<u8, String> {
    println!(
        r#"
___           _    ___ _
| _ \_  _ ___| |_ / __| |__ _ _ __  _ __
|   / || (_-<|  _| (__| / _` | '  \| '_ \
|_|_\\_,_/__/ \__|\___|_\__,_|_|_|_| .__/
                                     |_|

  1. Create a project
     Templates: Hello World or application
  2. Check this project
  3. Run tests
  4. Build this project
  5. Run this project
  6. Inspect architecture JSON
  7. Show command help
  0. Exit
"#
    );
    print!("Choose an option: ");
    io::stdout()
        .flush()
        .map_err(|error| format!("cannot write menu: {error}"))?;
    let mut choice = String::new();
    io::stdin()
        .read_line(&mut choice)
        .map_err(|error| format!("cannot read menu choice: {error}"))?;
    let choice = choice.trim();
    if choice == "0" || (choice.is_empty() && !io::stdin().is_terminal()) {
        return Ok(0);
    }
    if choice == "1" {
        print!("Project name: ");
        io::stdout()
            .flush()
            .map_err(|error| format!("cannot write prompt: {error}"))?;
        let mut name = String::new();
        io::stdin()
            .read_line(&mut name)
            .map_err(|error| format!("cannot read project name: {error}"))?;
        print!("Template [blank/app/web/tui/package] (blank): ");
        io::stdout()
            .flush()
            .map_err(|error| format!("cannot write template prompt: {error}"))?;
        let mut template = String::new();
        io::stdin()
            .read_line(&mut template)
            .map_err(|error| format!("cannot read template: {error}"))?;
        let template = match template.trim() {
            "" => "blank",
            other => other,
        };
        return run(vec![
            "init".into(),
            name.trim().into(),
            format!("--{template}"),
        ]);
    }
    if choice == "6" {
        print!("Inspection JSON file: ");
        io::stdout()
            .flush()
            .map_err(|error| format!("cannot write prompt: {error}"))?;
        let mut path = String::new();
        io::stdin()
            .read_line(&mut path)
            .map_err(|error| format!("cannot read file path: {error}"))?;
        return run(vec!["inspect".into(), path.trim().into()]);
    }
    let command = match choice {
        "2" => "check",
        "3" => "test",
        "4" => "build",
        "5" => "run",
        "7" => "--help",
        _ => return Err(format!("unknown menu option {choice:?}")),
    };
    run(vec![command.into()])
}

fn run(args: Vec<String>) -> Result<u8, String> {
    let command = args.first().map(String::as_str).ok_or("missing command")?;
    if command == "--version" || command == "-V" {
        println!("clamp {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if command == "--help" || command == "help" {
        println!("{}", usage());
        return Ok(0);
    }
    if command == "mcp" {
        mcp::serve(io::stdin().lock(), io::stdout().lock())
            .map_err(|error| format!("mcp: {error}"))?;
        return Ok(0);
    }
    if matches!(command, "key:generate" | "env:encrypt" | "env:decrypt") {
        let root = env::current_dir().map_err(|error| format!("cannot read folder: {error}"))?;
        let options = envfile::Options::parse(&args[1..])?;
        let message = match command {
            "key:generate" => envfile::key_generate(&root, &options),
            "env:encrypt" => envfile::encrypt(&root, &options),
            _ => envfile::decrypt(&root, &options, std::env::var(envfile::KEY_VARIABLE).ok()),
        }?;
        println!("{message}");
        return Ok(0);
    }
    if let Some(kind) = command.strip_prefix("make:") {
        let root = env::current_dir().map_err(|error| format!("cannot read folder: {error}"))?;
        let path = make::make(&root, kind, args.get(1))?;
        println!("Created {path}");
        return Ok(0);
    }
    if matches!(
        command,
        "migrate" | "migrate:rollback" | "migrate:status" | "db:seed"
    ) {
        // The app runs its own database commands; see `rustclamp::db::command`.
        let status = Command::new("cargo")
            .args(["run", "--quiet", "--", command])
            .status()
            .map_err(|error| format!("cannot start cargo: {error}"))?;
        return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
    }
    if command == "init" {
        let project = args.get(1).ok_or("init requires a project name or path")?;
        let flags: Vec<&str> = args[2..].iter().map(String::as_str).collect();
        let (template, frontend) = match flags.as_slice() {
            [] | ["--blank"] | ["--template", "blank" | "hello-world"] => ("blank", None),
            ["--app"] | ["--template", "app" | "application"] => ("app", None),
            ["--web"] | ["--template", "web"] => ("web", None),
            ["--vue"] | ["--template", "vue"] => ("web", Some("vue")),
            ["--react"] | ["--template", "react"] => ("web", Some("react")),
            ["--tui"] | ["--template", "tui"] => ("tui", None),
            ["--package"] | ["--template", "package"] => ("package", None),
            _ => {
                return Err(
                    "usage: clamp init NAME [--blank|--app|--web|--vue|--react|--tui|--package]"
                        .into(),
                );
            }
        };
        // Written directly rather than with `cargo new`, which would also add the
        // project to any workspace above it; `[workspace]` keeps it standalone.
        let root = std::path::Path::new(project);
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| {
                name.starts_with(|c: char| c.is_ascii_alphabetic())
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .ok_or("project name must start with a letter and use only letters, digits, - or _")?;
        if root.exists() {
            return Err(format!("{} already exists", root.display()));
        }
        fs::create_dir_all(root.join("src"))
            .map_err(|error| format!("cannot create project directory: {error}"))?;
        let features = match template {
            "web" => ", features = [\"web\", \"db\", \"crypto\"]",
            "package" => ", features = [\"web\"]",
            _ => "",
        };
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n\n[dependencies]\nrustclamp = {{ git = \"https://github.com/rustclamp/rustclamp\", branch = \"main\"{features} }}\n"
            ),
        )
        .map_err(|error| format!("cannot write manifest: {error}"))?;
        fs::write(root.join(".gitignore"), "/target\n")
            .map_err(|error| format!("cannot write .gitignore: {error}"))?;
        // ponytail: git is optional; a failed `git init` leaves a working project
        let _ = Command::new("git")
            .args(["init", "--quiet"])
            .arg(root)
            .status();
        if template == "package" {
            // A library: nothing to run, so no `cargo dev` alias or Procfile.
            create_package(root, name)?;
            println!("Created RustClamp package at {}", root.display());
            println!("Next: cd {} && cargo test", root.display());
            return Ok(0);
        }
        fs::create_dir_all(root.join(".cargo"))
            .map_err(|error| format!("cannot create .cargo directory: {error}"))?;
        fs::write(root.join(".cargo/config.toml"), "[alias]\ndev = \"run\"\n")
            .map_err(|error| format!("cannot write Cargo aliases: {error}"))?;
        fs::write(root.join("Procfile.dev"), "app: cargo run\n")
            .map_err(|error| format!("cannot write Procfile.dev: {error}"))?;
        match template {
            "app" => create_application(root)?,
            "web" => create_web(root, name, frontend)?,
            "tui" => create_tui(root)?,
            _ => {
                fs::write(
                    root.join("src/main.rs"),
                    "use rustclamp::prelude::*;\n\nfn main() {\n    Clamp::run(|| println!(\"Hello from Clamp!\"));\n}\n",
                )
                .map_err(|error| format!("cannot write generated entrypoint: {error}"))?;
                fs::write(
                    root.join("README.md"),
                    "# Clamp application\n\nCreated with `clamp init`. Run it with `cargo run`.\n",
                )
                .map_err(|error| format!("cannot write generated README: {error}"))?;
            }
        }
        println!("Created RustClamp {template} project at {}", root.display());
        if template == "web" {
            println!("Next: cd {} && clamp dev", root.display());
        } else {
            println!("Next: cd {} && cargo run", root.display());
        }
        return Ok(0);
    }
    if command == "self-update" || command == "global-update" {
        // Reinstall from the checkout this binary was built from, else the latest release (Unix) or GitHub main.
        let mut source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // tooling depends on ../../core and ../../kernel, so a GitHub install
        // clones all three repos side by side, as install.sh does.
        let work = std::env::temp_dir().join(format!("clamp-update-{}", std::process::id()));
        if source.join("Cargo.toml").exists() {
            println!("Updating clamp from {}", source.display());
        } else if cfg!(unix) {
            // Release binaries: rerun the installer, which fetches the latest release.
            println!("Updating clamp from the latest release");
            let status = Command::new("sh")
                .args([
                    "-c",
                    "curl --proto '=https' --tlsv1.2 -fsSL https://rustclamp.com/install.sh | sh",
                ])
                .status()
                .map_err(|error| format!("cannot start sh: {error}"))?;
            return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
        } else {
            println!("Updating clamp from github.com/rustclamp (main)");
            for repo in ["core", "kernel", "rustclamp"] {
                let status = Command::new("git")
                    .args(["clone", "--quiet", "--depth", "1"])
                    .arg(format!("https://github.com/rustclamp/{repo}"))
                    .arg(work.join(repo))
                    .status()
                    .map_err(|error| format!("cannot start git: {error}"))?;
                if !status.success() {
                    let _ = fs::remove_dir_all(&work);
                    return Err(format!("cannot clone rustclamp/{repo}"));
                }
            }
            source = work.join("rustclamp/tooling");
        }
        let status = Command::new("cargo")
            .args(["install", "--force", "--locked", "--path"])
            .arg(&source)
            .status()
            .map_err(|error| format!("cannot start Cargo: {error}"));
        let _ = fs::remove_dir_all(&work);
        return Ok(status?.code().unwrap_or(1).clamp(0, 255) as u8);
    }
    if command == "dev" {
        return dev();
    }
    if ["check", "test", "build", "run"].contains(&command) {
        let status = Command::new("cargo")
            .arg(command)
            .args(&args[1..])
            .status()
            .map_err(|error| format!("cannot start Cargo: {error}"))?;
        return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
    }
    let (mut positional, mut process_id, mut json_output) = (Vec::new(), None, false);
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json_output = true,
            "--process" => {
                index += 1;
                process_id = Some(args.get(index).ok_or("--process requires an id")?.clone());
            }
            option if option.starts_with('-') => return Err(format!("unknown option {option}")),
            value => positional.push(value.to_owned()),
        }
        index += 1;
    }
    let path = positional.first().ok_or("missing inspection JSON path")?;
    let (human, machine, code) = inspection(
        command,
        path,
        process_id.as_deref(),
        positional.get(1).map(String::as_str),
    )?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&machine).unwrap());
    } else {
        println!("{human}");
    }
    Ok(code)
}

/// Runs one read-only inspection command on a document file and returns its
/// human text, its `--json` value, and the exit status. `clamp mcp` calls this too.
fn inspection(
    command: &str,
    path: &str,
    process_id: Option<&str>,
    module: Option<&str>,
) -> Result<(String, Value, u8), String> {
    let document: Value = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?,
    )
    .map_err(|error| format!("invalid JSON in {path}: {error}"))?;
    validate_document(&document)?;

    if command == "doctor" {
        let code = if document["status"] == "resolved" {
            0
        } else {
            1
        };
        return Ok((doctor_text(&document).0, document, code));
    }
    if document["status"] != "resolved" {
        return Err("document contains composition errors; run `clamp doctor FILE`".into());
    }

    let processes = document["processes"]
        .as_array()
        .ok_or("resolved document has no process list")?;
    let process = match process_id {
        Some(id) => processes
            .iter()
            .find(|process| process["id"].as_str() == Some(id))
            .ok_or_else(|| format!("process {id:?} is not in this document"))?,
        None if processes.len() == 1 => &processes[0],
        None => return Err("select a process with --process ID".into()),
    };

    let (human, machine) = match command {
        "inspect" => (inspect_text(process), json!({"process": process})),
        "tree" => {
            let tree = tree_text(process);
            (tree.clone(), json!({"tree": tree}))
        }
        "graph" => {
            let edges = graph_text(process);
            (
                edges.clone(),
                json!({"edges": edges.lines().collect::<Vec<_>>()}),
            )
        }
        "why" => {
            let module = module.ok_or("why requires a module id")?;
            let explanation = why_text(process, module)?;
            (
                explanation.clone(),
                json!({"module": module, "explanation": explanation}),
            )
        }
        _ => return Err(format!("unknown command {command:?}")),
    };
    Ok((human, machine, 0))
}

fn usage() -> &'static str {
    "Usage:\n  clamp init <project-name> [--blank|--app|--web|--vue|--react|--tui|--package]\n  clamp <inspect|tree|graph|why|doctor> FILE [MODULE] [--process ID] [--json]\n  clamp mcp\n  clamp dev\n  clamp make:migration NAME | make:seeder NAME\n  clamp migrate | migrate:rollback | migrate:status | db:seed\n  clamp key:generate [--force]\n  clamp env:encrypt | env:decrypt [--key=KEY] [--env=NAME] [--force]\n  clamp self-update\n  clamp --version\n  clamp <check|test|build|run> [Cargo arguments]\n\nCreate a RustClamp blank, app, web (plain, Vue or React) or TUI scaffold or a package, inspect a resolved projection (also as an MCP server over stdio), run Procfile.dev concurrently, make migrations and seeders, run database commands, manage APP_KEY and encrypted .env files, reinstall clamp, or run a Cargo command."
}

fn create_application(root: &std::path::Path) -> Result<(), String> {
    fs::write(
        root.join("src/main.rs"),
        "mod app;\nmod modules;\n\nuse rustclamp::prelude::*;\n\nfn main() {\n    Clamp::run(app::run);\n}\n",
    )
    .map_err(|error| format!("cannot write application entrypoint: {error}"))?;
    fs::write(
        root.join("src/app.rs"),
        "use crate::modules::health;\n\npub fn run() {\n    println!(\"Application started\");\n    println!(\"Health check: {}\", health::check());\n}\n",
    )
    .map_err(|error| format!("cannot write application module: {error}"))?;
    fs::create_dir_all(root.join("src/modules"))
        .map_err(|error| format!("cannot create application modules: {error}"))?;
    fs::write(root.join("src/modules/mod.rs"), "pub mod health;\n")
        .map_err(|error| format!("cannot write module list: {error}"))?;
    fs::write(
        root.join("src/modules/health.rs"),
        "pub fn check() -> &'static str {\n    \"ready\"\n}\n",
    )
    .map_err(|error| format!("cannot write health module: {error}"))?;
    fs::write(
        root.join("README.md"),
        "# Clamp application\n\nCreated with `clamp init --app`. The binary entrypoint delegates to `app`, which composes modules under `src/modules`.\n\nRun with `cargo run`; add application behavior in modules and keep `main.rs` as the composition root.\n",
    )
    .map_err(|error| format!("cannot write application README: {error}"))?;
    Ok(())
}

const WEB_TEMPLATE: &[(&str, &str)] = &[
    ("app/main.rs", include_str!("../templates/web/app/main.rs")),
    ("app/lib.rs", include_str!("../templates/web/app/lib.rs")),
    (
        "app/config/app.rs",
        include_str!("../templates/web/app/config/app.rs"),
    ),
    (
        "app/config/database.rs",
        include_str!("../templates/web/app/config/database.rs"),
    ),
    (
        "app/config/filesystems.rs",
        include_str!("../templates/web/app/config/filesystems.rs"),
    ),
    (
        "app/config/logging.rs",
        include_str!("../templates/web/app/config/logging.rs"),
    ),
    (
        "app/http/controllers/controller.rs",
        include_str!("../templates/web/app/http/controllers/controller.rs"),
    ),
    (
        "app/http/middleware/.gitkeep",
        include_str!("../templates/web/app/http/middleware/.gitkeep"),
    ),
    (
        "app/http/requests/.gitkeep",
        include_str!("../templates/web/app/http/requests/.gitkeep"),
    ),
    (
        "app/models/.gitkeep",
        include_str!("../templates/web/app/models/.gitkeep"),
    ),
    (
        "app/routes/web.rs",
        include_str!("../templates/web/app/routes/web.rs"),
    ),
    ("build.rs", include_str!("../templates/web/build.rs")),
    (
        "app/routes/api.rs",
        include_str!("../templates/web/app/routes/api.rs"),
    ),
    (
        "tests/routes.rs",
        include_str!("../templates/web/tests/routes.rs"),
    ),
    (
        "app/resources/views/welcome.html",
        include_str!("../templates/web/app/resources/views/welcome.html"),
    ),
    (
        "app/resources/views/errors/4xx.html",
        include_str!("../templates/web/app/resources/views/errors/4xx.html"),
    ),
    (
        "app/resources/views/errors/5xx.html",
        include_str!("../templates/web/app/resources/views/errors/5xx.html"),
    ),
    (
        "app/resources/css/app.css",
        include_str!("../templates/web/app/resources/css/app.css"),
    ),
    (
        "app/resources/js/app.ts",
        include_str!("../templates/web/app/resources/js/app.ts"),
    ),
    (
        "public/robots.txt",
        include_str!("../templates/web/public/robots.txt"),
    ),
    (
        ".env.example",
        include_str!("../templates/web/.env.example"),
    ),
    (
        "package.json",
        include_str!("../templates/web/package.json"),
    ),
    (
        "package-lock.json",
        include_str!("../templates/web/package-lock.json"),
    ),
    (
        "vite.config.ts",
        include_str!("../templates/web/vite.config.ts"),
    ),
    (
        "Procfile.dev",
        include_str!("../templates/web/Procfile.dev"),
    ),
    ("README.md", include_str!("../templates/web/README.md")),
];

/// Where the welcome page mounts the example component; plain `--web` has none.
const COMPONENT_MOUNT: &str = "        <div class=\"mt-4 flex justify-center\" data-component=\"Counter\" data-props='{\"start\":0}'></div>\n";

/// `--vue`: written over [`WEB_TEMPLATE`].
const VUE_OVERLAY: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("../templates/web-vue/package.json"),
    ),
    (
        "package-lock.json",
        include_str!("../templates/web-vue/package-lock.json"),
    ),
    (
        "vite.config.ts",
        include_str!("../templates/web-vue/vite.config.ts"),
    ),
    (
        "app/resources/js/app.ts",
        include_str!("../templates/web-vue/app/resources/js/app.ts"),
    ),
    (
        "app/resources/js/components/Counter.vue",
        include_str!("../templates/web-vue/app/resources/js/components/Counter.vue"),
    ),
];

/// `--react`: written over [`WEB_TEMPLATE`].
const REACT_OVERLAY: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("../templates/web-react/package.json"),
    ),
    (
        "package-lock.json",
        include_str!("../templates/web-react/package-lock.json"),
    ),
    (
        "vite.config.ts",
        include_str!("../templates/web-react/vite.config.ts"),
    ),
    (
        "app/resources/js/app.ts",
        include_str!("../templates/web-react/app/resources/js/app.ts"),
    ),
    (
        "app/resources/js/components/Counter.tsx",
        include_str!("../templates/web-react/app/resources/js/components/Counter.tsx"),
    ),
];

/// Laravel-style layout: Rust and frontend sources in `app/`, web root in
/// `public/`, tests in `tests/`. `frontend` (`vue` or `react`) adds that
/// framework's components, mounted on server-rendered pages.
fn create_web(root: &std::path::Path, name: &str, frontend: Option<&str>) -> Result<(), String> {
    let crate_name = name.replace('-', "_");
    let (overlay, component): (&[(&str, &str)], &str) = match frontend {
        Some("vue") => (VUE_OVERLAY, COMPONENT_MOUNT),
        Some("react") => (REACT_OVERLAY, COMPONENT_MOUNT),
        _ => (&[], ""),
    };
    // The overlay comes last, so its files replace the plain ones.
    for (path, contents) in WEB_TEMPLATE.iter().chain(overlay) {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let contents = contents
            .replace("__CRATE__", &crate_name)
            .replace("        __COMPONENT__\n", component);
        fs::write(&file, contents).map_err(|error| format!("cannot write {path}: {error}"))?;
    }
    let _ = fs::remove_dir(root.join("src"));
    let manifest = root.join("Cargo.toml");
    let mut cargo_toml =
        fs::read_to_string(&manifest).map_err(|error| format!("cannot read manifest: {error}"))?;
    cargo_toml.push_str(&format!(
        "\n[build-dependencies]\nrustclamp = {{ git = \"https://github.com/rustclamp/rustclamp\", branch = \"main\", features = [\"build\"] }}\n\n[lib]\npath = \"app/lib.rs\"\n\n[[bin]]\nname = \"{name}\"\npath = \"app/main.rs\"\n"
    ));
    fs::write(&manifest, cargo_toml).map_err(|error| format!("cannot write manifest: {error}"))?;
    let gitignore = root.join(".gitignore");
    let mut ignored = fs::read_to_string(&gitignore).unwrap_or_default();
    // Every .env is secret except the example and the encrypted ones.
    ignored.push_str("/node_modules\n/public/build\n/public/storage\n/storage\n.env\n.env.*\n!.env.example\n!.env.encrypted\n!.env.*.encrypted\n");
    fs::write(&gitignore, ignored).map_err(|error| format!("cannot update .gitignore: {error}"))?;
    Ok(())
}

const PACKAGE_TEMPLATE: &[(&str, &str)] = &[
    (
        "src/lib.rs",
        include_str!("../templates/package/src/lib.rs"),
    ),
    (
        "resources/views/index.html",
        include_str!("../templates/package/resources/views/index.html"),
    ),
    (
        "resources/css/__NAME__.css",
        include_str!("../templates/package/resources/css/__NAME__.css"),
    ),
    (
        "tests/routes.rs",
        include_str!("../templates/package/tests/routes.rs"),
    ),
    ("README.md", include_str!("../templates/package/README.md")),
];

/// A self-contained package crate: routes, views, static files and tests.
fn create_package(root: &std::path::Path, name: &str) -> Result<(), String> {
    let crate_name = name.replace('-', "_");
    let struct_name: String = name
        .split(['-', '_'])
        .flat_map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase())
                .into_iter()
                .chain(chars)
        })
        .collect();
    let fill = |text: &str| {
        text.replace("__CRATE__", &crate_name)
            .replace("__STRUCT__", &struct_name)
            .replace("__ENV__", &crate_name.to_ascii_uppercase())
            .replace("__NAME__", name)
    };
    for (path, contents) in PACKAGE_TEMPLATE {
        let file = root.join(fill(path));
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        fs::write(&file, fill(contents))
            .map_err(|error| format!("cannot write {path}: {error}"))?;
    }
    Ok(())
}

fn create_tui(root: &std::path::Path) -> Result<(), String> {
    fs::write(
        root.join("src/main.rs"),
        r#"use std::io::{self, BufRead, Write};

use rustclamp::prelude::*;

fn main() {
    Clamp::run(|| {
        println!("Clamp TUI. Type a line, or `quit` to exit.");
        let mut stdout = io::stdout();
        let _ = write!(stdout, "> ");
        let _ = stdout.flush();
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if line.trim() == "quit" {
                break;
            }
            println!("you said: {line}");
            let _ = write!(stdout, "> ");
            let _ = stdout.flush();
        }
    });
}
"#,
    )
    .map_err(|error| format!("cannot write TUI entrypoint: {error}"))?;
    fs::write(
        root.join("README.md"),
        "# Clamp TUI\n\nCreated with `clamp init --tui`. Run it with `cargo run`; type `quit` to exit.\n",
    )
    .map_err(|error| format!("cannot write TUI README: {error}"))?;
    Ok(())
}

/// Runs every `name: command` line of `Procfile.dev` at once, prefixing output
/// with its name. A process that fails stops the rest.
fn dev() -> Result<u8, String> {
    // ponytail: no restart-on-crash or file watching; put `watchexec -r -- cargo run` in the Procfile for reload
    if std::path::Path::new("package.json").exists()
        && !std::path::Path::new("node_modules").exists()
    {
        println!("[clamp] installing frontend dependencies: npm install");
        let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
            .arg("install")
            .status()
            .map_err(|error| format!("cannot start npm (is Node.js installed?): {error}"))?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
    }
    let procfile = fs::read_to_string("Procfile.dev").unwrap_or_else(|_| "app: cargo run\n".into());
    let mut children = Vec::new();
    for line in procfile.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, command) = line
            .split_once(':')
            .ok_or_else(|| format!("Procfile.dev line needs `name: command`: {line:?}"))?;
        let (name, command) = (name.trim().to_string(), command.trim());
        let mut shell = if cfg!(windows) {
            let mut shell = Command::new("cmd");
            shell.arg("/C");
            shell
        } else {
            let mut shell = Command::new("sh");
            shell.arg("-c");
            shell
        };
        let mut child = shell
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot start {name}: {error}"))?;
        for pipe in [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn io::Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let name = name.clone();
            std::thread::spawn(move || {
                for line in io::BufRead::lines(io::BufReader::new(pipe)).map_while(Result::ok) {
                    println!("[{name}] {line}");
                }
            });
        }
        println!("[clamp] started {name}: {command}");
        children.push((name, child));
    }
    if children.is_empty() {
        return Err("Procfile.dev has no commands".into());
    }
    while !children.is_empty() {
        let mut index = 0;
        while index < children.len() {
            let Some(status) = children[index]
                .1
                .try_wait()
                .map_err(|error| error.to_string())?
            else {
                index += 1;
                continue;
            };
            let (name, _) = children.remove(index);
            if status.success() {
                println!("[clamp] {name} finished");
                continue;
            }
            println!("[clamp] {name} failed ({status}); stopping the rest");
            for (_, child) in &mut children {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    /// The welcome page quotes `app/routes/api.rs`; a stale copy sends readers
    /// to code the project does not have (#88).
    #[test]
    fn welcome_page_quotes_the_real_api_routes() {
        let page = include_str!("../templates/web/app/resources/views/welcome.html");
        let panel = page
            .split_once("app/routes/api.rs</p>")
            .and_then(|(_, rest)| rest.split_once("</pre>"))
            .map(|(panel, _)| panel)
            .expect("welcome page has an api.rs panel");
        let mut code = String::new();
        let mut in_tag = false;
        for c in panel.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                c if !in_tag => code.push(c),
                _ => {}
            }
        }
        let code = code
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        assert_eq!(
            code.trim(),
            include_str!("../templates/web/app/routes/api.rs").trim()
        );
    }
}
