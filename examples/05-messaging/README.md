# Message Mapping

`UserCreated` is an in-process typed event with no serialization traits. The
application explicitly maps it to the versioned `MessageEnvelope` at the
process boundary and serializes the envelope. The message contract does not
change the domain event type. `rustclamp-messaging` also supplies a bounded
in-memory bus for transport-independent development and tests.

Run it from the ecosystem workspace:

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/05-messaging/Cargo.toml --example 05-messaging
```

## What can I do now?

- [CreateOrder outbox](../06-create-order/README.md): deliver messages through an outbox
- [Email worker](../03-email-worker/README.md): handle messages in a worker
