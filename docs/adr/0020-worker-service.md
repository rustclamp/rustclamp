# ADR 0020: WorkerService — the framework owns the worker loop

Status: Accepted, 2026-09-30. Amends the worker README's rule that retry and
dead-letter policy belong to the transport adapter.

## Context

Two apps built on `rustclamp-worker` wrote the same loop by hand. The
`test/jobrunner` RustClamp build spent 293 of its 795 lines in `runner.rs`
on claiming with backpressure, bounded concurrency, retries with backoff,
dead-lettering, and a timed drain that cancels what is still running.
Example 08 (NATS email worker) hand-classified every dispatch result into
ack, retry, reject and dead-letter, with a stepped backoff table, a deadline
check and a handler timeout. Neither loop involved anything specific to its
transport. ADR 0018 fixed the handler contract (`Delivery`, result values,
typed payloads); this ADR moves the loop itself into the framework.

## Decision

1. **Classification is pure** (worker, no feature). `RetryPolicy` (any backoff
   function; `linear` helper) and `HandlerRegistry::deliver` turn one attempt
   into `Outcome::Done(value)`, `Retry(delay)` or `DeadLetter { reason }`.
   `DeadReason` is the union of both apps: `NoHandler`, `InvalidPayload`,
   `Permanent`, `RetryExhausted`, `UnknownOutcome`, `Expired`, `Malformed`.
   `deliver` reads the clock eagerly, so its future is `Send` for any `Clock`.
2. **The loop is a feature** (`rustclamp-worker/service`). `WorkerService` runs
   a registry over a `Transport` (`claim(limit)`, `settle(receipt,
   Settlement)`). Transport methods and receipts stay on the service's own
   task, so they need not be `Send`. The service owns the following:
   - backpressure: claimed-and-unsettled messages stay within `concurrency + capacity`;
   - the concurrency limit;
   - in-process retries under the policy;
   - dead-lettering at claim for unroutable and malformed items (0 attempts);
   - an optional per-attempt timeout, recorded as `UnknownOutcome`;
   - on shutdown: waiting messages are released, running attempts drain until
     `drain_timeout`, then a root `CancellationToken` cancels them, and they
     are released without counting the attempt.

   The app observes the loop through `on_event(ServiceEvent)`, `stats()` and
   the returned `ServiceReport { returned, cancelled, drained }`. Transports
   keep ack, nack and dead-letter *storage*. The service decides only *which*
   of those happens.
3. **Scheduler driver** (`rustclamp-scheduler/tokio`).
   `Scheduler::run_until(clock, resolution, stop)` replaces hand-written tick
   loops.
4. **Supporting changes.**
   - `rustclamp_core::SystemClock`.
   - `runtime::tokio_runtime::shutdown_signal()` installs its SIGINT and SIGTERM
     handlers when called. A signal before the first poll no longer kills the
     process.

## Boundaries

New edges, all optional: worker → runtime and Tokio (feature `service`), and
scheduler → Tokio (feature `tokio`). Both are recorded in `ALLOWED` and in
`OPTIONAL`. The base crates stay Tokio-free. Tests use the optional Tokio
through the feature on a current-thread runtime, so Tokio is never a
non-optional dev-dependency.

## Consequences

- One tested implementation of the loop instead of one per app. Example 08's
  classification became a policy plus a mapping to its own strings.
- Retries happen in-process: the message stays claimed while backing off. A
  broker that must own redelivery (NATS `Nak(delay)`) keeps using `deliver()`
  directly without the service. This is still supported.
- Breaking: `DeadReason` gained `Malformed`, so exhaustive matches must add it.
- Rust code, dependencies, runtime and CPU before and after rewriting the job
  runner on the service: see `test/jobrunner/RESULTS.md`.
