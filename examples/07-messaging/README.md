# Message Mapping

`UserCreated` is an in-process typed event with no serialization traits. The
application explicitly maps it to the versioned `MessageEnvelope` at the
process boundary and serializes the envelope. The message contract does not
change the domain event type. `rustclamp-messaging` also supplies a bounded
in-memory bus for transport-independent development and tests.

Run it from the ecosystem workspace:

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/07-messaging/Cargo.toml --example 07-messaging
```
