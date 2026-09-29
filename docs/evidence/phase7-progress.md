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

The initial `rustclamp-worker` target compiles message-name/schema-version
handlers, rejects invalid or duplicate declarations, decodes serialized
envelopes, and preserves handler errors. It depends on public Core and Messaging
contracts; routing has no broker-specific message type.

The `08-email-worker` example wires the domain `EmailSender` port to a
`QueuedEmailSender`, routes its envelope through the in-memory bus, and invokes
the worker handler, which calls the email operation with a memory gateway. It
also defines independent `email-api` and `email-worker` executables that exchange
the versioned envelope over JetStream. The API waits for the broker's publish
acknowledgement; the Worker uses a durable pull consumer with one in-flight
message and ACKs only after handler success. The example README documents a
two-terminal run. Live broker execution is pending because the local Docker
daemon is unavailable. Delivery outcomes are explicit: success ACKs; transient
handler errors retry with bounded backoff for five total attempts; invalid
envelopes are copied to a dead-letter subject before source ACK; and a valid
message without a route is terminally rejected. The consumer lease is 45 seconds
with at most one unacknowledged delivery. Worker handlers classify failures as
retryable, permanent, or unknown outcome. Only retryable failures are authorized
to retry; unknown outcomes are preserved in dead-letter records and are not
retried automatically. The envelope propagates an optional Unix-millisecond
deadline; the Worker checks it before execution and times out work at the
deadline. A timeout during execution is recorded as unknown outcome. Queue depth,
unacknowledged work, and concurrent handler execution are bounded by stream
retention and a single in-flight delivery. The Worker stops fetching on Ctrl-C,
then completes or times out its current delivery before exiting; an unacknowledged
delivery after process loss is left for lease-based redelivery.
The example's opt-in tracing feature emits an API publish event and a Worker
handler span with the same correlation and message IDs.

The `09-create-order` example adds an atomic PostgreSQL transaction for
inventory decrement, order insert, and serialized outbox intent. A supervised
publisher claims at most ten rows with `FOR UPDATE SKIP LOCKED`, publishes each
message to a bounded JetStream stream, and marks it sent only after broker
confirmation. If the publish succeeds but the database commit fails, the row
remains pending and can publish again. The example compiles cleanly; live
database/broker execution and crash-window evidence are still pending.
The same example now includes an `orders-worker` with a durable PostgreSQL inbox.
It commits the inbox ID and fulfillment record atomically, then ACKs the broker
delivery. Duplicate message IDs skip the effect; a transaction failure rolls
both records back. The example compiles and passes Clippy, but concurrency and
crash windows still need executable failure evidence.

## Verification so far

- `cargo check --offline --locked` passes for an isolated copy of the messaging
  package.
- `cargo fmt -- --check` passes for the messaging package.
- The messaging package's resolved dependency closure passes the boundary policy.
- An isolated Worker package build with its Core and Messaging dependency closure
  passes, and its resolved dependency closure passes the boundary policy.
- The event-mapping example prints the explicit envelope's JSON representation.
- The in-memory bus is bounded and returns a full-queue envelope to the caller
  for retry rather than dropping it.
- The email-worker example runs successfully and passes Clippy with warnings
  denied.
- Both JetStream process projections compile and pass Clippy with warnings
  denied; live execution still needs a running NATS server.
- The combined email example still runs successfully after the deadline and
  handler-classification changes.
- The optional tracing and default email-worker feature sets compile and pass
  Clippy with warnings denied.
- The CreateOrder/outbox example passes offline Cargo check, Clippy with warnings
  denied, and Cargo metadata resolution. No database or broker was available for
  live execution.

## Next

Add crash-window, duplicate, poison-message, and retry-exhaustion evidence, then
run the process projections against live PostgreSQL and JetStream services.
