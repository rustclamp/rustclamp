# ADR 0031: MCP server for inspection

Status: Accepted, 2026-10-07.

## Context

Stage 8 (AI and MCP) of the roadmap asks for adapters over inspection that are never the source of truth and never needed to build or run an app. `clamp inspect`, `tree`, `graph`, `why` and `doctor` already explain a resolved process from a schema-v1 document. An AI client can run those commands through a shell, but then it parses help text and exit codes. MCP gives it typed tools instead.

## Decision

- **`clamp mcp`** is an MCP server over stdio: newline-delimited JSON-RPC 2.0, hand-written on `serde_json`, which the tooling already depends on. No MCP SDK crate.
- **Protocol:** the `initialize` handshake revisions, newest `2025-11-25`. If the client asks for `2025-06-18`, `2025-03-26` or `2024-11-05`, the server answers with that version; for anything else it answers `2025-11-25`. The 2026-07-28 revision drops the handshake in favour of per-request `_meta` and `server/discover`. This server doesn't implement it: `server/discover` gets -32601, which dual-era clients read as "legacy server" and fall back to `initialize`.
- **Methods:** `initialize`, `ping`, `tools/list`, `tools/call`. Notifications (`notifications/initialized`, cancellations) get no answer. Unknown methods return -32601. A line that isn't JSON returns -32700 with a null id, and the server keeps reading. No batches, resources, prompts or logging.
- **Tools:** `inspect`, `tree`, `graph`, `why`, `doctor`. Each takes the CLI's arguments as `file`, `process` and `module` (`why` only). Each tool calls the same `inspection` function as the CLI and returns its `--json` value as text content. A command error, such as an unreadable file or an unknown process, comes back as a result with `isError: true` so the model can read it. An unknown tool or a missing `file` is a -32602 protocol error. `doctor` on an invalid composition is a normal result: the diagnostics are the answer.
- **Read-only:** tools only read the document file the client names. They don't run Cargo, start the app or write files. The app still writes its own document with `inspection_document`.

## Consequences

The server is a thin adapter: the CLI output and the tool output can't drift because they come from one function. A unit test drives it in process: initialize, tools/list, tools/call, a notification, a malformed line, an unknown method and ping. A client can read any JSON file the user running `clamp` can read. Only the schema check stands between it and the content, and a file that fails the check returns just the error message. That is the same trust as giving the client a shell. A handshake-less (2026-07-28) mode waits until a client we use needs it.
