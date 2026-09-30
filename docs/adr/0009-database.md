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
  `Db::rollback` runs `down` for the last batch, newest first.
- Migrations and seeders are discovered, not registered: the app's
  `build.rs` calls `rustclamp::build::migrations` and `::seeders` (the std-only
  `build` feature, a build dependency), which list every file in
  `app/database/migrations/` and `app/database/seeders/` in file-name order.
  The file name is the recorded migration name (`migration_name(file!())`);
  the struct is that name without its leading numbers, in UpperCamelCase.
  `#[path]` maps a dated file to a module, since a module name cannot start
  with a digit.
- `Seeder` structs fill data; `Db::seed` runs each in a transaction.
  `rustclamp::db::command` runs `migrate`, `migrate:rollback` and `db:seed`
  from `cargo run -- <command>`; the server still migrates at startup.
- `db.table("posts")` is a query builder like Laravel's `DB::table`:
  `where_eq`, `where_op`, `where_null`, `where_in`, `where_exists`, `join`,
  `left_join`, `order_by`, `order_by_raw`, `limit`, then `get`,
  `first`, `count`, `insert`, `update`, `update_values` or `delete`. Values are always bound; the SQL
  fragments of `where_exists` and `order_by_raw` are `&'static str`;
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
engine exists, eager loading in the query builder, models on top of it, and MongoDB. Each waits until an app needs it.
