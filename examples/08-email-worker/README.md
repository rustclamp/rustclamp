# Email Worker

This combined development composition shows the API calling the domain-owned
`EmailSender` port, an adapter mapping the request to a message, and a Worker
handler invoking the email operation. The domain types have no broker or
serialization dependencies. It uses the bounded in-memory bus; a later step
will run the API and Worker as separate processes over JetStream.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/08-email-worker/Cargo.toml --example 08-email-worker
```
