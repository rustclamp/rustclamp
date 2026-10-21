# ADR 0039: Scheduler leader lock on a Postgres advisory lock

Status: Proposed, 2026-10-21.

## Context

`rustclamp-scheduler` keeps its schedule and its "already running" flag per
process (ADR 0035 lists this). Its README tells anyone running several
replicas to bring "an explicit distributed lock", and nothing in the
ecosystem provides one, so N replicas run each job N times
(rustclamp/scheduler#11, rustclamp/postgres#8).

## Decision

- **The contract lives in the scheduler**: a `LeaderLock` trait with one
  method, `hold()`, a boxed future of `bool` ("take the lock if free, or
  confirm it is still held"). `Scheduler::leader_only(lock)` asks it once per
  tick that has a due job. A standby advances its schedule without running
  and counts the skip in `TickReport::not_leader`. The scheduler gains no
  dependency.
- **Not in Core.** The only consumer is the scheduler; a Core trait would be
  a placeholder to connect the graph, which CONTRIBUTING rules out.
- **The Postgres implementation lives in `rustclamp-postgres`** behind an
  optional `scheduler` feature: `Database::try_lock` takes a session advisory
  lock on a dedicated pooled connection, and `AdvisoryLeader` pings that
  connection on each `hold()` and re-acquires when it died.
- **New edge:** `rustclamp-postgres -> rustclamp-scheduler`, optional. Its
  closure is Core plus the Tokio crates the postgres entry already allows
  through SQLx, so `tools/boundaries.py` adds only `rustclamp-scheduler` to
  `ALLOWED` and `OPTIONAL`.

## Consequences

- Releases are ordered: scheduler 1.1 (the trait) before the postgres
  release that turns the feature on and bumps its requirement.
- `TickReport` gains a public field, which breaks struct literals and
  exhaustive patterns; `publish.py`'s semver gate will flag it for the
  scheduler release.
- Not included: a blocking or timed lock wait, per-job leader-only
  declarations, and lock implementations for SQLite or Redis. Each waits
  until an app needs it.
