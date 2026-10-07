# Observability

One signup request goes through the API and is handled by the Worker. The
framework serves `/metrics` and counts the request; the app adds its own
counter to the same scrape. The request's `X-Request-Id` (generated when the
client sends none) becomes the message's correlation id, so the API's and the
Worker's trace lines carry the same id.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/09-observability/Cargo.toml --example 09-observability
```

It prints the two trace lines, then the scrape:

```text
INFO api{correlation_id=request-1}: queued welcome email
INFO worker{correlation_id=request-1}: sent welcome email to=ada@example.test
# TYPE http_request_duration_seconds summary
http_request_duration_seconds_sum 0.000167
http_request_duration_seconds_count 1
# TYPE http_server_errors_total counter
http_server_errors_total 0
# TYPE emails_sent_total counter
emails_sent_total 1
```

- `with_request_context` reads or assigns the request id; the handler copies
  `RequestContext::correlation_id` into `MessageEnvelope::correlation_id`.
- `with_metrics(router, "/metrics", extra)` counts every request except the
  scrape and appends `extra()`, here `emails_sent_total`. An app on the std
  web server gets the same from `Router::metrics` with a `metrics::Registry`.
- The API and Worker share an in-memory bus; across processes the envelope
  carries the id the same way (see [Email worker](../03-email-worker/README.md)).

Totals only: no per-route labels or latency buckets yet (ADR 0030).

## What can I do now?

- [Capability](../a1-capability/README.md): start the architecture track
