# ADR 0024: Command contribution target

Status: Proposed, 2026-09-30.

## Context

There is no CLI target in the framework. Two apps built on RustClamp wrote
their own:

- Level 07 (todo CLI) copied `CommandDeclaration` and `CliCommandTarget` out
  of `examples/a3-contribution`.
- Level 08 (system monitor) turned two process roots into a `clap` `match` by
  hand, so the process projection selected the modules but not the commands.

`examples/a3-contribution` already proves the shape: a module contributes a
command declaration under a qualifier, and a target validates the declarations
(duplicate names), sorts them and compiles them into a `CommandTree`. What it
is not: a package. Its handler is `fn(Option<&dyn Clock>)`, which only fits
that example, and it has no argument parsing.

## Proposal

A real `CliCommandTarget` that a kernel `TargetComposition` builds from
module contributions, and that dispatches parsed `clap` subcommands to
handlers.

1. **Declaration.** `CommandDeclaration<Q>` carries a name, a
   `clap::Command` for that subcommand (its arguments and help), and a
   handler `Fn(&ArgMatches) -> Result<Output, E>`. The qualifier `Q` keeps
   public and admin trees apart, as in the example.
2. **Target.** `CliCommandTarget<Q>` implements `ContributionTarget`. `build`
   rejects duplicate names with the same structured error as the example
   (target, name, both contributors) and produces a `CommandTree`.
3. **Tree.** `CommandTree::command()` returns the root `clap::Command` with
   every subcommand attached, in name order. `CommandTree::run(args)` parses,
   finds the handler by subcommand name and calls it. An unknown or missing
   subcommand is a `clap` usage error, not a panic.
4. **Process projection.** A process projection already lists the
   contributions consumed by the selected modules
   (`ProcessProjection::contributions`). The CLI process builds its
   `TargetComposition` only from those, so a worker process never carries the
   commands of a CLI-only module. This replaces level 08's hand-written
   `match` over roots.
5. **Errors.** Handler errors and `clap` errors are returned, never printed
   by the target. Mapping them to exit codes belongs to the error model
   (rustclamp/rustclamp#44).

## Boundaries

- `clap` is not a dependency of any RustClamp crate today. It would be an
  optional dependency behind a feature, so Pico (ADR 0003) and default builds
  stay dependency-free. The feature enables `clap` with
  `default-features = false, features = ["std"]` plus whichever of `help`,
  `usage`, `error-context` we choose to keep (open question 3).
- The transitive closure of `clap` must be added to `ALLOWED` and `OPTIONAL`
  in `tools/boundaries.py`, as ADR 0020 did for Tokio, and `Cargo.lock`
  updated.
- The target needs `rustclamp-kernel` (`TargetComposition`) and
  `rustclamp-core` (`ContributionTarget`). The facade depends on neither
  today except core under `uuid`. Where the target lives decides which edge
  is new (open question 1).

## Open questions

1. **Placement.** Facade (`rustclamp::cli`, feature `cli`, new optional
   facade -> kernel edge) or a new package crate `rustclamp-cli` that depends
   on core, kernel and `clap`? The second keeps the facade edge-free; the
   first matches where the web target lives.
2. **Handler signature.** `Fn(&ArgMatches) -> Result<Output, E>` is generic
   over `E` only through boxing. Alternatives: a typed argument struct via
   `clap::Parser`/`FromArgMatches`, or an injected context (the example's
   `Option<&dyn Clock>`). Which one is the default?
3. **`clap` feature set.** Full defaults (colour, suggestions) versus the
   minimum. Affects size and the boundaries list.
4. **Example 03.** It documents "Core and Kernel; facade absent", backed by
   Phase 3 measurements. Porting it changes those numbers and adds a
   dependency, so it stays as the dependency-free prototype until this ADR is
   accepted.

## Consequences

- Levels 07 and 08 drop their local copies; commands follow the process
  projection instead of a hand-written `match`.
- One place owns duplicate-name detection and command ordering.
- Not decided here: nested subcommand groups, shell completion, and
  async handlers.
