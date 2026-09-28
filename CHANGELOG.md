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
