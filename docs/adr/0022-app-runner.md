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
5. **Failures are collected, not short-circuited.** A failing `drain` or `stop`
   is recorded and the sequence continues. `run` returns `Result<(), RunError>`
   where `RunError.failures` lists every `Failure { phase, name, message }` in
   order.
6. **One token.** `shutdown_token()` returns the token; cancelling it is
   equivalent to a signal.
7. **Signals (`signal` feature).** `run()` installs the SIGINT/SIGTERM and
   SIGHUP handlers before startup work, and runs `run_until(shutdown_signal)`.
   SIGHUP calls the optional `on_reload(FnMut())` hook and does not stop the app.
   `run_until(future)` is the signal-free form used by tests.

## Open questions

1. **Drain is unbounded for parts.** `Drain::drain` is synchronous and
   deadline-free, so the drain timeout only bounds services. Bounding a part's
   drain needs either the trait to take a deadline (core change) or the
   runner to run it on the blocking pool and abandon it on timeout (leaks a
   thread; needs `M: 'static` behind a mutex, which it already has).
2. **Should the traits be async?** Real init/stop (open a pool, flush a
   queue) is async. Today the runner calls the sync hooks inline on the runtime
   thread, which blocks it. Options: keep sync (init is cheap; wrap in
   `spawn_blocking`), add async twins in runtime, or change core. This ADR
   keeps them sync.
3. **Reload has no core trait.** `on_reload` is a plain closure. A `Reload`
   trait in core would let modules opt in like the others; not added
   speculatively.
4. **Error type.** Hooks render errors with `Display` into `String`, losing
   the source chain. Acceptable until an app needs to match on a cause.
5. **`Ready` semantics.** The core doc says "process policy decides global
   readiness". The runner treats any `ready` error as fatal. A degraded/optional
   policy (`FailurePolicy`) is not applied to parts.
6. **Service restart.** A service that exits early ends the app; the
   existing `Supervisor` restart policies are not used. Wrap the closure body
   in `supervise_async` if an app wants restart-once.
7. **Dependency order.** Order is registration order. Deriving it from
   `Requires`/`Provides` in the frozen process is a kernel concern and is left
   out.

## Consequences

- One tested implementation of the sequence and the unwinding instead of one per app.
- The stress-ladder's hand-written `join!` glue becomes `AppRunner` plus
  closures; the facade can adopt it without a new dependency direction (the
  runner takes closures, so scheduler/worker stay unknown to runtime).
- Core is untouched, so this ADR can be rejected or reshaped (open questions
  1-3) without a breaking release.
- Blocking hooks run on the runtime thread until question 2 is decided.
