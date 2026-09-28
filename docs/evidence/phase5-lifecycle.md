# Phase 5 Lifecycle Progress

Status: synchronous fake-resource proof; Phase 5 remains in progress.

The `examples/05-lifecycle` package projects and freezes a Worker composition
before it opens its fake Database state or starts Users. Provider dependencies
derive Database → Users initialization; started consumers drain and stop before
their dependencies. Structured process status separates started, ready, alive,
admission, and health. Shutdown accepts an explicit reason and runs drain,
cancellation, reverse stop, and a final diagnostic flush.

## Verification

Nine tests cover ordering, startup and health failures, partial-acquisition
cleanup, continued safe cleanup after a drain error, bounded fake operation
durations, optional degradation, programmatic shutdown, and external-resource
ownership. Clippy passes with warnings denied. The example stays facade-free
and adds no crates.

## Lifecycle Timings

The deterministic fake clock is configured with these operation durations:

| Measure | Result |
| --- | ---: |
| Database + Users initialization | 3 ms |
| Users + Worker start | 2 ms |
| Time to ready | 5 ms |
| Worker + Users drain | 4 ms |
| Worker + Users + Database stop | 3 ms |
| Start through completed shutdown | 12 ms |

These are simulated values for ordering and timeout tests, not elapsed hardware
performance. They do not predict production startup latency.

## Ownership Evidence

The managed fixture initializes and closes Database. With
`ExternalDatabaseOwner`, the database is already open, its Initialize and Stop
hooks are omitted, Users still receives its capability, and the database
remains open after process shutdown. This is a behavior comparison verified by
test, not a performance measurement. The external owner remains responsible for
closing it. With the fixture's configured durations, external ownership records
3 ms to ready and 9 ms through shutdown, versus 5 ms and 12 ms for the managed
path; the difference is exactly the skipped Database hooks, not a production
performance claim.

## Build Comparison

Three release-build repetitions were measured for Phase 5; the Phase 4 process
example had two. Both resolve Core and Kernel, but the Phase 5 executable
contains a different workload and extra lifecycle operations. The comparison
is a checkout snapshot, not a causal estimate of lifecycle overhead.

| Metric | Phase 4 `04-process` | Phase 5 `05-lifecycle` | Difference |
| --- | ---: | ---: | ---: |
| Clean-build median | 1.920 s (n=2) | 2.202 s (n=3) | +0.282 s |
| Unchanged-build median | 38.67 ms (n=2) | 40.13 ms (n=3) | +1.46 ms |
| Release executable | 4,609,920 B | 4,586,208 B | -23,712 B |
| Dependency packages | 2 | 2 | no change |
| Process wall-time median | 1.133 ms | 1.262 ms | +0.129 ms |

Process wall time includes process spawn, the complete example, output handling,
and exit; it is not time-to-ready. The controlled fake-clock timing above is the
readiness metric for this slice. Full sample ranges, toolchain, and machine
details are in the [raw report](reports/phase5-lifecycle-build.json); the Phase
4 report is [here](reports/phase4-process-example.json).

## Remaining Phase 5 Work

This package is a proof, not a public production lifecycle API. Its fake
Database, Users, and Worker participants now execute through Core's independent
synchronous Initialize, Start, Ready, Drain, and Stop contracts with an
identity-only `LifecycleContext`. The dispatcher remains local to this example.
Managed versus external ownership is demonstrated for a single process; shared
application/process lifetime, execution-scoped resources, observability failure
behavior, a reusable synchronous runtime driver, and the optional Tokio adapter
and supervision remain open. No Tokio dependency or runtime repository was
added.
