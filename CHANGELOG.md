# Changelog: rustclamp

## Unreleased

### Added

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
