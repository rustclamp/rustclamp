<img src="https://docs.rustclamp.com/assets/rustclamp-logo.png" alt="RustClamp logo" width="160">

# RustClamp

**RustClamp is the application composition layer for Rust.** It sits above the
crates you already use (Axum and Tower, SQLx, Tokio, NATS) and composes them
into one application: modules, typed capabilities, lifecycle and processes. It
does not replace them. This is the main repository: the `rustclamp` facade
crate, the `clamp` developer tool, examples and architecture decisions. The
component crates live in their own repositories (see [Components](#components)).

The facade starts small. With no features it has no dependencies and one
entrypoint:

```rust
use rustclamp::prelude::*;

fn main() {
    Clamp::run(|| {
        println!("Hello Clamp");
    });
}
```

`Clamp::run` calls the closure once on the current thread and returns its result
unchanged. Errors and panics keep ordinary Rust behavior. Everything else is an
optional feature you turn on.

## Quick start

Install the `clamp` tool and create a project:

```sh
curl -fsSL https://rustclamp.com/install.sh | sh   # or: cargo install --path tooling
clamp init my-app --web                             # also: --blank, --app, --vue, --react, --tui, --package, --profile
cd my-app
clamp dev
```

`--web` creates a std-only HTTP server with a Laravel-style `app/` layout and a
Vite+ and Tailwind 4 frontend (needs Node/npm). `--vue` and `--react` mount
components on server-rendered pages. `--package` creates a reusable web package
([ADR 0008](docs/adr/0008-web-packages.md)). `--profile cli,service,worker`
combines presets for a kind of software into one binary
([ADR 0032](docs/adr/0032-profiles-and-recipes.md)). See [tooling/README.md](tooling/README.md)
for every command, including `clamp make:controller`, `clamp make:migration`, `clamp migrate` and
`clamp inspect`.

To use the facade directly, depend on it from Git (publishing to crates.io is
disabled for now) and pick features:

```toml
[dependencies]
rustclamp = { git = "https://github.com/rustclamp/rustclamp", branch = "main", features = ["web", "db"] }
```

Rust 1.96.1 is the tested minimum (edition 2024). Run the smallest example with
`cargo run --offline --example 00-pico`.

## Features

All modules are opt-in Cargo features. Unsafe code is forbidden.

| Feature | Module | What it gives you |
| --- | --- | --- |
| `config` | `rustclamp::config` | `.env` and environment configuration |
| `log` | `rustclamp::log` | Leveled logging with `single`, `daily`, `stderr` and `stack` channels |
| `time` | `rustclamp::time` | UTC dates and times on `SystemTime`, RFC 3339 parse/format, no time crate |
| `uuid` | `rustclamp::uuid` | UUID v4 and v7; v7 comes from `rustclamp_core::Reference` ([ADR 0021](docs/adr/0021-reference.md)) |
| `storage` | `rustclamp::storage` | Named file disks (`local`, `public`) that refuse paths leaving the disk |
| `crypto` | `rustclamp::crypto` | Password hashing, encryption, SHA-256/HMAC, random bytes ([ADR 0010](docs/adr/0010-crypto.md)) |
| `db` | `rustclamp::db` | Bundled SQLite: migrations, seeders, `Schema` builder, `table(..)` query builder, `Db::transaction`, `States`, `#[derive(Model)]` ([ADR 0009](docs/adr/0009-database.md), [0014](docs/adr/0014-derive-macros.md)) |
| `web` | `rustclamp::web` | std-only HTTP server: routing, sessions, CSRF, validation, uploads, Blade-style views, `problem+json` errors, graceful `serve_until`, `web::testing::Client` ([ADR 0007](docs/adr/0007-phase9-web-scaffold-options.md), [0012](docs/adr/0012-view-templates.md)) |
| `auth` | `rustclamp::web::auth` | Login, roles, email verification and password reset ([ADR 0013](docs/adr/0013-auth-and-roles.md), [0016](docs/adr/0016-account-flows.md)) |
| `mail` | `rustclamp::mail` | SMTP with STARTTLS over rustls, queued through a `mail_outbox` table ([ADR 0015](docs/adr/0015-mail.md)) |
| `queue` | `rustclamp::queue` | Background jobs by name: `request.queue().dispatch(..)`, retries with backoff, `failed_jobs`, a `jobs` table or Redis, `queue:work` ([ADR 0019](docs/adr/0019-queue.md)) |
| `markdown` | `rustclamp::web::markdown` | Markdown to HTML that escapes raw HTML and unsafe links |
| `regex` | `rustclamp::web::Patterns` | `regex:NAME` validation rule with linear-time patterns ([ADR 0033](docs/adr/0033-regex-validation.md)) |
| `cache` | `rustclamp::cache` | Bounded in-process cache with LRU eviction and optional TTL |
| `metrics` | `rustclamp::metrics` | Counters and gauges with a Prometheus text renderer; `Router::metrics` serves them |
| `build` | `rustclamp::build` | `build.rs` helpers such as `build::database` |

`rustclamp::error` is always available: the `ExitError` trait and `error::run` /
`error::run_json` print an error and exit with its code.

Some ADRs are still proposed. Bearer API tokens (`Auth::issue_token`,
the `bearer` middleware) are implemented, but
[ADR 0023](docs/adr/0023-bearer-api-tokens.md) is not yet accepted, and neither are
ADRs 0021, 0022 and 0026. The [CHANGELOG](CHANGELOG.md) lists what has landed,
including breaking changes to views and `Db::transaction`.

## Components

Applications compose modules through typed capabilities and contributions.
Kernel resolves a process projection from selected roots; a runtime drives the
work. The composition model is still prototype-stage, and the examples show
each step: see the [examples](examples/README.md) for the learning path and the architecture track.

| Repository | Responsibility |
| --- | --- |
| [`core`](https://github.com/rustclamp/core) | Shared, domain-neutral contracts: capabilities, modules, contributions, identities, `Reference` |
| [`kernel`](https://github.com/rustclamp/kernel) | Composition and resolution of typed requirements and contributions |
| [`runtime`](https://github.com/rustclamp/runtime) | Executor-neutral task supervision; optional Tokio adapter |
| [`http`](https://github.com/rustclamp/http) | Optional Axum and Tower integration for HTTP interfaces |
| [`messaging`](https://github.com/rustclamp/messaging) | Versioned message envelope contracts |
| [`worker`](https://github.com/rustclamp/worker) | Message handler contributions and routing for workers |
| [`scheduler`](https://github.com/rustclamp/scheduler) | Contribution target for bounded, clock-driven jobs |
| [`postgres`](https://github.com/rustclamp/postgres) | Optional SQLx PostgreSQL pool and transaction integration |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets --all-features -- -D warnings
cargo test --offline --locked --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --offline --locked --no-deps --all-features
```

Measurements and their limitations are in [docs/evidence](docs/evidence/phase1.md).
For the coordinated checkout, architecture checks and release policy, see
[CONTRIBUTING.md](CONTRIBUTING.md). Documentation lives at
[docs.rustclamp.com](https://docs.rustclamp.com).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option ([ADR 0011](docs/adr/0011-license.md)).
Unless you state otherwise, any contribution you submit for inclusion is
dual licensed as above, without additional terms or conditions.
