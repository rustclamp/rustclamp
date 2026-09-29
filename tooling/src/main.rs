//! Command-line interface for resolved Clamp process inspection.

use serde_json::{Value, json};
use std::{env, fs, process::ExitCode};

use rustclamp_tooling::{
    doctor_text, graph_text, inspect_text, tree_text, validate_document, why_text,
};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("clamp: {message}\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<u8, String> {
    let command = args.first().map(String::as_str).ok_or("missing command")?;
    if command == "--help" || command == "help" {
        println!("{}", usage());
        return Ok(0);
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
    "Usage: clamp <inspect|tree|graph|why|doctor> FILE [MODULE] [--process ID] [--json]\n\nInspect a versioned resolved process projection exported by an application."
}
