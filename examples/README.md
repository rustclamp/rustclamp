# Examples

## Learning path

Each step is a runnable package that `tools/check.py` builds and runs in this order.

| Step | Example | Shows |
| --- | --- | --- |
| 0 | [`00-pico`](00-pico/README.md) | Hello Clamp |
| 1 | [`01-users`](01-users/README.md) | One operation from a CLI, then HTTP, then PostgreSQL |
| 2 | [`02-axum-app`](02-axum-app/README.md) | An existing Axum app joins Clamp without rewriting its routes |
| 3 | [`03-email-worker`](03-email-worker/README.md) | API hands work to a worker |
| 4 | [`04-scheduler`](04-scheduler/README.md) | Scheduled jobs and drain |
| 5 | [`05-messaging`](05-messaging/README.md) | Versioned message envelopes |
| 6 | [`06-create-order`](06-create-order/README.md) | PostgreSQL outbox to a broker |
| 7 | [`07-device-loop`](07-device-loop/README.md) | Deterministic device loop |
| 8 | [`08-site-server`](08-site-server/README.md) | Serve a site through the HTTP target |
| 9 | [`09-observability`](09-observability/README.md) | Metrics and one correlation id through API and Worker |

## Architecture track

How Clamp itself is built, in dependency order.

| Example | Shows |
| --- | --- |
| [`a1-capability`](a1-capability/README.md) | Typed capability injection |
| [`a2-module`](a2-module/README.md) | Module requirement and provider resolution |
| [`a3-contribution`](a3-contribution/README.md) | Domain-owned contribution targets |
| [`a4-process`](a4-process/README.md) | Process projection, configuration isolation, inspection |
| [`a5-lifecycle`](a5-lifecycle/README.md) | Startup, readiness, drain and cleanup |
| [`a6-platform-neutral`](a6-platform-neutral/README.md) | Core in a `no_std + alloc` consumer |
