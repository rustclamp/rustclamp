//! Command-line interface for resolved Clamp process inspection.

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
        "\n  ╭──────────────────────────────╮\n  │       Welcome to Clamp       │\n  │        RustClamp tools       │\n  ╰──────────────────────────────╯\n\n  1. Create a project\n  2. Check this project\n  3. Run tests\n  4. Build this project\n  5. Run this project\n  6. Inspect architecture JSON\n  7. Show command help\n  0. Exit\n"
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
        return run(vec!["init".into(), name.trim().into()]);
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
    if command == "--help" || command == "help" {
        println!("{}", usage());
        return Ok(0);
    }
    if command == "init" {
        let project = args.get(1).ok_or("init requires a project name or path")?;
        if args.len() != 2 {
            return Err("init accepts one project name or path".into());
        }
        let status = Command::new("cargo")
            .args(["new", "--bin", project])
            .status()
            .map_err(|error| format!("cannot start Cargo: {error}"))?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
        let root = std::path::Path::new(project);
        let manifest = root.join("Cargo.toml");
        let mut cargo_toml = fs::read_to_string(&manifest)
            .map_err(|error| format!("cannot read generated manifest: {error}"))?;
        cargo_toml.push_str(
            "rustclamp = { git = \"https://github.com/rustclamp/rustclamp\", branch = \"main\" }\n",
        );
        fs::write(&manifest, cargo_toml)
            .map_err(|error| format!("cannot update generated manifest: {error}"))?;
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
        println!("Created RustClamp application at {}", root.display());
        println!("Next: cd {} && cargo run", root.display());
        return Ok(0);
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
    let document: Value = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?,
    )
    .map_err(|error| format!("invalid JSON in {path}: {error}"))?;
    validate_document(&document)?;

    if command == "doctor" {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&document).unwrap());
        } else {
            println!("{}", doctor_text(&document).0);
        }
        return Ok(if document["status"] == "resolved" {
            0
        } else {
            1
        });
    }
    if document["status"] != "resolved" {
        return Err("document contains composition errors; run `clamp doctor FILE`".into());
    }

    let processes = document["processes"]
        .as_array()
        .ok_or("resolved document has no process list")?;
    let process = match process_id.as_deref() {
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
            let module = positional.get(1).ok_or("why requires a module id")?;
            let explanation = why_text(process, module)?;
            (
                explanation.clone(),
                json!({"module": module, "explanation": explanation}),
            )
        }
        _ => return Err(format!("unknown command {command:?}")),
    };
    if json_output {
        println!("{}", serde_json::to_string_pretty(&machine).unwrap());
    } else {
        println!("{human}");
    }
    Ok(0)
}

fn usage() -> &'static str {
    "Usage:\n  clamp init <project-name>\n  clamp <inspect|tree|graph|why|doctor> FILE [MODULE] [--process ID] [--json]\n  clamp <check|test|build|run> [Cargo arguments]\n\nCreate a Clamp application, inspect a resolved projection, or run a Cargo command."
}
