# ADR 0038: Postgres transport and outbox for the worker service

Status: Proposed, 2026-10-21.

## Context

An Axum app on `rustclamp-postgres` (ADR 0005) needs "commit the domain write
and enqueue the job in one transaction". The worker has that only for SQLite
(`SqliteQueue` and `sqlite::enqueue`, ADR 0026), and MQTT (ADR 0028) is a
broker, not an outbox. So the app either keeps jobs in a second store and
loses atomicity, or hand-writes the outbox table and claim SQL, as obix,
sqlxmq and many app-local `jobs` tables do. ADR 0034 put Postgres behind the
facade's sync `Db` and left `rustclamp-postgres` as the SQLx path for async
apps, which is where this belongs (worker#19).

SQLite's queue assumes one consumer per database: `recover` puts every
claimed row back (ADR 0026, open question 2). A Postgres database is shared by
several app instances, so that assumption does not carry over.

## Decision

1. **`rustclamp_worker::postgres`, behind an optional feature `postgres`**
   (implies `service`, adds an optional `rustclamp-postgres` and uses its
   re-exported `sqlx`, so an app's lock holds one SQLx).
2. **`postgres::enqueue(&mut tx, message, run_at, dedupe_key)`**: one insert
   on the caller's `PgConnection` (`&mut Transaction` derefs to it), so the
   job commits or rolls back with the caller's writes. A repeated
   `dedupe_key` inserts nothing and returns `false`, as in SQLite.
3. **`PgQueue`** implements `Transport` (async; no `Blocking` wrapper) over a
   `PgPool`, an injected `Clock` and a visibility timeout.
   - Table `worker_jobs`: the SQLite columns plus `claims` (a counter) and
     `claimed_until` (the lease end, Unix ms on the clock, like `run_at`).
   - `claim(limit)`: rows that are pending and due, or claimed with an
     expired lease, oldest `run_at` first, selected `FOR UPDATE SKIP LOCKED`
     in a CTE and marked claimed in the same statement. Concurrent consumers
     never get the same row and never wait on each other.
   - The receipt is `(id, claims)`. `settle` updates only while that claim
     still owns the row, so a consumer whose lease ran out cannot overwrite
     the new owner's outcome. `Done`, `DeadLetter` and `Release` store what
     SQLite stores.
   - No `recover`: an expired lease is the crash recovery, and it is safe
     with many consumers.
4. **`postgres::migration(version) -> rustclamp_postgres::Migration`** for the
   app's own `Migrations` plan. Pools never migrate on connect (ADR 0005), and
   the version is the app's, so it cannot collide with its own migrations.

### Where it lives: worker feature, not the postgres crate

| | worker `postgres` feature | module in `rustclamp-postgres` |
|---|---|---|
| New direct edges | `worker -> rustclamp-postgres` (optional) | `postgres -> worker`, `messaging`, `runtime`, `tokio`, `serde_json` |
| Layering | the trait owner implements it for a store, as for `sqlite` and `mqtt` | a leaf database crate implements the worker's trait |
| Default builds | unchanged | unchanged only if made optional too |

ADR 0026 chose the worker feature for SQLite for the same reasons. The
crates newly reachable from `rustclamp-worker` are exactly
`rustclamp-postgres`'s allowed closure; `tools/boundaries.py` adds that
closure to the worker's set and lists `rustclamp-postgres` in `OPTIONAL`.

## Open questions

1. **Lease extension.** The service holds a claim while the message waits for
   a runner and through every attempt and backoff (ADR 0020). A visibility
   timeout shorter than that lets a second consumer run the job in parallel.
   A heartbeat that extends `claimed_until` while a claim is held would remove
   the sizing rule; it needs a hook in `Transport` and is not built here.
2. **Durable retry.** Same as ADR 0026: retries are in-process, so a reclaim
   after a crash starts at attempt 1.
3. **Wake-ups.** Consumers poll. `LISTEN/NOTIFY` on enqueue would cut the
   latency to the poll interval's worth; not built until someone needs it.
4. **Retention and table name.** As in ADR 0026: done and dead rows stay, and
   `worker_jobs` is fixed.

## Consequences

- An Axum app gets an outbox, due-time claiming, dedupe, many consumers and
  crash recovery on the database it already has.
- Default builds of `worker` are unchanged; `--features postgres` adds SQLx.
- The worker's Postgres tests run when `RUSTCLAMP_TEST_DATABASE_URL` is set,
  each in a schema of its own; CI's examples job starts a Postgres for them.
