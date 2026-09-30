# ADR 0026: Durable SQLite transport and outbox for the worker service

Status: Proposed, 2026-09-30.

## Context

`WorkerService` (ADR 0020) runs handlers over a `Transport`, but the only
transports that exist are ones an app writes itself. The job-runner ladder
levels showed what an app needs and had to build locally (level 05):

- a `jobs` table with a dedupe key, an attempt count and a `run_at`, claimed
  "everything due on the injected clock";
- an `outbox` written in the same transaction as the business change, so a
  job exists if and only if the change committed;
- recovery of rows a crashed run left claimed.

`messaging` has only in-memory buses, so every such app copies the same SQL.
Facade `db` already depends on `rusqlite` (bundled), but the facade does not
depend on `worker`, and a `Transport` needs the worker's `service` types.

## Decision

1. **`rustclamp_worker::sqlite`, behind an optional feature `sqlite`**
   (implies `service`; adds `rusqlite` 0.40.2 with `bundled`, the exact
   requirement the facade's `db` feature uses, so a Cargo lock holds one
   `libsqlite3-sys`).
2. **`SqliteQueue`** is a `BlockingTransport` over one connection; an app
   wraps it in `Blocking` (from the ergonomics work, worker#4) so SQLite calls
   run on Tokio's blocking pool, not on the service task.
   - Table `worker_jobs(id, message_id, message, dedupe_key UNIQUE, run_at,
     state, attempts, result, reason, error)`, created by `migrate`.
   - `claim(limit)`: the rows with `state = 'pending' AND run_at <= now`,
     oldest `run_at` first, marked `claimed` in one statement. `now` comes
     from an injected `Clock`, so tests use `ManualClock`.
   - `settle`: `Done` and `DeadLetter` store state, attempts, result or
     reason and error. `Release` returns the row to pending.
   - `recover`: every `claimed` row goes back to pending (one consumer per
     database).
   - A row that does not decode is claimed as `Claim::Malformed`.
3. **`sqlite::enqueue(conn, message, run_at, dedupe_key)`** is the outbox
   helper: a plain insert on the caller's connection or open `Transaction`,
   so the job commits or rolls back with the caller's own writes. A repeated
   `dedupe_key` inserts nothing and returns `false`.

### Where it lives: worker feature, not the facade

| | worker `sqlite` feature | facade module |
|---|---|---|
| New direct edges | `worker -> rusqlite` (optional) | `rustclamp -> rustclamp-worker`, and with `service` `-> rustclamp-runtime`, `tokio` |
| Crates newly reachable | the `rusqlite` closure (29 packages, all already allowed for the facade) | `worker`, `messaging`, `runtime`, `tokio` and their closure into a facade that has none of them |
| Default builds | unchanged | unchanged only if made optional too |

The worker feature adds one edge to a crate that has no SQLite dependency
today, for apps that opt in. The facade option adds four, and inverts the
layering (the facade would depend on a leaf crate to implement that crate's
trait). `tools/boundaries.py` needs `rusqlite` and its closure allowed for
`rustclamp-worker` and listed in `OPTIONAL`; this ADR's PR carries that change.

## Open questions

1. **Durable retry.** The service retries in-process and holds the claim while
   backing off (ADR 0020), and `Settlement` has no retry variant. So this first
   version cannot "settle Retry by rescheduling `run_at`" (level 05): a crash
   during backoff re-runs the job through `recover`, with `attempts` reset. To
   reschedule, the service needs a mode where a retryable failure settles as
   `Settlement::Retry { delay, attempts, error }` and the transport stores
   `run_at = now + delay`. That changes the service contract (breaking
   `Settlement`) and needs a decision; it is not built here.
2. **Several consumers.** Claiming is atomic in SQLite, but `recover` assumes
   a single consumer. A lease column (`claimed_until`) would lift that; not
   built until someone runs two workers on one file.
3. **Retention.** Done and dead rows stay forever. A purge helper or a policy
   is left to the app for now.
4. **Table name and shape.** `worker_jobs` is fixed; an app that wants its own
   table or extra columns cannot. Configurable name is cheap to add if wanted.

## Consequences

- Apps get dedupe, due-time claiming, attempts, crash recovery and an outbox
  without writing SQL, tested on `ManualClock`.
- Default builds of `worker` are unchanged; only `--features sqlite` pulls
  `rusqlite` and compiles bundled SQLite (a C build).
- Enqueue is synchronous rusqlite, meant to run inside the app's own
  transaction; async apps call it from `spawn_blocking` as they do for
  other `db` work.
