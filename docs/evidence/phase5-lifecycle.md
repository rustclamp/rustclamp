# Phase 5 Lifecycle Progress

Status: complete. Lifecycle behavior uses deterministic fake resources; runtime, dependency, and build measurements cover both default and Tokio paths.

The `examples/05-lifecycle` package projects and freezes a Worker composition
before it opens its fake Database state or starts Users. Provider dependencies
derive Database → Users initialization; started consumers drain and stop before
their dependencies. Structured process status separates started, ready, alive,
admission, and health. Shutdown accepts an explicit reason and runs drain,
cancellation, reverse stop, and a final diagnostic flush.

## Verification

Twelve default-feature tests cover ordering, startup and health failures, partial-acquisition
cleanup, continued safe cleanup after a drain error, bounded fake operation
durations, optional degradation, programmatic shutdown, and external-resource
ownership. Clippy passes with warnings denied. The example stays facade-free
and has no Tokio dependency unless `tokio-runtime` is selected. The Runtime package has four manual-runtime tests and five Tokio adapter tests.

## Lifecycle Timings

The deterministic fake clock is configured with these operation durations:

| Measure | Result |
| --- | ---: |
| Database + Users initialization | 3 ms |
| Users + Worker start | 2 ms |
| Time to ready | 5 ms |
| Worker + Users drain | 4 ms |
| Worker + Users + Database stop | 3 ms |
| Start through completed shutdown | 14 ms |

These are simulated values for ordering and timeout tests, not elapsed hardware
performance. They do not predict production startup latency.

## Ownership Evidence

The managed fixture initializes and closes Database. With
`ExternalDatabaseOwner`, the database is already open, its Initialize and Stop
hooks are omitted, Users still receives its capability, and the database
remains open after process shutdown. This is a behavior comparison verified by
test, not a performance measurement. The external owner remains responsible for
closing it. With the fixture's configured durations, external ownership records
3 ms to ready and 11 ms through shutdown, versus 5 ms and 14 ms for the managed
path; the difference is exactly the skipped Database hooks, not a production
performance claim.

The coexisting-projection test freezes Worker and Reporter from one
application blueprint. Both process fixtures use the same application-owned
fake database, but have separate active state: stopping Worker leaves Reporter
active. Stopping both does not close the database; only the application owner
does. This demonstrates the intended ownership lifetimes in the example and
does not add resource injection or a resource registry to Kernel.

Worker declares two execution roots. The fixture permits an explicitly owned
execution resource only when its root appears in the frozen projection. The
two handles have independent active state and share the application database;
stopping one leaves the other active. This is caller-managed scoped state, not
a service locator or transient object manager.

`Outcome::inspection()` carries the frozen provider/consumer edges together
with actual fixture ownership, supported lifecycle phases, and cleanup status.
The example binary prints this inspection view. Tests verify managed versus
external database ownership, the participant phase list, successful cleanup,
and a sticky failure state when any cleanup phase fails.

## Build Comparison

The Phase 4 comparison below is retained as historical context. The final P5
build reports separately compare the same lifecycle executable with the default
and optional Tokio feature, with three release-build repetitions each.

| Metric | Phase 4 `04-process` | Earlier Phase 5 snapshot | Difference |
| --- | ---: | ---: | ---: |
| Clean-build median | 1.920 s (n=2) | 2.202 s (n=3) | +0.282 s |
| Unchanged-build median | 38.67 ms (n=2) | 40.13 ms (n=3) | +1.46 ms |
| Release executable | 4,609,920 B | 4,586,208 B | -23,712 B |
| Dependency packages | 2 | 2 | no change |
| Process wall-time median | 1.133 ms | 1.262 ms | +0.129 ms |

Process wall time includes process spawn, the complete example, output handling,
and exit; it is not time-to-ready. The controlled fake-clock timing above is the
readiness metric for this slice. Historical sample ranges, toolchain, and machine
details are in the [earlier raw report](reports/phase5-lifecycle-build.json).

| Final P5 metric | Default | Tokio feature |
| --- | ---: | ---: |
| Clean release build median (n=3) | 2.241 s | 6.525 s |
| Unchanged build median (n=3) | 47.11 ms | 57.85 ms |
| Release executable | 4,626,176 B | 4,673,872 B |
| Resolved dependency packages | 3 | 17 |
| Process wall-time median (n=3) | 1.337 ms | 1.457 ms |

The optional feature adds 47,696 executable bytes in this build. The default
dependency graph contains Core, Kernel, and Runtime; Tokio and its dependencies
appear only when selected. The [default build report](reports/phase5-lifecycle-default.json)
and [Tokio build report](reports/phase5-lifecycle-tokio.json) include raw
samples, toolchain, target, and machine details.

The instrumented task harness runs 4,500 tasks in each mode over nine samples.
Direct work measures 7 ns/task with no allocations; the manual supervisor
measures 601 ns/task, 5 allocations, and 96 requested bytes/task; the Tokio
blocking-task adapter measures 1.105 ms/task, 7.002 allocations, and 360.1
requested bytes/task. Managed Tokio startup measured 164 μs and process threads
went 1 → 3 → 1. These are host-specific microbenchmarks; Tokio scheduling here
uses `spawn_blocking`, and task timings are not an end-to-end service workload.
See the [default runtime report](reports/phase5-runtime-cost-default.json) and
[Tokio runtime report](reports/phase5-runtime-cost-tokio.json). Fake-clock
readiness and shutdown remain 5 ms and 14 ms respectively; separate wall-clock
samples are recorded in those reports.

## Remaining Phase 5 Work

This package is a proof, not a public production lifecycle API. Its fake
Database, Users, and Worker participants now execute through Core's independent
synchronous Initialize, Start, Ready, Drain, and Stop contracts with an
identity-only `LifecycleContext`. The dispatcher remains local to this example.
Managed versus external ownership, coexisting application/process lifetimes,
execution-scoped resources, lifecycle inspection, bounded fallback diagnostics,
and task supervision are demonstrated. The Tokio adapter is optional and
process-selected. The optional Tokio adapter also supports native async work with the same
completion, cancellation, timeout, retry, and required/optional failure
policies. The default runtime contract remains synchronous and executor-neutral.
