# Email Worker

This combined development composition shows the API calling the domain-owned
`EmailSender` port, an adapter mapping the request to a message, and a Worker
handler invoking the email operation. The domain types have no broker or
serialization dependencies. It uses the bounded in-memory bus.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/08-email-worker/Cargo.toml --example 08-email-worker
```

## Separate processes

Start a local JetStream server:

```sh
docker run --rm --name rustclamp-nats -p 4222:4222 nats:2-alpine -js
```

In one terminal, start the Worker; in another, run the API projection. Both
read `NATS_URL`, defaulting to `nats://127.0.0.1:4222`.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/08-email-worker/Cargo.toml --bin email-worker
cargo run --offline --locked --manifest-path rustclamp/examples/08-email-worker/Cargo.toml --bin email-api
```

The API waits for JetStream's publish acknowledgement. The Worker uses a
durable pull consumer with one unacknowledged message at a time and a 30-second
acknowledgement lease. It acknowledges only after its handler succeeds. A
worker crash or expired lease can result in redelivery; the email operation
must be idempotent before this example is used with an external mail service.
