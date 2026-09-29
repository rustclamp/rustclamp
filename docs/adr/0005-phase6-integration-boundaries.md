# ADR 0005: HTTP and PostgreSQL Integration Boundaries

Status: Accepted for the Phase 6 prototype.

## Decision

HTTP is an optional `rustclamp-http` package built on Axum and Tower. It owns
HTTP route path validation, route compilation, request middleware, decoding,
content negotiation, response presentation, and graceful serving of a caller
owned listener. The compiled Axum router remains available to applications, so
they can add ecosystem middleware or use an existing Axum server. Core and
Kernel do not depend on HTTP crates.

PostgreSQL is an optional `rustclamp-postgres` package built on SQLx. It owns
qualified pool configuration, managed versus external pool cleanup, health
probes, and explicit migration ordering/execution. SQL and transaction handles
stay in application adapters. Core and Kernel do not depend on SQLx or a
database runtime.

The `06-users` consumer demonstrates a domain-owned repository port with memory
and PostgreSQL adapters. Direct calls, console, and HTTP use the same Users
operations. The console feature graph excludes HTTP, PostgreSQL, and Tokio.
PostgreSQL transactions are held by an infrastructure repository instance
created for one execution; pools are shared by clones within an application,
while separately constructed pools remain independent. This is process-local
resource behavior and makes no cross-process sharing claim.

## Evidence and consequences

The package and example test suites cover route conflicts, context propagation,
representation mapping, bounded request/response bodies, listener drain,
resource ownership, qualified database identity, migrations, tenant-scoped
transactions, rollback, and concurrent execution isolation. Real PostgreSQL
tests run explicitly through `postgres/tests/integration/run.sh`; no migration
runs automatically when a pool connects.

The HTTP package adds an Axum/Tower dependency graph when selected. SQLx adds a
larger database graph, including platform-specific transitive crates. The
dependency boundary checker records the resolved allowlists. Existing Axum and
SQLx APIs remain directly accessible to adapter authors.

## Alternatives

- A custom HTTP server/router would duplicate mature protocol and middleware
  behavior and was not justified by the examples.
- An ORM would obscure the transaction and SQL boundaries this phase is meant
  to test.
- Automatic migrations on application startup would couple process startup to
  schema mutation and make multi-process deployment unsafe by default.

## References

- [Axum documentation](https://docs.rs/axum/latest/axum/)
- [Tower HTTP documentation](https://docs.rs/tower-http/latest/tower_http/)
- [SQLx PostgreSQL documentation](https://docs.rs/sqlx/latest/sqlx/postgres/)
