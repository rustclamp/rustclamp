# Shared Users Operation

This example uses one domain service and a domain-owned `UserRepository` port
from direct tests, a standard-library-style console command, and an optional
Axum HTTP integration. The console target uses `futures-executor` to drive the
memory adapter's immediately ready operations; its dependency graph has no
HTTP, PostgreSQL, or Tokio package.

The HTTP feature resolves identity at the request boundary, adds tenant,
correlation, cancellation, and bounded deadline context, and enforces
authorization in the Users operation. `UsersHttp` contributes route declarations
that Kernel's public `TargetComposition` passes to the HTTP `HttpRoutes<Public>`
target. Route compilation happens before listener binding or pool setup.

JSON and HTML, XML, binary, and event-stream output all derive from the same
semantic `User` result. Malformed JSON is rejected by Axum's decoder before the
operation or repository runs. Errors keep the typed Users error internally and
use a deliberately generic server message for storage failures.

## Run

```sh
cargo run --offline --locked --manifest-path examples/01-users/Cargo.toml --bin users-console -- create Ada
cargo test --offline --locked --manifest-path examples/01-users/Cargo.toml
cargo test --offline --locked --manifest-path examples/01-users/Cargo.toml --features http --all-targets
cargo run --offline --locked --manifest-path examples/01-users/Cargo.toml --features http --example 01-users-http
cargo run --offline --locked --manifest-path examples/01-users/Cargo.toml --features http,measure-allocations --release --example measure-http
RUSTCLAMP_TEST_DATABASE_URL=postgres://... cargo run --offline --locked --manifest-path examples/01-users/Cargo.toml --features postgres,measure-allocations --release --example measure-postgres
```

With `postgres` selected, the app adapter uses an execution-scoped SQLx
transaction and can run against the service fixture documented in the
PostgreSQL package. Enable `tracing` to add operation spans and a subscriber.

The integration boundary and current verification results are documented in
the main repository's [Phase 6 evidence](../../docs/evidence/phase6-progress.md).

## What can I do now?

- [Existing Axum app](../02-axum-app/README.md): keep an existing Axum router and add Clamp beside it
- [Email worker](../03-email-worker/README.md): move work to a background worker
- [CreateOrder outbox](../06-create-order/README.md): publish messages reliably from Postgres
