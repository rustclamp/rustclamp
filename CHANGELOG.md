# Changelog: rustclamp

## Unreleased

### Changed (breaking)

- Views use templates instead of `<!--key-->` markers (ADR 0012). Old callers
  still compile but render wrong: markers are left as invisible comments, and
  values passed through `escape` are escaped twice. To migrate:
  - Replace `<!--key-->` with `{{ key }}`, or `{!! key !!}` for HTML you made.
  - Drop `escape(...)` from values passed to `render`, `Request::render` and
    `package_view`.
  - `security_headers` now blocks inline `<script>` and `on*=` handlers: move
    them into `resources/js`, or into `public/` with `vite-ignore`.
  - `<!--status-->`/`<!--reason-->` become `{{ status }}`/`{{ reason }}`;
    `<!--csrf-->` becomes `{!! csrf !!}`, and `<!--old:email-->` becomes
    `{{ old.email }}`. `<!--flash-->`/`<!--errors-->` become
    `@if(flash)…{{ flash }}…@endif` and `@foreach(errors as error)`.

### Added

- Login and roles (ADR 0013), the optional `auth` feature: `web::auth::Auth`
  (`attempt`, `create_user`, `sync_role`, `at_least`), `authenticate`,
  `required` and `role("admin")` middleware, `allows` with the before rule
  (`blocked` refused, `super-admin` allowed), `Request::user`/`auth`,
  `Session::invalidate` for logout. One `role` column per user. Logins are
  throttled per email and address; unknown email and wrong password are
  indistinguishable. `App` takes `roles` and runs `user:create`. The router
  answers every `403` as `404`, logged with `http_status_code=403`, with the
  same body and headers as a real 404. The login throttle
  (`LOGIN_PER_MINUTE`, default 5) honours `TRUST_PROXY`.
  `testing::Client::with_header`.
  `testing::Client::cookie`.
- Security: `security_headers` also sends a strict Content-Security-Policy
  (`web::CONTENT_SECURITY_POLICY`; scripts only from the app's files, a
  route's own policy wins), HSTS and a Permissions-Policy.
  `Session::regenerate()` moves a session to a new ID and CSRF token (call it
  on login). A `url` validation rule accepts only `http`/`https` links.
- Less app code: handlers may return `web::Result` and use `?` (the error is
  logged, the visitor gets `500`); `Sessions::from_config` reads
  `SESSION_MINUTES`/`SESSION_SECURE` and refuses an insecure cookie in
  production (`Config::is_production`); `web::markdown::to_html` behind the
  new `markdown` feature (pulldown-cmark) escapes raw HTML and unsafe link
  schemes; `web::App` runs a web app's startup; `Router::up` health check;
  `Request::flash`; `db::timestamp::{date, iso8601}`. The web template uses
  them and drops its health controller.
- `web::App` owns an app's wiring: it holds the routes too, opens and
  migrates the database, shares it and the disks with handlers and adds
  `security_headers`. `main.rs` is `my_site::app().run()`; tests use
  `app().test(env)` (in-memory, migrated, seeded). `web::testing::Client`
  browses a router like a visitor: cookie jar, CSRF token from the last page,
  `see`/`location`/`body_text` on `Response`. `Throttle::from_config(config,
  key, default)` reads the limit and `TRUST_PROXY`; `markdown::front_matter`
  splits `key: value` headers from Markdown.
- `web::serve` answers on a fixed pool of `WEB_THREADS` threads (default 32)
  instead of one request at a time; accepted connections queue four per
  thread, so a flood cannot grow threads or memory without bound.
- Web apps keep config in one file per area, like Laravel's `config/`:
  `app/config/app.rs`, `database.rs` and `logging.rs` name every key and its
  default. The framework takes them as `db::Settings` (`Db::connect`) and
  `log::Settings` (`Logger::new`); `Log::init` installs the app's logger at
  startup. `Db::open` and `Logger::from_config` still read `.env` directly.
- `rustclamp::storage` (`storage` feature, in `web`), like Laravel's
  `Storage`: named disks with `put`, `get`, `exists`, `delete` and `url`,
  refusing paths that leave the disk. Web apps configure `local`
  (`storage/app/private`) and `public` (`storage/app/public`, served at
  `/storage`) in `app/config/filesystems.rs`, reach them with
  `request.storage()`, and link `public/storage` at startup.
- File uploads: `multipart/form-data` forms, up to `web::MAX_UPLOAD` (10 MiB;
  other bodies keep the 1 MiB `MAX_BODY`). `Request::file`/`files` return
  `UploadedFile`s; `Request::form` and validation read the form's text fields,
  CSRF token included. `UploadedFile::store(disk, folder, allowed)` saves under
  a new UUIDv7 name and keeps only the listed extensions, so an upload cannot
  put `.html` or `.svg` on a public disk. Validation rules for uploads, as in
  Laravel: `file`, `image` (JPEG, PNG, GIF or WebP, checked by content; no
  SVG), `mimes:pdf,txt`, and `min`/`max` in kilobytes on file fields.
- Less boilerplate in apps: `build::database("app/database")` replaces the
  three `build::migrations`/`seeders`/`states` calls (still available) and
  writes one `database.rs`; migrations it lists are named after their file,
  so `fn name() { migration_name(file!()) }` can go (a hand-listed migration
  without `name()` panics rather than guessing). `log::request_log` is the
  request-logging middleware the web template used to carry in
  `app/http/middleware/`.
- Views are templates (ADR 0012): a std-only Blade subset rendered at request
  time from the Vite-built HTML. `{{ name }}` escapes, `{!! name !!}` does
  not; `@if`/`@else`, `@foreach`, `@extends`/`@section`/`@yield`,
  `@include`, with named values for components (`post: featured`). Data is
  `web::Value`, built from anything `web::ToValue`
  (text, numbers, bools, lists, options, and your models). `render`,
  `Request::render` and `package_view` take `&[(&str, &dyn ToValue)]`
  instead of `<!--key-->` slots; `Request::render` passes `csrf`, `flash`,
  `errors` and `old`; error views get `status` and `reason`. An unknown name
  or broken view is logged and answers `500`. `public/build/views/` is no
  longer served raw.
- The CLI is 0.3.0 (release tag `clamp-v0.3.0`): database, crypto and `.env`
  commands, and `clamp init --web` apps with `db` and `crypto`.

- Web packages: `web::Package`, `Router::package` (package middleware stays on
  package routes), `web::package_view` with app overrides under
  `views/vendor/{package}/`, and `clamp init NAME --package` (ADR 0008).
- `rustclamp::db` (optional `db` feature): SQLite via rusqlite with bundled
  SQLite, opened from `DB_CONNECTION`/`DB_DATABASE`. Laravel-style
  `Migration` structs with `up`/`down`, batches and `Db::rollback`;
  `Seeder` structs and `Db::seed`; a `Schema` builder (`create`, `table`,
  `drop`); a `db.table(...)` query builder; `db::command` for `migrate`,
  `migrate:rollback` and `db:seed`.
- Web: `Router::state` and `Request::state` share app values with handlers
  (`request.db()` with `db`); `Request::validate` with `required`, `min`,
  `max`, `email` and `integer` rules, and `Invalid::back` redirecting with
  errors and old input; `Session::flash`; `Request::render` passes the
  CSRF field, flash, errors and old input to the view.
- `Sessions::database(db)`: sessions in a SQLite `sessions` table, so they
  survive restarts; expired rows are pruned.
- `rustclamp::build::states`: every file in `app/database/states/` becomes
  `database::states::*`; the web template's `build.rs` calls it.
- `db::States`: allowed transitions for a status column, each move recorded
  in `state_history` with who made it; `history`, `can`, `allowed_from`.
- `clamp make:migration NAME` (dated stub; `create_x` and `add_y_to_x` are
  filled in) and `clamp make:seeder NAME`; `clamp migrate`,
  `migrate:rollback`, `migrate:status` and `db:seed` run the app's
  `db::command`, which gains `migrate:status`.
- Log channels as in Laravel: `single` (`file` still works), `daily`
  (`app-YYYY-MM-DD.log`, newest `LOG_DAILY_DAYS` kept), `stderr` and `stack`
  (`LOG_STACK=daily,stderr`); `Logger::daily`.
- `rustclamp::uuid` (optional std-only `uuid` feature, included by `db`):
  `Uuid::v7` (time-ordered, strictly increasing per process) and `Uuid::v4`,
  `parse`, SQLite mapping as text, and `Schema`'s `table.public_id()`.
- `rustclamp::crypto` (optional `crypto` feature, RustCrypto): Argon2id
  `Hash::make`/`check`, `Key` (`APP_KEY`, `base64:`) and `Crypt`
  (XChaCha20-Poly1305), `sha256`, `hmac_sha256` and constant-time
  `hmac_verify` (ADR 0010).
- `clamp key:generate`, `clamp env:encrypt` and `clamp env:decrypt`
  (`--key=`, `--env=`, `--force`, `CLAMP_ENV_KEY`); `clamp init --web` turns on
  `crypto` and lists `APP_KEY` in `.env.example`.
- `Db::transaction`: kept on `Ok`, undone on `Err`, and nested as a
  savepoint inside another transaction; migrations, rollbacks and state
  transitions use it, so they work inside a seeder. `Seeder::run` returns
  `SeedError` (any error), so `?` works on transitions and I/O too.
- `db::Model`: a table as a struct (`TABLE`, `from_row`) with `query`,
  `all` and `find`.
- `rustclamp::build` (optional `build` feature): `build.rs` discovery of
  `app/database/migrations/` and `app/database/seeders/`. `clamp init NAME
  --web` ships the `build.rs`, so a new file there is all it takes (ADR 0009).
- `clamp --version`; the CLI is 0.2.0 (release tag `clamp-v0.2.0`).
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
