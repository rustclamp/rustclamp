# ADR 0034: Postgres behind `Db`

Status: Accepted, 2026-10-07.

## Context

`rustclamp::db::Db` (ADR 0009) is SQLite only. That suits one app on one host, but not an app that runs several instances behind a load balancer: each host has its own file, so sessions, the queue's `jobs` (ADR 0019), the `mail_outbox` daily cap (ADR 0015) and the app's own tables are split per host. A managed database (RDS and the like, already named in ADR 0009's "Deferred") is not possible either. #121 asks for a Postgres driver behind the same `Db`, `Query` and `Schema` API, as an optional feature with a `boundaries.py` entry, unified with the `postgres` crate rather than forked.

What stands in the way today:

- **The public API is rusqlite.** `Db::with` and `Db::read` lend a `rusqlite::Connection`; `Query::where_eq` takes `&dyn ToSql`, `insert` takes `impl Params`, `get` maps a `rusqlite::Row`; every result is `sqlite::Result`. `Model::from_row` (and so `#[derive(Model)]` in `macros/`) takes `&sqlite::Row`. Apps write `sqlite::params![..]` (the web template's README and the `make:seeder` stub do).
- **SQL is SQLite dialect.** `Schema` writes `INTEGER PRIMARY KEY`, `INTEGER` for booleans and foreign ids, `TEXT` timestamps with `DEFAULT CURRENT_TIMESTAMP`. Raw SQL uses `?1` placeholders. `Query::insert` returns `last_insert_rowid()`; `get` writes `LIMIT -1 OFFSET n`. `migrate_user_version` is `PRAGMA user_version`.
- **Framework code assumes one writer.** `queue.rs` reserves with `UPDATE jobs ... WHERE id = (SELECT id ... LIMIT 1) RETURNING ...`, and `mail.rs` counts today's mail and then claims a row in one deferred transaction. Both are correct only because SQLite runs one write transaction at a time. Under Postgres `READ COMMITTED`, two workers can reserve the same job and two deliverers can overshoot the cap. `RETURNING` and `ON CONFLICT ... DO UPDATE` (sessions) are the same in both engines.
- **The existing Postgres crate is async.** `rustclamp-postgres` (ADR 0005) wraps SQLx `PgPool` on Tokio, with its own `rustclamp_migrations (version, description)` table and no migrate-on-start. `web::App` is std-only with a thread per connection, and the facade allows no Tokio (`tools/boundaries.py`; ADRs 0019 and 0026 rejected those edges for the same reason).

The repository does not say which engines the fleet's apps use in production.

## Decision

### Client: a std-only wire-protocol client in the facade

| | (a1) `postgres` crate (sync) | (a2) own client, std `TcpStream` | (b) reuse `rustclamp-postgres` |
|---|---|---|---|
| Runtime | wraps `tokio-postgres` and a Tokio runtime | blocking, like SMTP (ADR 0015) and RESP (ADR 0027) | async SQLx on Tokio |
| New crates | about 60 (`postgres` 0.19 resolved 2026-10-07: tokio, mio, socket2, futures-*, rand, md-5, stringprep, unicode-*, whoami, parking_lot, phf); TLS needs another adapter crate | none: SHA-256 and HMAC (`sha2`, `hmac`), `base64ct`, `getrandom`, `rustls` and `webpki-roots` are already allowed for the facade | SQLx's graph, plus `tokio` in the facade |
| Fits `Db` | yes, but types are its own | yes | no: every call needs a runtime and `block_on` |

- **Recommended: (a2), behind an optional `postgres` feature** (`db` plus `dep:sha2`, `dep:hmac`, `dep:base64ct`, `dep:getrandom`, `dep:rustls`, `dep:webpki-roots`). It adds no crate that `boundaries.py` doesn't already allow for `rustclamp`; only the feature comment changes. What it has to cover, sized from `src/mail/smtp.rs` (478 lines) and `src/redis.rs` (621 lines), comes to roughly 1,000 to 1,300 lines with tests:
  - startup message and `ParameterStatus` / `BackendKeyData` / `ReadyForQuery`;
  - auth: SCRAM-SHA-256 (PBKDF2 over the existing HMAC, client nonce from `getrandom`), and cleartext only over TLS. MD5 is refused with a clear error (Postgres has deprecated it);
  - simple query for migrations and `execute_batch`; extended query (`Parse`/`Bind`/`Describe`/`Execute`/`Sync`) for bound statements, all in **text format**: parameters are sent untyped and the server infers them, and results are decoded by column type OID (int2/4/8, float4/8, bool, text, varchar, bytea hex, null);
  - `ErrorResponse` fields into an error with the SQLSTATE, so a unique violation (`23505`) can be told apart;
  - TLS: `SSLRequest`, then rustls with SNI and `webpki-roots`, the same handshake code path as SMTP's STARTTLS.
- **(b) is rejected for the facade**: it is the Tokio edge ADRs 0019 and 0026 already turned down. "Unify, don't fork" is read as: one set of config keys and one migration story (see open question 3), while `rustclamp-postgres` stays the SQLx integration for async (Axum) apps, as ADR 0005 has it. If (b) is preferred anyway, it belongs in the `rustclamp-postgres` repo with a sync wrapper, not in the facade.

### How `Db` holds two engines

- **A private enum, not a trait**: `Db` holds `Backend::Sqlite { writer, readers }` or `Backend::Postgres(Pool)`, like `queue::Driver`. Two engines don't need a trait object or a generic parameter on every `Db`, `Query` and `Model`.
- **Facade-owned types replace rusqlite in the shared API**: `db::Value` (null, integer, real, text, blob, bool), a `db::Param` trait for bound values (`i64`, `f64`, `bool`, `&str`, `String`, `Vec<u8>`, `Option<T>`, `Uuid`), `db::Row` with `get::<T: FromColumn>(name)`, `db::Error` (with the SQLSTATE or SQLite code), a `db::params!` macro, and `db::Connection`, which `Db::with`, `Db::read` and `Tx` lend (`execute`, `query_row`, `query`, `execute_batch`). `Model::from_row` and the derive move to `db::Row`. **This is a breaking change** for any app that names `sqlite::` types; `rustclamp::db::sqlite` stays as an escape hatch, reachable through a SQLite-only `Db::sqlite(|connection| ..)` that returns an error on Postgres.
- **Config**: `DB_CONNECTION=sqlite|pgsql` (Laravel's name), and for `pgsql` either `DATABASE_URL` (`postgres://user:pass@host:port/db?sslmode=..`) or `DB_HOST`, `DB_PORT` (5432), `DB_DATABASE`, `DB_USERNAME`, `DB_PASSWORD`, `DB_SSLMODE`. Without the `postgres` feature, `pgsql` stops the app at startup with an error that names the feature, as an unknown engine does now.
- **Placeholders**: `Query` writes `?` or `$n` from the dialect. Raw SQL is written once with `?` or `?N`; on Postgres a small lexer rewrites them to `$N`, skipping string literals, quoted identifiers, comments and dollar-quoted bodies.
- **Dialect differences the facade handles**:
  - `Query::insert` uses `RETURNING id` on Postgres (`last_insert_rowid()` on SQLite). A table with no `id` column uses a new `insert_only` that returns nothing.
  - `Query::get` writes `OFFSET n` without a `LIMIT` on Postgres.
  - Upsert keeps `INSERT ... ON CONFLICT (..) DO UPDATE`, valid in both.
- **`Schema` renders per dialect**: `Schema::create` and `::table` return a `Sql` value (`Display`, so `Migration::up` returns `impl Into<Sql>`) that `Db::migrate` renders for its engine. Mapping on Postgres:

  | `Schema` | SQLite (unchanged) | Postgres |
  |---|---|---|
  | `id` / `auto_id` | `INTEGER PRIMARY KEY [AUTOINCREMENT]` | `BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY` |
  | `integer`, `foreign_id` | `INTEGER` | `BIGINT` |
  | `real` | `REAL` | `DOUBLE PRECISION` |
  | `boolean` | `INTEGER` | `BOOLEAN` |
  | `string`, `text`, `public_id` | `TEXT` | `TEXT` |
  | `timestamp` | `TEXT`, `CURRENT_TIMESTAMP` default | `TEXT`, default `to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')` |
  | new `blob` | `BLOB` | `BYTEA` |

  Timestamps stay text in the same `YYYY-MM-DD HH:MM:SS` UTC form, so `db::timestamp` and every row mapper work unchanged; UUIDs stay hyphenated text. `Column::default(sql)` and `check(sql)` are raw SQL and stay the author's responsibility; `default_bool(false)` and `default_now()` are added so the common cases are portable. Hand-written SQL migrations run as written; an app that targets both engines uses `Schema` or branches on `db.dialect()`.
- **Framework-owned tables** (`migrations`, `jobs`, `failed_jobs`, `mail_outbox`, `sessions`, `state_history`) are created through `Schema` or a dialect branch, not the SQLite DDL strings that live in `db.rs`, `queue.rs`, `mail.rs`, `web/session.rs` and `db/states.rs` today.

### Transactions, locking, connections, TLS

- **Transactions**: `Db::transaction` is `BEGIN` / `COMMIT`, nested calls are savepoints, as now. `transaction_immediate` keeps its meaning (one such writer at a time) on Postgres by taking `pg_advisory_xact_lock(<fixed key>)` after `BEGIN`. That serializes those transactions across every instance, which is what SQLite's `IMMEDIATE` does. The mail claim switches to `transaction_immediate`.
- **Queue claims** add `FOR UPDATE SKIP LOCKED` to the reserving subquery on Postgres, so workers on several hosts never reserve the same job and don't wait on each other.
- **Migrations** take `pg_advisory_lock` for the whole run, since every instance migrates at startup. `migrate_user_version` stays SQLite only and returns an error on Postgres.
- **Pool**: a mutex-guarded `Vec` of idle connections, opened on demand up to `DB_POOL` (default 10), with a 5 s wait for a free one (the same patience as SQLite's busy timeout) and then an error. A connection that fails mid-statement is dropped, not returned. A transaction holds one connection until it ends. There is no separate reader set; MVCC lets reads run beside writes.
- **TLS**: `DB_SSLMODE` = `disable`, `require` (encrypt, no certificate check) or `verify-full` (the default for any host other than localhost or a Unix socket path). There is no `prefer`, so a downgrade can't happen silently.
- **`Db::blocking`** works unchanged; it only moves the call to a thread.

### Testing

- Every test that exercises `Db`, `Query`, `Schema`, `Model`, the queue, the outbox, sessions and auth runs against both engines through one helper that returns a `Db` for the engine under test. SQLite always runs. Postgres runs when `RUSTCLAMP_TEST_DATABASE_URL` is set, each test in its own schema (`SET search_path`) that is dropped afterwards.
- CI's `test` job adds a `postgres:17-alpine` service beside `redis:8-alpine` and sets the variable, and runs the suite with `--features postgres`.
- The wire client also gets unit tests against a fake server on a local `TcpListener` (startup, SCRAM exchange with RFC 7677's test vector, error responses, a dropped connection), like the RESP and SMTP clients.

### Not included

MySQL, read replicas, `LISTEN`/`NOTIFY`, `COPY`, binary format, native `timestamptz`/`uuid`/`jsonb` columns, client certificates, Unix-socket peer auth, prepared-statement caching, and query cancellation (`BackendKeyData` is read but unused). Each waits until an app needs it.

## Resolved questions (2026-10-07)

1. **Breaking the API:** in one release. Nothing is published on crates.io yet (#84), so there is no outside user to migrate; the CHANGELOG lists it as breaking.
2. **Feature shape:** `postgres` implies `db`, and SQLite stays compiled in. Tests and local development keep using SQLite `:memory:`.
3. **`rustclamp-postgres`:** shared env keys only. Each keeps its own migrations table; `examples/01-users` stays on the crate.
4. **Types:** text timestamps in the same UTC format on both engines, and `BOOLEAN` for booleans.
5. **TLS:** `DB_SSLMODE` defaults to `verify-full` off localhost, and `DB_SSLROOTCERT` (a CA file) ships from the start for providers with their own CA.
6. **Fleet target:** still open. No app is committed to moving yet; CI tests against Postgres 17.

## Consequences

- An app switches engines with `DB_CONNECTION` and the `postgres` feature. Its own raw SQL is portable only if it sticks to the common subset.
- The default build and a `db`-only build are unchanged in dependencies. `--features postgres` adds no crates beyond those already allowed for `rustclamp`, so `tools/boundaries.py` needs no new `ALLOWED` entries; only its comment names the `postgres` feature as another user of `sha2`, `hmac`, `rustls` and `webpki-roots`. The `OPTIONAL` set already requires them to stay optional.
- The facade maintains a second wire protocol. SCRAM, TLS and type decoding are security-relevant code that needs the same review as the SMTP client.
- Each `Query` call on Postgres is a network round trip rather than an in-process call, so N+1 loops that SQLite hid become visible. Eager loading (deferred in ADR 0009) gains priority.
- The test suite runs twice in CI, and the SQLite-only assumptions in `queue.rs` and `mail.rs` get Postgres-specific code paths that only the live job exercises.
