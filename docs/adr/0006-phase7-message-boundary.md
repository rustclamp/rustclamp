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
representation. Keep these dependencies out of Core and Kernel. The first
implementation does not promise binary payloads, schema evolution tooling, or a
specific broker.

## Consequences

- Message names and schema versions are explicit across process boundaries.
- A message can retain request/workflow correlation and direct causation.
- Domain event types remain independent from wire formats and broker behavior.
- A later real-transport proof must preserve these fields and define compatibility
  and delivery behavior without expanding the base dependency closure.
