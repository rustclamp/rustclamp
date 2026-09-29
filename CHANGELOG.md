# Changelog: rustclamp

## Unreleased

### Added

- Phase 0 package scaffold and development checks.
- Synchronous `Clamp::run` entrypoint and a prelude exporting `Clamp`.
- Pico example, plain Rust comparison, isolated dependency regression checks,
  ownership/error/panic tests, and reproducible cost and allocation measurements.
- Expanded the project README, architecture diagrams, verification guidance,
  measured comparisons, evidence, and release documentation.
- Updated isolated checks to include declared internal dependency closures and
  distinguish package-list validation from registry-dependent archive verification.
- Added colored test/benchmark terminal verdicts, per-benchmark intent text, and
  Phase 2 qualifier, cardinality, and composition-edit measurements.
- Added facade-free Clock and module consumer examples, request-state and
  non-Send borrowing tests, and example checks to the coordinated runner.
- Recorded Phase 2 example build reports and synthetic 1/5/20-module measurements.
- Added a target-owned CLI contribution example with qualified command sets,
  structured conflicts, orphan validation, and assembly/runtime measurements.
- Started process projection with stable application/process/execution
  identities, root-based reachability, and a CLI/Worker example sharing Clock.
- Completed the process-composition prototype with provider defaults and
  replacement provenance, projection-scoped configuration, freeze/inspection,
  process isolation assertions, build-target comparison, and scale evidence.
- Started Phase 5 with a synchronous fake-resource lifecycle proof covering
  dependency order, readiness, failure cleanup, deadlines, health, and shutdown.
- Added Phase 5 lifecycle evidence with fake-clock metrics and qualified
  build-footprint comparisons against the Phase 4 process example.
- Documented Core's opt-in synchronous lifecycle participation contracts in
  the lifecycle example and Phase 5 evidence.
- Added an external fake Database owner path that preserves caller ownership
  through process shutdown, with managed/external behavior and fake-clock
  comparisons in the lifecycle docs.
- Added a coexisting Worker/Reporter projection proof: application-owned fake
  Database state is shared and survives process shutdown, while process state
  remains isolated.
- Added an execution-scoped fake resource handle that can be constructed only
  for roots in the frozen process projection and stops independently.
- Added lifecycle outcome inspection for dependency edges, actual resource
  owners, participant phases, and cleanup state, with example output.
- Added optional Axum/Tower HTTP route integration, bounded streaming and
  graceful listener shutdown, plus qualified SQLx PostgreSQL pools and explicit
  migrations.
- Added the shared Users domain, console and HTTP adapters, optional
  transaction-scoped PostgreSQL adapter, isolated dependency checks, and Phase 6
  architecture/evidence documentation.
- Added release workload comparisons for direct Axum and SQLx paths, including
  measurement-only allocation instrumentation and dependency/build/binary reports.
- Added transport-neutral message envelopes, bounded in-memory and JetStream
  messaging, process-separated email workers, classified retries, deadlines,
  dead-letter handling, and tracing correlation.
- Added the PostgreSQL transactional outbox/inbox CreateOrder proof, payment
  uncertainty and compensation, process/ownership inspection, and live crash-
  window evidence against PostgreSQL and JetStream.
- Added the dependency-light Scheduler target with controlled-clock, overlap,
  misfire, admission, and drain checks, plus Phase 7 throughput and dependency
  measurements.
- Completed Phase 8 deterministic loop/device proofs, a Kernel-free no_std +
  alloc consumer, portability findings, cross-domain dependency-absence checks,
  and 1/5/20/50/100/500-module plus loop/footprint measurements.
- Added a Phase 9 RustClamp HTTP site-hosting example and compared its responses
  byte-for-byte with both Python preview sites.
- `web::error(status)` renders the app's `errors/{status}` or `errors/{N}xx`
  view, else a built-in page; router 404s, missing files and the unbuilt-frontend
  503 use it, and responses carry proper reason phrases.
- `web` parses full requests (headers, query, `Content-Length` body, peer) with
  size limits and a read timeout, adds response headers, `redirect`, and
  `Router::middleware` / `Router::group`; a panicking handler answers 500.
- `web::Throttle` / `throttle` fixed-window rate limiting (429 with
  `Retry-After`, opt-in `X-Forwarded-For`), `web::Sessions` server-side sessions
  and `web::csrf` (419); tokens come from `/dev/urandom`, keeping the facade
  dependency-free.
- `rustclamp::config` (`config` feature, included by `web`): `.env` plus
  environment, typed getters, values hidden from `Debug`.
- `web` routes accept `{name}` segments (`/blog/{slug}`), read with
  `Request::param`.
- `rustclamp::log` (`log` feature, included by `web`): `Log::debug` through
  `Log::emergency` (PSR-3 levels) to `storage/logs/app.log` or stderr, set by
  `LOG_LEVEL` (or `silent`), `LOG_CHANNEL` and `APP_ENV`; the web server logs
  handler panics.
- `web::render` (fills `<!--key-->` markers in a built view), `web::escape` and
  the `web::security_headers` middleware; `Throttle::per_minute`, and
  `Throttle::trust_forwarded` now takes a `bool`.
- `clamp init --web` uses the my-site layout: `app/routes/{web,api}.rs`,
  `app/resources/`, `.env.example`, with `/storage` and `.env` git-ignored.
- `web::error` fills `<!--status-->` and `<!--reason-->` in the app's error
  views. `clamp init --web` adds `errors/4xx.html` and `errors/5xx.html`, a
  Tailwind-only welcome page, `config/app.rs`, a base
  `http/controllers/controller.rs`, a health controller, a `request_log`
  middleware, and `http/requests/` and `models/` folders.
