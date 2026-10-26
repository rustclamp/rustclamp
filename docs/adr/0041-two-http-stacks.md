# ADR 0041: Two HTTP stacks, and which one to pick

Status: Proposed, 2026-10-26.

## Context

RustClamp has two ways to serve HTTP, and nothing says which one an app
should use:

- **`rustclamp::web`**, the `web` feature of the facade (`src/web.rs`). A
  synchronous server on the standard library: a pool of `WEB_THREADS` threads
  (default 32), `Router` with groups and middleware, views built by Vite into
  `public/build/`, forms, sessions, uploads, `auth`, throttling and error pages.
  `clamp init --web` scaffolds it. No async runtime, no Tokio.
- **`rustclamp-http`** (rustclamp/http), from ADR 0005. Axum and Tower:
  modules contribute `HttpRoute`s, the crate compiles them into an
  `axum::Router` and serves a listener the caller owns. It has request context,
  problem+json errors, body limits, health probes, metrics (ADR 0030), gRPC
  (ADR 0029) and idempotency keys (ADR 0037).

The two already overlap. Both answer errors in problem+json (`web::problem`
copies `HttpError`'s fields), both echo `X-Request-Id`, and both expose
Prometheus metrics, with differently named latency metrics (ADR 0030). The
root README lists `http` as "Optional Axum and Tower integration" next to a
facade that ships its own server, so a new user has to guess.

## Decision

Keep both. Each one is for a different kind of app.

| | `rustclamp::web` | `rustclamp-http` |
|---|---|---|
| App shape | Server-rendered web app: pages, forms, sessions, login | API or service built from the component crates |
| Start with | `clamp init NAME --web` | `rustclamp-http` + `core`/`kernel`/`runtime` |
| Concurrency | Thread pool, blocking I/O | Async, Tokio |
| Data | `rustclamp::db` (SQLite, Postgres) | `rustclamp-postgres` (SQLx) |
| Ecosystem | None; std only | Any Tower layer or Axum extractor |

- **`rustclamp::web` is the default.** The getting-started docs and
  `clamp init --web` keep using it. It is the Laravel-shaped part of the
  framework.
- **Use `rustclamp-http` when the app needs any of:** many long-held
  connections (streaming, server-sent events, long polling) that would use up
  a thread pool; gRPC; Tower or Axum middleware from the ecosystem; or
  composition through `core`/`kernel` contributions.
- **No bridge between them.** The facade does not depend on `rustclamp-http`,
  and `rustclamp-http` does not depend on the facade (ADR 0030 already keeps
  them independent). An app uses one stack. Handlers, middleware and routers
  are not portable between them, so moving an app from one to the other means
  rewriting its route layer.
- **`rustclamp::web` stays synchronous.** When an app needs async HTTP, the
  answer is `rustclamp-http`, not an async runtime inside the facade.
- **Shared conventions stay shared.** Where both stacks do the same thing on
  the wire, they do it the same way: problem+json fields, `X-Request-Id`, and
  `/livez`/`/readyz` where they exist. A change to one of these in one stack
  is matched in the other, or the difference is recorded in an ADR.

## Docs

- The root README's component table describes `http` as "Async HTTP on Axum
  and Tower for APIs and services; web apps use the `web` feature" and links
  this ADR.
- The `rustclamp-http` README says when to use it instead of
  `rustclamp::web`, and links this ADR.
- The `web` module docs (`src/web.rs`) end with a short paragraph that points
  to `rustclamp-http` for async, gRPC and Tower.

## Consequences

Two route APIs to document, test and keep in step on the wire. Users choose
once, at the start, and switching later costs a rewrite of the route layer.
In return, the default stays free of Tokio and Axum, and APIs keep the full
async ecosystem.

## Deferred

These wait until someone needs them: a guide for moving from `web` to `http`;
a `clamp init --http` scaffold; one latency metric name for both stacks (ADR
0030 keeps them separate for now).
