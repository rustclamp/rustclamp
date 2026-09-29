# Phase 7 Progress: Messaging and Workers

The initial `rustclamp-messaging` package defines a transport-neutral serialized
envelope with stable message identity, semantic name, schema version, correlation
and optional causation identity, and JSON payload. Local Rust events remain
ordinary in-process values. The `07-messaging` example maps a local `UserCreated`
event explicitly to this envelope before serialization.

The boundary and JSON payload choice are recorded in
[ADR 0006](../adr/0006-phase7-message-boundary.md). The package is added to the
local workspace and isolated-package tooling. Its `MessageBus` capability is
typed through Core, while transport and serialization remain outside Core and
Kernel. The bounded in-memory bus is for tests and development; it does not
claim cross-process delivery. NATS JetStream is selected as the real transport,
using durable pull consumers, explicit acknowledgments, and bounded batches.
Its delivery is at least once, so duplicate-safe handling is required.

## Verification so far

- `cargo check --offline --locked` passes for an isolated copy of the messaging
  package.
- `cargo fmt -- --check` passes for the messaging package.
- The messaging package's resolved dependency closure passes the boundary policy.
- The event-mapping example prints the explicit envelope's JSON representation.
- The in-memory bus is bounded and returns a full-queue envelope to the caller
  for retry rather than dropping it.

## Next

Implement the optional JetStream adapter, then prove handler contributions and
the API-to-worker path.
