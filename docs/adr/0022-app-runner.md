# ADR 0022: App runner — one driver for the lifecycle traits

Status: Proposed, 2026-09-30.

## Context

`rustclamp-core` defines five opt-in lifecycle traits: `Initialize`, `Start`,
`Ready`, `Drain`, `Stop`. Nothing drives them. The stress-ladder apps
(levels 03 and 08) hand-sequence startup, ready, run and stop, and glue the
signal, the services and the scheduler together with `tokio::join!` and a
`CancellationToken`. Every app re-derives the same ordering and the same
failure handling, and each gets a slightly different one.

The traits have these properties, which constrain any driver:

- They are **synchronous** and take `&mut self`.
- Each has its **own `Error` associated type**; nothing bounds it.
- They are **separate opt-ins**: a module may implement any subset, and
  Rust cannot ask "does this type implement `Drain`?" without specialization.
- `Drain` has **no deadline** and no way to observe one.
- There is **no reload trait**.
- Long-lived work (HTTP server, worker, scheduler loop) is **not a module
  lifecycle step**; it is an async loop that runs between `Ready` and `Drain`.

`rustclamp-runtime` already owns `CancellationToken`, `shutdown_signal()`,
`reload_signal()` and `wait_for_shutdown()`. The old facade branch
`phase5-lifecycle-driver` proved the sequencing with fake resources in a
synchronous example; it never reached a real service or signal.

## Decision

An `AppRunner` in `rustclamp-runtime`, behind the `tokio` feature (`signal`
adds signal joining). **No change to the core traits.**

1. **Participants are registered explicitly.** `Part::new(module)` wraps a
   module; `.initialize()`, `.start()`, `.ready()`, `.drain()`, `.stop()` each
   register that trait's method (bounded on the trait, and on
   `Error: Display`). A phase with no hook is skipped. This works around the
   "can't detect an opt-in" problem at the cost of one line per trait.
2. **Services are async closures.** `service(name, |token| async { .. })`
   returns `Result<(), String>`. The scheduler, the worker service and an HTTP
   server are all just services; the runner does not depend on any of those
   crates.
3. **Order.**
   - Startup, for parts in registration order, one phase at a time across
     all parts: `initialize`, then `start`, then `ready`.
   - Services are spawned after every part is ready and receive the one shutdown token.
   - Run until the shutdown future completes, the token is cancelled, or any
     service exits (an `Ok` exit counts: services are long-lived).
   - Shutdown: cancel the token (closes admission), join services within the
     **drain timeout** (default 30 s; stragglers are aborted and reported),
     then `drain` parts in reverse order, then `stop` parts in reverse order.
4. **Startup failure unwinds.** If any part fails `initialize`, `start` or
   `ready`, no service is spawned, `drain` is not run (nothing was admitted),
   and `stop` runs, in reverse order, on every part whose `initialize`
   **succeeded** — including the failing part when it failed in `start` or
   `ready`, since `initialize` acquired resources. A part whose own
   `initialize` failed is not stopped.
5. **Failures are collected, not short-circuited.** A failing or panicking
   `drain` or `stop` hook is recorded and the sequence continues (hooks run
   under `catch_unwind`; a panic becomes a `Failure`). `run` returns
   `Result<(), RunError>` where `RunError.failures` lists every
   `Failure { phase, name, message }` in order. Errors are rendered with
   `Display` into `String`, and `Phase`, `Failure` and `RunError` are
   `#[non_exhaustive]`, so a structured cause can be added later without a
   break.
6. **One token.** `shutdown_token()` returns the token; cancelling it is
   equivalent to a signal.
7. **Signals (`signal` feature).** `run()` installs the SIGINT/SIGTERM
   handlers before startup work and runs `run_until(shutdown_signal)`.
   `run_until(future)` is the signal-free form used by tests.
8. **Reload is a service, not a trait.** There is no `Reload` trait in core
   and no runner hook. The app calls `reload_signal()` before `run()` and
   moves the stream into a `service` that selects on `recv()` and
   `token.cancelled()`. This also keeps `signal` building off Unix.
9. **Lifecycle hooks stay synchronous.** Startup runs before any service, and
   `drain`/`stop` run after the services are joined, so nothing else needs the
   runtime thread while a hook blocks it. Async init/stop is not needed for
   this ADR; an app that needs it wraps the call in `block_on` or a service.
10. **`ready` failures stay fatal.** Any `ready` error unwinds startup. No
    degraded or optional policy is applied to parts.
11. **A service that exits early ends the app**, even with `Ok`. Restart is
    opt-in: wrap the closure body in `supervise_async`.
12. **Order is registration order.** Deriving it from `Requires`/`Provides` is
    left to the kernel.
13. **Aborted services are awaited.** When the drain timeout expires the
    stragglers are aborted, then joined for a short grace period (1 s) before
    parts drain and stop, so a service cannot still hold a part's handles when
    it stops. Each straggler is reported by name; a task that never yields
    cannot be aborted and is reported as not stopping.

## Sharing a module with services

`Part::new(module)` moves the module into the runner (behind a mutex). A
service that needs the same state (a pool, a queue handle) must get it from
the module itself: keep the shared state in an `Arc` field and clone the
`Arc` handle into the service closure **before** wrapping the module in a
`Part`.

## Open questions

1. **Drain is unbounded for parts. Deferred.** `Drain::drain` is synchronous
   and deadline-free, so the drain timeout only bounds services, and total
   shutdown time is `drain_timeout` plus the unbounded part drains. Abandoning
   a `spawn_blocking` drain on timeout is not an option: the abandoned closure
   still holds the module's mutex, so `stop` would block on it forever
   (deadlock). Bounding it properly needs the trait to take a deadline, a core
   change.

## Consequences

- One tested implementation of the sequence and the unwinding instead of one per app.
- The stress-ladder's hand-written `join!` glue becomes `AppRunner` plus
  closures; the facade can adopt it without a new dependency direction (the
  runner takes closures, so scheduler/worker stay unknown to runtime).
- Core is untouched, so this ADR can be rejected or reshaped (the open
  question) without a breaking release.
- Blocking hooks run on the runtime thread; this is safe because no service runs during them (Decision 9).
