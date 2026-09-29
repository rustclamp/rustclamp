# ADR 0009: SQLite in the framework

Status: Accepted, 2026-09-29.

## Context

Web apps need a database, and a new `clamp init --web` app should have one
without running a server. `rustclamp::web` is synchronous with one thread per
connection, so the async SQLx integration (`rustclamp-postgres`) does not plug
into it.

## Decision

- The facade gets an optional `db` feature: `rustclamp::db::Db`, a synchronous
  SQLite connection from rusqlite with SQLite bundled (compiled in, no system
  library). The default build keeps zero dependencies; `web` does not imply
  `db`.
- `clamp init --web` enables `["web", "db"]`. `DB_CONNECTION` (only `sqlite`)
  and `DB_DATABASE` (default `storage/database.sqlite`, or `:memory:`) come from
  `.env`. Any other engine stops the app at startup.
- File databases use WAL, a 5 s busy timeout and foreign keys.
- Migrations are Laravel-style: a struct per file implementing `Migration`
  (`name`, `up`, `down`), with the SQL written by `Schema` (`create`,
  `table`, `drop`; SQLite dialect) or by hand. `Db::migrate` runs each once,
  in a transaction, recorded with its batch in a `migrations` table;
  `Db::rollback` runs `down` for the last batch, newest first. The web
  template runs `app/database/migrations.rs` at startup, and
  `cargo run -- migrate:rollback` rolls back.
- `db.table("posts")` is a query builder like Laravel's `DB::table`:
  `where_eq`, `where_op`, `where_null`, `order_by`, `limit`, then `get`,
  `first`, `count`, `insert`, `update` or `delete`. Values are always bound;
  table and column names must be plain identifiers or it panics, so a name
  taken from a request cannot carry SQL.
- One connection behind a mutex, shared by every request thread. A pool waits
  for measurements that show the lock matters.
- rusqlite is re-exported as `rustclamp::db::sqlite`. Apps use it rather than
  their own `rusqlite`: `libsqlite3-sys` links `sqlite3`, so two versions cannot
  coexist in one binary.
- `tools/boundaries.py` allows rusqlite's closure for `rustclamp` and requires
  it to stay optional. The wasm-only entries (`sqlite-wasm-rs`, `wasm-bindgen`,
  `js-sys`) appear because Cargo metadata resolves every target.

## Deferred

PostgreSQL and MySQL (synchronous drivers, same `DB_CONNECTION` switch; they
also cover RDS), Redis for cache, sessions and queues, file storage (local
disk, then S3-compatible for R2 and S3), a shared query trait once a second
engine exists, joins and eager loading in the query builder, models on top of it, and MongoDB. Each waits until an app needs it.
