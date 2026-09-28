# ADR 0004: Additive module declarations

Status: Prototype decision, 2026-09-28. Revisit after process composition is
implemented.

## Context

Phase 2 needs named module declarations for typed capabilities without making
every module implement unrelated lifecycle hooks. Two viable declaration styles
exist: implement additive Core traits on reusable module types, or declare
module IDs and typed provisions/requirements at the composition call site.

## Decision

Keep both styles. Use `Module`, `Requires<C>`, and `Provides<C>` for reusable,
named module contracts. Keep `CapabilityRequirement::new` and
`Provision::new` as first-class explicit declarations for dynamic or local
composition. Do not add another builder type yet; it would duplicate the
constructors without solving a demonstrated use case.

The traits are additive and capability-scoped. A module implements only the
contracts it participates in; neither Core nor Kernel introduces a universal
construction or lifecycle trait.

```rust
impl Module for ClockModule {
    const ID: ModuleId = ModuleId::new("app.clock");
}

impl Provides<ClockCapability> for ClockModule {
    fn provided_value(&self) -> &(dyn Clock + 'static) {
        self
    }
}

impl Module for GreeterModule {
    const ID: ModuleId = ModuleId::new("app.greeter");
}

impl Requires<ClockCapability> for GreeterModule {}

let provision = Provision::<ClockCapability>::from_module(&clock);
let requirement = CapabilityRequirement::<ClockCapability>::from_module::<GreeterModule>();
```

The explicit alternative remains available without implementing traits:

```rust
let provision = Provision::<ClockCapability>::new(
    ModuleId::new("app.clock"),
    &clock as &dyn Clock,
);
let requirement = CapabilityRequirement::<ClockCapability>::new(
    ModuleId::new("app.greeter"),
    None,
);
```

## Comparison

| Concern | Additive traits | Explicit declarations |
| --- | --- | --- |
| Capability typing | `Provides<C>` and `Requires<C>` bind a module type to `C`; provision values remain checked by `C::Value`. | `Provision<C>` and `CapabilityRequirement<C>` are also typed by `C`; module IDs are supplied independently. |
| Provider selection | A selected `ModuleId` is explicit, but Rust does not prove that the selected module implements `Provides<C>`; composition still validates it. | The selected ID is likewise validated during composition. |
| Diagnostics | Calling `from_module` without the required implementation gives compiler error E0277 at that call, naming the missing `Requires<C>` bound. Missing and ambiguous providers remain structured runtime `CompositionError`s. | Constructor argument type errors are local and direct. A misspelled or absent module identity is not a Rust type error; graph validation reports it. |
| IDE discovery | Named impls support go-to-implementation and trait-implementation search. | Wiring is visible at the composition site and easy to scan there. |
| Flexibility | Best for reusable static module types; trait associated constants do not describe runtime-conditional requirements. | Best for local, generated, or runtime-selected declarations. |

IDE behavior is a qualitative API assessment; no editor-specific benchmark was
run. Neither style makes provider selection fully compile-time: stable semantic
IDs and composition-time validation remain necessary for this prototype.

## Compile-Cost Experiment

Two minimal consumer crates used Rust 1.96.1 (`x86_64-unknown-linux-gnu`) and
depended on the same local Core and Kernel packages. One declared the
Clock/Greeter relation through the traits; the control used `Provision::new` and
`CapabilityRequirement::new`. Both called the same resolver. Five pairs of
incremental `cargo check --offline` runs alternated order in one shared target
directory after dependencies had compiled.

| Measurement | Trait fixture | Explicit fixture |
| --- | ---: | ---: |
| Median Cargo check time, 5 runs | 0.04 s | 0.04 s |
| Median wall time, 5 runs | 0.076 s | 0.074 s |
| `.rmeta` size for fixture crate | 7,339 B | 5,152 B |

Wall-time samples, in milliseconds: traits `[76, 82, 74, 77, 73]`; explicit
`[76, 73, 74, 71, 86]`. Cargo's own check-time output rounded all ten samples to
0.03 or 0.04 s.

The 2 ms wall-time difference is below useful attribution for this small check;
no compile-time difference was established. The trait fixture metadata is 2,187 B
larger, but this is not linked binary size and is not representative of a larger
consumer. The temporary fixture source was outside the repositories and was not
retained as a second implementation.

## Consequences

- Reusable modules can advertise capability participation once and reuse it.
- Simple compositions retain the direct constructor path and do not need trait
  implementations or a separate builder abstraction.
- Selected provider IDs and cross-module validity remain composition-time checks.
- Reconsider a builder only when a real composition API needs dynamic additions,
  removal, or grouped validation not served by these declarations.
