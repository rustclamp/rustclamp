//! `clamp mcp`: the inspection commands as MCP tools over stdio (ADR 0031).
//!
//! Newline-delimited JSON-RPC 2.0 with the `initialize` handshake. Each tool
//! calls the same `inspection` function as the CLI and returns its `--json` value.

use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

/// Revisions this server speaks, newest first. All share the handshake and tool shapes used here.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const TOOLS: [(&str, &str); 5] = [
    (
        "inspect",
        "Resolved process: roots, included modules with reasons, providers, exclusions, contributions.",
    ),
    ("tree", "Inclusion tree of a resolved process."),
    (
        "graph",
        "Edges of a resolved process: inclusion, capability and contribution.",
    ),
    (
        "why",
        "Why a module is included in, or excluded from, a resolved process.",
    ),
    (
        "doctor",
        "Whole document; for an invalid composition, its diagnostics.",
    ),
];

/// Serves requests from `input` until it closes, one JSON-RPC response per line on `output`.
pub fn serve(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = respond(&line) else {
            continue;
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}

/// The response line for one request, or `None` for a notification.
fn respond(line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(failure(
                Value::Null,
                -32700,
                &format!("parse error: {error}"),
            ));
        }
    };
    let id = request.get("id").cloned();
    let Some(method) = request["method"]
        .as_str()
        .filter(|_| request["jsonrpc"] == "2.0")
    else {
        return Some(failure(
            id.unwrap_or(Value::Null),
            -32600,
            "invalid request",
        ));
    };
    // Notifications (`notifications/initialized`, cancellations) need no answer.
    let id = id?;
    let params = &request["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS
                .into_iter()
                .find(|v| *v == asked)
                .unwrap_or(VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "clamp", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            Ok(json!({"tools": TOOLS.map(|(name, description)| tool(name, description))}))
        }
        "tools/call" => call(params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => failure(id, code, &message),
    })
}

fn tool(name: &str, description: &str) -> Value {
    let mut properties = json!({
        "file": {"type": "string", "description": "Path to an inspection document (schema version 1) written by the application."},
        "process": {"type": "string", "description": "Process ID; required when the document has more than one process."},
    });
    let mut required = vec!["file"];
    if name == "why" {
        properties["module"] = json!({"type": "string", "description": "Module ID to explain."});
        required.push("module");
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

fn call(params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    if !TOOLS.iter().any(|(tool, _)| *tool == name) {
        return Err((-32602, format!("unknown tool {name:?}")));
    }
    let arguments = &params["arguments"];
    let file = arguments["file"]
        .as_str()
        .ok_or((-32602, "missing argument file".to_owned()))?;
    // Command errors (unreadable file, unknown process) are tool results the model can read.
    Ok(
        match crate::inspection(
            name,
            file,
            arguments["process"].as_str(),
            arguments["module"].as_str(),
        ) {
            Ok((_, machine, _)) => json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&machine).unwrap()}],
                "isError": false,
            }),
            Err(message) => {
                json!({"content": [{"type": "text", "text": message}], "isError": true})
            }
        },
    )
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustclamp_core::{ApplicationId, CapabilityId, ExecutionId, ModuleId, ProcessId};
    use rustclamp_kernel::ApplicationBlueprint;

    fn session(lines: &[String]) -> Vec<Value> {
        let mut output = Vec::new();
        serve(lines.join("\n").as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn serves_inspection_tools_over_json_rpc() {
        let root = ModuleId::new("app.cli");
        let provider = ModuleId::new("storage.memory");
        let mut blueprint = ApplicationBlueprint::new(ApplicationId::new("sample"));
        blueprint
            .add_module(root)
            .add_module(provider)
            .add_execution(ExecutionId::new("cli"), root)
            .add_process(ProcessId::new("process.cli"), vec![ExecutionId::new("cli")])
            .require_provider(root, CapabilityId::new("storage.database"), None, provider);
        let projection = blueprint.project(ProcessId::new("process.cli")).unwrap();
        let document = rustclamp_tooling::inspection_document(&[&projection]).unwrap();
        let file = std::env::temp_dir().join(format!("clamp-mcp-{}.json", std::process::id()));
        std::fs::write(&file, document.to_string()).unwrap();
        let file = file.to_str().unwrap();

        let responses = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#.into(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.into(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"why","arguments":{"file":file,"module":"storage.memory"}}}).to_string(),
            "{not json".into(),
            r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#.into(),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"inspect","arguments":{"file":file,"process":"nope"}}}).to_string(),
            r#"{"jsonrpc":"2.0","id":6,"method":"initialize","params":{"protocolVersion":"1900-01-01"}}"#.into(),
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#.into(),
        ]);
        std::fs::remove_file(file).unwrap();

        // The notification gets no response; the malformed line does, and the server keeps going.
        assert_eq!(responses.len(), 8);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["inspect", "tree", "graph", "why", "doctor"]
        );
        let result = &responses[2]["result"];
        assert_eq!(result["isError"], false);
        let why: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(
            why["explanation"]
                .as_str()
                .unwrap()
                .contains("provides storage.database to app.cli")
        );
        assert_eq!(responses[3]["error"]["code"], -32700);
        assert_eq!(responses[3]["id"], Value::Null);
        assert_eq!(responses[4]["error"]["code"], -32601);
        assert_eq!(responses[5]["result"]["isError"], true);
        assert_eq!(responses[6]["result"]["protocolVersion"], VERSIONS[0]);
        assert_eq!(
            responses[7],
            json!({"jsonrpc": "2.0", "id": 7, "result": {}})
        );
    }
}
