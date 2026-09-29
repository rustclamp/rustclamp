# CreateOrder Outbox

`orders-api create` decrements inventory, inserts an order, and inserts its
versioned outbox message in one PostgreSQL transaction. The `outbox-publisher`
polls pending rows in bounded batches, publishes to JetStream, then marks rows
published only after broker confirmation. It holds row locks while publishing;
multiple publisher processes skip rows already locked by another batch.

Run PostgreSQL and NATS with JetStream, then configure `DATABASE_URL` and
`NATS_URL` (which defaults to `nats://127.0.0.1:4222`):

```sh
docker run --rm --name rustclamp-orders-db -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=orders -p 5432:5432 postgres:17
docker run --rm --name rustclamp-orders-nats -p 4222:4222 nats:2-alpine -js
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/orders
cargo run --offline --locked --manifest-path rustclamp/examples/09-create-order/Cargo.toml --bin orders-api -- migrate
cargo run --offline --locked --manifest-path rustclamp/examples/09-create-order/Cargo.toml --bin outbox-publisher
cargo run --offline --locked --manifest-path rustclamp/examples/09-create-order/Cargo.toml --bin orders-api -- create widget 2
```

If publishing succeeds but the database transaction cannot commit its
`published_at` update, the row remains pending and may be published again.
Delivery is at least once; P7-14 adds a durable inbox before claiming duplicate
effects are bounded. The polling loop logs failures and retries at 500 ms
intervals; the stream itself rejects new messages after reaching 10,000 messages
or 64 MiB, leaving the outbox row pending for a later attempt.
