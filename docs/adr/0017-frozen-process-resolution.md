# ADR 0017: Resolve capability values through the frozen process

Status: Accepted, 2026-09-30. Amends ADR 0004 ("do not add another builder
type yet").

## Context

The `uptime-demo` stress test (`~/projects/rustclamp/test/`, a one-shot CLI
built once on RustClamp and once in plain Rust against the same spec) wired one
value through the framework. It needed ~70 lines in `app.rs`, and the same edge
was declared twice: once in `ApplicationBlueprint::require_provider` and again
by hand in a `CapabilityComposition` (provisions + requirements). Nothing kept
the two in sync; `freeze` only fed a `debug_assert!`. `ProjectionError` had no
`Display`/`Error`, so apps printed `{:?}`. The runtime's `tokio` feature always
enabled Tokio `signal`, adding `signal-hook-registry` and `errno` to apps that
never wait for Ctrl-C.

This is the demonstrated need ADR 0004 asked for before revisiting.

## Decision

- `FrozenProcess::resolve::<C>(consumer, &[Provision<C>])` finds the consumer's
  requirement in the frozen projection and resolves it with the provider chosen
  at freeze time. The blueprint is the single declaration; the caller only
  supplies constructed values. Undeclared/inactive requirements fail with the new
  `CompositionErrorKind::UndeclaredRequirement`; a selected provider absent from
  the provisions fails with `ProviderSelectionUnavailable`. Unqualified only
  until a process needs a qualified variant.
- `ApplicationBlueprint::add_root(process, execution, module)` declares a
  single-root process (module + execution + process) in one call.
- `ProjectionError` and `TargetCompositionError<E>` implement `Display` and
  `Error`, so composition failures propagate with `?` into `Box<dyn Error>`.
- `rustclamp-runtime` gains a `signal` feature (`tokio` + `tokio/signal`);
  `wait_for_ctrl_c` and `ShutdownSignal` require it.

No new builder type, trait, or macro. `CapabilityComposition` and `Resolver`
stay for compositions not expressed as a blueprint.

## Consequences

Measured on the demo (same tests pass, plain-Rust suite passes against it too):

| | before | after |
|---|---|---|
| `src/app.rs` | ~80 lines (approx.), 3 marker types, 5 trait impls | 45 lines, 1 capability impl |
| Rust code in `src/` | 255 | 223 (plain Rust: 197) |
| Crates in dependency tree | 119 | 117 (plain Rust: 114) |

Part of that reduction is the explicit declaration style ADR 0004 already
allowed (dropping `Module`/`Provides`/`Requires` impls and marker types). This
ADR accounts for removing the hand-built `CapabilityComposition`, the
freeze-to-`debug_assert!` glue, `{:?}` error mapping, the separate
execution/process declarations, and two signal crates.

- Breaking: users of `TokioRuntime::wait_for_ctrl_c` must enable `signal`. No
  in-repo caller exists.
- Breaking: exhaustive matches on `CompositionErrorKind` need the new variant.
- Remaining ceremony is stable IDs (application, process, execution, modules);
  that is the framework's model, not boilerplate to remove here.
