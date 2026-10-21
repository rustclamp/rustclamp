# ADR 0037: Idempotency keys in http, their Postgres store in postgres

Status: Accepted, 2026-10-21.

## Context

Payment and order APIs on `rustclamp-http` hand-roll `Idempotency-Key`
handling per endpoint (rustclamp/http#23). A shared store is needed once an app
runs more than one instance, and the shared database is PostgreSQL
(rustclamp/postgres#7).

## Decision

- `rustclamp-http`, feature `idempotency`: `with_idempotency`, an
  `IdempotencyStore` trait and an in-memory store. Request fingerprints are
  SHA-256 (`sha2` 0.10, whose closure http already allows through `ws`).
- `rustclamp-postgres`, feature `idempotency`: `PgIdempotencyStore`, through a
  new **optional** edge `rustclamp-postgres -> rustclamp-http`. The trait's owner
  and the store's owner differ, so one of them has to name the other; the
  integration that owns SQL depends on the one that owns HTTP, never the
  reverse. `boundaries.py` lets postgres reach http's allowlist and keeps the
  edge optional.
- A 5xx answer releases the key instead of being cached: a server error
  usually means the work did not happen, and a retry should run it.

## Alternatives

- Trait in Core: Core would carry an HTTP-shaped contract it does not need.
- Postgres store in http behind `sqlx`: http would own SQL and SQLx's graph.
- Application glue implementing the trait over a postgres helper: every app
  writes the same adapter, which is what the feature is meant to remove.
