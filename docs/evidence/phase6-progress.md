# Phase 6 Evidence: HTTP, Console, and PostgreSQL

Phase 6 adds the `rustclamp-http` and `rustclamp-postgres` integration packages
and the `06-users` consumer. The architecture choice and dependency boundaries
are recorded in [ADR 0005](../adr/0005-phase6-integration-boundaries.md).

## Verified behavior

- The HTTP suite exercises contributed route validation, Axum adoption,
  request context, authorization ordering, content negotiation, typed error
  disclosure, request limits, bounded streaming, disconnect cancellation,
  graceful shutdown, and port conflict handling.
- Users direct and console tests exercise shared domain operations, tenant
  isolation, authorization, validation, deadline, and cancellation behavior.
- PostgreSQL contract tests exercise `Primary`/`Analytics` identities,
  shared application pool clones, independent pools, managed/external ownership,
  selected configuration failure, connection failure, and migration planning.
- Explicit tests against local PostgreSQL 18.6 pass for migration application,
  migration rollback on failure, health, transaction rollback, Users commit and
  rollback, concurrent tenant isolation, and pool shutdown/reconnection.
- The console-only example build graph excludes Axum, SQLx, and Tokio.

The reproducible service fixture is `postgres/tests/integration/run.sh`; it
uses the checked-in Compose file and runs ignored integration tests explicitly.
For this evidence run, an ephemeral PostgreSQL 18.6 instance was initialized in
`/tmp` and the ignored tests were invoked directly with
`RUSTCLAMP_TEST_DATABASE_URL`.

## Verification commands

```sh
python3 rustclamp/tools/check.py
RUSTCLAMP_TEST_DATABASE_URL=postgres://... cargo test --offline --locked --manifest-path postgres/Cargo.toml --test postgres -- --ignored
RUSTCLAMP_TEST_DATABASE_URL=postgres://... cargo test --offline --locked --manifest-path rustclamp/examples/06-users/Cargo.toml --features postgres --test postgres -- --ignored
```

HTTP listener tests require local socket access. The database integration tests
need a reachable PostgreSQL server. The default Cargo test runs keep these
service tests ignored, so normal domain and mapping checks remain infrastructure
free.

## Measurements

The included JSON reports preserve toolchain, host, manifest, lockfile, commands,
and raw samples. They use two clean builds per profile and the repository's
existing release profile. All runs were from one container host; they are
prototype measurements, not portable predictions.

| Target | Resolved dependencies | Clean build median | Unchanged rebuild median | Release binary | Measured process |
| --- | ---: | ---: | ---: | ---: | ---: |
| Console only | 8 | 3.439 s | 102.3 ms | 521,712 B | 2.29 ms median |
| HTTP in-process workload | 62 | 17.940 s | 108.3 ms | 1,982,280 B | 59.1 ms median for the benchmark process |
| PostgreSQL in-process workload | 160 | 40.850 s | 134.6 ms | 5,651,192 B | 271.3 ms median for the benchmark process |

The paired release workloads used seven samples. The direct Axum route median
was 3,728 ns/request; the RustClamp route median was 6,857 ns/request (1.84x).
Both invoke the same Users operation and return JSON; the RustClamp path also
includes contributed routing, request middleware, and negotiation. The direct
route made 40 allocations and requested 3,519 bytes per request; the RustClamp
route made 69 allocations and requested 7,165 bytes per request. These counts
use `stats_alloc` only in the measurement examples.

The direct SQLx transaction median was 184.4 us/read; the Users adapter median
was 167.0 us/read (0.91x), using the same query and transaction boundary
against local PostgreSQL 18.6. Direct SQLx made 20 allocations and requested
12,832 bytes per read; the Users adapter made 25 allocations and requested
13,123 bytes. The small timing difference is within local host noise. The
benchmark sources are the `measure-http` and `measure-postgres` examples.

The runtime numbers include request/context construction, local scheduling,
serialization, and database/host noise as described above. See the raw
[`console`](reports/phase6-users-console.json), [`HTTP`](reports/phase6-http.json),
and [`PostgreSQL`](reports/phase6-postgres.json) reports.
