# Existing Axum app

A plain Axum router, written with no knowledge of Clamp, joins Clamp
composition as one `HttpRoute::mount` contribution. A second module adds
`/health` beside it; the existing routes, state and layers stay as they were.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/02-axum-app/Cargo.toml --example 02-axum-app
cargo run --offline --locked --manifest-path rustclamp/examples/02-axum-app/Cargo.toml --example 02-axum-app -- serve
```

Without arguments it sends three requests through the composed router and
prints the answers; `serve` listens at `http://127.0.0.1:8003`.

## What can I do now?

- [Email worker](../03-email-worker/README.md): hand slow work to a worker
- [Site server](../08-site-server/README.md): serve a whole site through the HTTP target
