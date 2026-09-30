# ADR 0018: Worker-app stress test — cancellation, shutdown, composition

Status: Accepted (decisions 1–5, proposal A implemented), 2026-09-30. Proposals B–C open.

## Context

`test/jobrunner/` built one spec twice: a spool-directory job worker with two
processes (`run`, `enqueue`), typed handlers, retries, dead-lettering, bounded
concurrency with backpressure, a heartbeat, crash recovery and graceful drain on
SIGINT/SIGTERM. The plain-Rust build had 585 lines of code; the RustClamp
build, using core, kernel, runtime, worker, scheduler and messaging, had 806,
and its author recorded 14 framework gaps. Each was checked against the source.

## Decisions

1. **Awaitable cancellation** (runtime). `CancellationToken::cancelled()` and
   `TaskContext::cancelled()` return a std-only future that completes when the
   token or any parent is cancelled. Waiters deregister on drop, so a root token
   shared by thousands of jobs does not accumulate wakers. `spawn_async` selects
   on it instead of waking every 5 ms. The base runtime stays free of Tokio.
2. **Shutdown signal** (runtime, feature `signal`). `shutdown_signal().await`
   resolves on SIGINT, or SIGTERM on Unix (`ShutdownSignal::Terminate`).
   `wait_for_ctrl_c` stays.
3. **Route introspection** (worker). `HandlerRegistry::contains` and `routes`, so
   a one-shot process can validate a message against the same registry the
   worker dispatches with.
4. **Contributions from the frozen process** (kernel). `FrozenProcess::compose`
   builds a target from the blueprint's `add_contribution` edges: contributions
   from modules outside the process are dropped, an included module without an
   edge or a declared contributor with no value fails with `ComposeError`. This
   is ADR 0017's single-declaration rule applied to contributions.
   `TargetCompositionError` is unchanged; `compose` has its own error type so
   `build` callers never match variants they cannot receive.
5. **Scheduler first run** (scheduler). `JobDeclaration::first_run_after_interval()`
   opts a job out of running on the first tick. The default is unchanged. The
   immediate first tick was the only behavioral difference between the two
   builds.

Not changed: gap 4 (a handler error code travels as a boxed error and is
downcast, which is ordinary Rust); gap 8 (a thread-count knob on
`TokioRuntime::managed`, not needed by the spec); gap 14 (stable string IDs
are the composition model, see ADR 0017).

## Open proposals

**A. Handler signature (worker; gaps 2, 3, 5) — implemented.** Handlers take `Delivery { message, attempt }` and return a `Value`; `HandlerDeclaration::typed` decodes payloads (`DispatchError::InvalidPayload`) and backs `HandlerRegistry::validate`; `RetryPolicy` + `deliver()` classify each attempt (`Outcome`, `DeadReason`), which replaced example 08's hand-written classification. Original proposal: Handlers can neither return a
value nor learn the delivery attempt, and they observe cancellation only by
being dropped. All three change `HandlerFuture`, so they should land as one
breaking change: a handler receives `Delivery { message, attempt }` and returns
`Result<serde_json::Value, HandlerFailure>` (worker already depends on
serde_json; `Value::Null` for fire-and-forget). Cooperative cancellation would
add a worker → runtime edge, which needs a boundary-policy change. Drop
cancellation is enough for async handlers, so defer that part. Affects examples
08 and 09.

**B. Lifecycle driver (core/runtime; gap 10).** `Initialize/Start/Ready/Drain/Stop`
have no runner; both the job runner and example 05 order them by hand. A driver
needs a decision on ordering (the frozen dependency graph, reversed for
stop), failure policy per phase, and whether it lives in runtime or the facade.
Needs its own ADR before code.

**C. Owned provisions (kernel; gap 13).** `Provision` borrows `&'a C::Value`,
which forces `'static` values or long-lived locals. An `Arc` form
(`Provision::shared`) would allow constructed-at-startup values. Deferred until
a second app needs it.

## Consequences

Measured on the job runner, before and after rewriting it on 1–5: see
`test/jobrunner/RESULTS.md`.
