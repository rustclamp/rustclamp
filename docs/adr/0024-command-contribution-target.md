# ADR 0024: Command contribution target

Status: Accepted, 2026-10-07 (web commands); the kernel-level target below
proposed with answers, 2026-10-21 (rustclamp/rustclamp#141).

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

## Decision (2026-10-07)

Web apps get console commands now, without `clap` and without a new
dependency (rustclamp/rustclamp#122):

- `rustclamp::web::Command` is a name, a one-line description and a handler
  `fn(&[String], &Config, &Db) -> i32` that gets the arguments after the
  name and returns the exit status.
- `App::commands` lists them (`&'static [Command]`). `cargo run -- NAME ARGS`
  runs one; `cargo run -- list` (or `help`) prints the built-in commands
  (`migrate`, `migrate:status`, `migrate:rollback`, `db:seed`, `user:create`)
  and the app's, with descriptions. An unknown name prints a hint and exits
  with 2. An app command that takes a built-in name, or a name another app
  command has, panics on the first run.
- The web template keeps them in `app/console/`; `clamp make:command NAME`
  writes one and adds it to the module map in `app/lib.rs`.

The kernel-level `CliCommandTarget` with `clap` below was deferred to
rustclamp/rustclamp#141; see "Decision (2026-10-21)" for the answers to its
open questions.

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

## Decision (2026-10-21)

Status: proposed until the maintainer confirms answers 1 and 3. Implemented
as `rustclamp::cli` (rustclamp/rustclamp#141).

1. **Placement: the facade, `rustclamp::cli` behind feature `cli`.** The
   target implements `rustclamp_core::ContributionTarget` and needs nothing
   from the kernel: the caller builds it with `TargetComposition::build` or
   `FrozenProcess::compose`, which already drops contributions from modules
   outside the process. So the only new edges are facade -> `clap` and the
   existing optional facade -> core, which `tools/boundaries.py` already
   allows. Kernel is a path dev-dependency for the projection test only. A
   `rustclamp-cli` crate would add a repository, a release and a publish
   step (the scheduler crate is still unpublished, #84) for one module.
2. **Handler: the web `Command` shape.** `CommandDeclaration::new(name,
   description, run)` with `run: Fn(&ArgMatches) -> i32 + Send + Sync`,
   stored behind an `Arc` as the scheduler stores its job handlers. Context
   comes from what the closure captures, so there is no context type
   parameter. The exit status is the handler's return, as in
   `web::Command`; `CommandTree::run` returns `Result<i32, clap::Error>` and
   prints nothing (item 5). `.arg(..)` adds `clap` arguments; typed argument
   structs (`FromArgMatches`) stay available to an app that adds `clap` with
   `derive` itself.
3. **`clap` feature set: the minimum that still lists commands.**
   `default-features = false, features = ["std", "help", "usage",
   "error-context"]`: help and usage text with each command's one-line
   description, and errors that say what went wrong. No `color` and no
   `suggestions`. The closure is `clap`, `clap_builder`, `clap_lex` and
   `anstyle`, added to `ALLOWED["rustclamp"]`, with `clap` in `OPTIONAL`.
4. **Example 03 (`examples/a3-contribution`) is not ported.** It stays the
   dependency-free Core + Kernel prototype its Phase 3 measurements
   describe; `rustclamp::cli` is its production form. `tests/cli.rs` proves
   the target through a frozen process projection.

Not done here: a contributed command named `help` collides with `clap`'s
`help` subcommand and panics in debug builds when parsed; nested groups,
completion and async handlers stay undecided.

## Open questions (2026-10-07)

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
