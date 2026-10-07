# RustClamp developer tooling

`rustclamp-tooling` provides a versioned inspection document API and the `clamp`
developer command. It consumes resolved process metadata from an application;
it does not parse source code, initialize the application, open connections, or
start services.

## Install the command

`curl -fsSL https://rustclamp.com/install.sh | sh` installs the latest release
binary (`clamp-v*` tags; Linux x86_64 and macOS) into `~/.cargo/bin`, verifying
its SHA-256 checksum. Other platforms build from GitHub `main`. Windows uses the
`.zip` from the GitHub release.

From the coordinated source checkout, install it into Cargo's global binary
directory:

```sh
cargo install --path rustclamp/tooling
```

After pulling or changing the source, `clamp self-update` reinstalls it from
the checkout it was built from. A release binary on Linux or macOS reruns the
installer instead; elsewhere it rebuilds from GitHub `main`.

Make sure `~/.cargo/bin` is on `PATH`. After installation, `clamp` works from
any current directory. It uses Cargo in that directory for project commands.
Running `clamp` with no arguments opens a welcome menu for project creation,
Cargo commands, and architecture inspection.

The application exports a document with `inspection_document(&[&projection])`
and writes the returned JSON value. A single document can contain multiple
processes from one application. Pass `--process ID` when it contains more than
one.

```sh
clamp inspect architecture.json
clamp tree architecture.json --process web
clamp graph architecture.json --json
clamp why architecture.json storage.postgres --process web
clamp doctor architecture.json
clamp check --all-targets
clamp test
clamp run --example my_app
```

Exit status is 0 for a valid document, 1 when `doctor` reports an invalid
composition, and 2 for command, file, or schema errors. `--json` returns
structured JSON. The output includes stable semantic IDs, sorted process and
relationship collections, resolved inclusion paths, provider selection,
replacements, exclusions, contributions, and structural counts. Counts are
descriptive; they are not estimates of memory or runtime cost.

The `check`, `test`, `build`, and `run` commands pass their arguments and exit
status through to Cargo in the current directory. `clamp init <project-name>`
creates a minimal Hello World project (`--blank`, the default). Add `--app` for a
larger Rust project layout with a composition module and sample health module,
`--web` for a Rust server with routes (Laravel-style `app/` with `routes/` and `resources/`, `public/`, `tests/`) and a Vite+ and Tailwind 4
frontend (needs Node/npm; `clamp dev` runs `vp build --watch` beside `cargo run`),
`--vue` or `--react` for the same web app with Vue or React components mounted on its
server-rendered pages (`<div data-component="Counter" data-props='{…}'>` loads
`resources/js/components/Counter.vue` or `.tsx`), or `--tui` for an
interactive terminal loop. `--package` creates a self-contained web package
instead: a library with its own routes, views, static files, settings and tests,
which an app adds with `Router::package` (ADR 0008). `--profile cli,service,worker`
combines profiles (ADR 0032): one file per profile in `src/`, the union of their
facade features, and a `main.rs` that runs the first one, or another by its
subcommand (`serve`, `work`). Every other project gets
`Procfile.dev` (`app: cargo run`) and a `cargo dev` alias for `cargo run`. `clamp dev` runs each `name: command`
line of `Procfile.dev` at once with `[name]`-prefixed output; a command that
fails stops the rest. All manifests use the RustClamp facade from its public Git repository so a
starter app can build before the crates.io publication gate is cleared.
Frontend starter kits are separate under `starter-kits/`. `clamp --version` prints
the installed version, which matches the `clamp-v*` release tag.

## MCP server

`clamp mcp` serves the inspection commands to an AI client as MCP tools over
stdio (newline-delimited JSON-RPC 2.0, protocol `2025-11-25` with the
`initialize` handshake; older revisions the client asks for are echoed). The
tools are `inspect`, `tree`, `graph`, `why` and `doctor`. Each takes `file`
(the inspection document), optional `process`, and for `why` a `module`, and
returns the same JSON as `--json`. They only read the document; nothing is built
or run. Register it with Claude Code:

```sh
claude mcp add clamp -- clamp mcp
```

Other clients take the same command in their config:
`{"mcpServers": {"clamp": {"command": "clamp", "args": ["mcp"]}}}`. See ADR 0031.

## Schema version 1

Every document has `schema_version: 1`, an `application` semantic ID, and a
`status`. Resolved documents have a `processes` array. Invalid documents have a
`process` ID and `diagnostics` array containing stable `code`, `severity`,
human `message`, and structured `context` fields. Diagnostic codes identify
categories; consumers should preserve unknown codes and fields for forward
compatibility. A changed meaning or incompatible shape requires a new schema
version. The command rejects unknown versions.

The inspection model contains architecture metadata only. It never serializes
configuration values, secrets, live runtime state, or provider credentials.
`with_config_keys(document, &config)` adds an optional `config_keys` array (key
names only), which `clamp inspect` shows; a value never enters the document.
Adding fields to this public schema requires compatibility review.

## Inspection boundaries and example coverage

The document is a snapshot of Kernel's resolved `ProcessProjection`. It shows
which execution roots, modules, providers, and contributions participate in a
selected process, with the reasons Kernel included or excluded modules. An
application must resolve its projections and call `inspection_document`
itself; `clamp` does not discover declarations from source code or invoke the
application.

Use these fields to explain the resolved architecture:

| Question | Projection field or command | Meaning |
| --- | --- | --- |
| What can this process start from? | `roots`; `included_modules[].reason` | Declared execution roots and the execution that made a module a root |
| Why is a module included? | `included_modules[].path`, `included_modules[].reason`; `clamp why FILE MODULE` | A deterministic root-to-module path and the root, provider, or contribution reason |
| Which provider was selected, and why? | `requirements[].provider`, `selection` | Selection is `unique`, `default`, `explicit`, or `optional_absent`; the projection records the chosen provider |
| Did a provider replace another? | `requirements[].replaced_provider` and provider `reason` | The replaced provider identity, when replacement occurred |
| Why was a declared module left out? | `exclusions[].reason`; `clamp why FILE MODULE` | `explicit` means excluded by declaration; `unreachable` means no root reached it |
| Which contributions are wired? | `contributions[]` | Contributor, consuming module, target, qualifier, and contribution identity |
| Which setting names/defaults does the application need? | Not represented in this Kernel projection | Configuration readers are application-owned; use the application's setup documentation and validation errors |

The projection shows static provider selection, but it cannot claim that a
runtime setting was read or validated. For concrete examples, [`a4-process`](../examples/a4-process/README.md)
requires positive-integer `WORKER_CONCURRENCY` only for Worker;
[`06-create-order`](../examples/06-create-order/README.md) requires
`DATABASE_URL` and defaults `NATS_URL` to its local NATS endpoint. Those
examples' setup and validation code owns the authoritative rules.

Lifecycle and ownership are outside this schema. Inspection does not report
which process owns a runtime resource, initialization and cleanup order,
readiness, task state, active work, or shutdown progress. The
[`a5-lifecycle` example](../examples/a5-lifecycle/README.md) documents those
owner and cleanup boundaries. They are runtime outcomes
and can change after a projection is frozen. An included module does not imply
that it has started or is healthy.

| Example | Architecture evidence | What inspection can represent |
| --- | --- | --- |
| `a3-contribution` | Typed CLI contribution target and command assembly | Not target assembly; it is not a process projection |
| `a4-process` | Roots, reachable providers, contributions, exclusions, and selected-process configuration | Its resolved projection; configuration reading remains application-owned |
| `a5-lifecycle` | Projection plus resource ownership, startup, readiness, drain, and cleanup | Projection only; lifecycle outcomes are omitted |
| `01-users` | Domain operation with console, HTTP, and PostgreSQL adapters | No exported process projection |
| `05-messaging` | Message mapping and transport-neutral envelope | No; message delivery is outside projection metadata |
| `03-email-worker` | API-to-worker message flow and broker adapter | No; process-to-process transport is not represented |
| `06-create-order` | PostgreSQL outbox/inbox and broker failure windows | Its process projections; transaction and delivery state are not represented |
| `04-scheduler` | Job composition, timing, admission, and drain | No; scheduler state is not represented |
| `07-device-loop` | Deterministic updates, input, CAN decoder table, and runtime selection | Its hardware composition projection; device and loop state are omitted |
| `a6-platform-neutral` | Core-only `no_std + alloc` consumer | No; it has no Kernel projection |
| `08-site-server` | HTTP route target serving generated static files | No; route composition is not a process projection |
| `09-observability` | Request metrics and one correlation id across API and Worker | No; metrics and traces are runtime output |

This is a coverage boundary, not a claim that `clamp` has been run against each
example. The coordinated runner compares every reference example that exports
a `ProcessProjection`: `a4-process`, `a5-lifecycle`, `06-create-order`, and
`07-device-loop`. Rows without a process projection have no comparable output
for this API. Runtime facts remain in their owning runtime or domain report
rather than being inferred from this document.
