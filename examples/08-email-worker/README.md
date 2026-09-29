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
durable pull consumer with one unacknowledged message at a time and a 45-second
acknowledgement lease. It acknowledges only after its handler succeeds. A
worker crash or expired lease can result in redelivery; the email operation
must be idempotent before this example is used with an external mail service.
The stream is bounded to 10,000 messages and 64 MiB with a new-message discard
policy, so publishing fails when capacity is exhausted instead of deleting
older queued work.
The message carries a 20-second deadline, below the 45-second lease. The Worker
checks it before starting and bounds any handler attempt to 25 seconds. If
timeout drops an in-flight handler, its side effect is treated as unknown and
sent to the dead-letter subject because it may already have happened. On Ctrl-C,
the Worker stops fetching and finishes its current delivery before exiting.

Handler errors retry at 1, 5, 15, then 30 seconds, up to five total deliveries.
The final failed attempt is copied to `email.dead`; the source is acknowledged
only after JetStream confirms the dead-letter publish. Invalid envelope bytes
go directly to the dead-letter subject. A valid envelope with no matching
handler is rejected with a terminal acknowledgement and remains in the source
stream for inspection. JetStream delivery is at least once, including the
publish-to-dead-letter/ack gap.

Handlers must classify failures as retryable, permanent, or unknown outcome.
Only retryable failures are authorized for another attempt. An unknown outcome
is dead-lettered with its classification because retrying after a possible
remote side effect could duplicate that effect. An application that wants to
retry unknown outcomes must first protect the side effect with an idempotency
key or durable inbox.
