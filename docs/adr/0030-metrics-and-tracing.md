# ADR 0030: Metrics and tracing

Status: Accepted, 2026-10-07.

## Context

Tracing is already correlated: `rustclamp_http::with_request_context` reads a valid `X-Request-Id` or `X-Correlation-Id` (or assigns one) and echoes it, `MessageEnvelope` carries a `correlation_id`, and the Worker examples open a span with it. The std web server tags log lines with the request's reference and answers with it in `X-Request-Id`. Metrics are not: `rustclamp::metrics::Registry` renders Prometheus text, but every app writes its own `/metrics` route and its own request counting, and an HTTP (Axum) app can't use the registry at all, because `rustclamp-http` doesn't depend on the facade.

## Decision

- **`rustclamp_http::with_metrics(router, path, extra)`** counts requests, `5xx` answers and time to response in three atomics, and answers `GET path` with them in the Prometheus text format: an `http_request_duration_seconds` summary (`_sum`, `_count`) and `http_server_errors_total`, followed by `extra()`. `extra` is how an app adds its own metrics (a `Registry::render`, or a few `format!` lines), so the HTTP crate doesn't need a registry. Scrapes aren't counted. The route is outside layers applied before it, so an app chooses whether auth covers it.
- **`Router::metrics(path, registry)`** on the std web router (`web` + `metrics`) does the same with a shared `Arc<Registry>`: `http_requests_total`, `http_server_errors_total` and `http_request_duration_microseconds_total`. The registry has integer counters only, hence microseconds; there are no float metrics to add for one total.
- **No new tracing API.** The correlation id already reaches both sides; what was missing was showing it. `examples/09-observability` copies `RequestContext::correlation_id` into the envelope, opens an `api` and a `worker` span with it, and its test asserts both trace lines carry the id the framework assigned, and that `/metrics` counted the request and the app's own counter.
- **Not included:** per-route or per-status labels, latency histograms, OpenTelemetry export, and W3C `traceparent` propagation. Labels and buckets wait for a dashboard that needs them; OpenTelemetry and `traceparent` wait for an app that ships traces to a collector. None needs a new dependency today.

## Consequences

One process-wide total per metric: a scrape tells an operator the request rate, error rate and mean latency, not which route is slow. Time stops at the response head, so a streamed body's duration isn't counted. The two stacks name the latency metric differently (seconds summary vs microseconds counter), because the facade registry has no floats; a dashboard reads one or the other, not both. `rustclamp-http` and the facade stay independent of each other.
