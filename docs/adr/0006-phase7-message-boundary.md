# ADR 0006: Phase 7 Message Boundary

Status: Accepted

## Context

Phase 7 needs an explicit contract for work crossing process boundaries. The
existing in-process event examples carry Rust values and do not define stable
serialization, identity, or compatibility semantics.

## Decision

The optional `rustclamp-messaging` package owns a serializable envelope with a
stable message identity, semantic name, schema version, correlation identity,
optional causation identity, and JSON payload. Applications map local typed
events to this envelope explicitly. The envelope contains no broker delivery or
acknowledgement state; transport and worker packages own those concerns.

Use Serde for the public serialization contract and JSON for the initial payload
representation. Keep these dependencies out of Core and Kernel. The separate-
process proof will use NATS JetStream through the official `async-nats` client.
Use a durable pull consumer with explicit acknowledgments and a bounded batch.
JetStream supports at-least-once delivery, acknowledgment timeouts, redelivery
limits, and bounded pending acknowledgments; it can redeliver after an uncertain
acknowledgment, so handlers must tolerate duplicates. Max-delivery messages
remain in the stream and produce advisories, so dead-letter routing remains an
explicit application policy. Core NATS alone is not the reliability transport
for this proof.

The initial envelope does not promise binary payloads or schema evolution
tooling. The NATS adapter will stay optional and outside Core and Kernel.

References: [JetStream pull consumers](https://docs.nats.io/learn/jetstream/pull-consumers),
[async-nats JetStream API](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/).

## Consequences

- Message names and schema versions are explicit across process boundaries.
- A message can retain request/workflow correlation and direct causation.
- Domain event types remain independent from wire formats and broker behavior.
- The transport proof must preserve these fields and define compatibility and
  delivery behavior without expanding the base dependency closure.
