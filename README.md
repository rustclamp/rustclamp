<img src="https://raw.githubusercontent.com/rustclamp/docs.rustclamp.com/main/assets/rustclamp-logo.png" alt="RustClamp logo" width="160">

# RustClamp

**RustClamp is a composable application framework for Rust.** This is its main
framework repository. The `core`, `kernel`, and `runtime` repositories are planned
companion components, not separate products.

A small synchronous entrypoint for Clamp applications.

```rust
use rustclamp::prelude::*;

fn main() {
    Clamp::run(|| {
        println!("Hello Clamp");
    });
}
```

`Clamp::run` calls the closure once on the current thread and returns its result
unchanged. Captures may borrow local data or consume owned values. Errors and
panics keep ordinary Rust behavior. This path needs no Core, Kernel or Runtime.

Run Pico from this repository with `cargo run --offline --example 00-pico`.
Rust 1.96.1 is the tested minimum; the facade has no dependencies. Publishing is
disabled until licensing, registry ownership and the prototype API are reviewed.

See the [Pico results](docs/evidence/phase1.md) for measured costs and limitations.
The example and source are available in the `rustclamp` repository. Package checks
run in independent repositories and in the combined developer checkout.

## Project Map

| Repository | Responsibility | Status |
| --- | --- | --- |
| [`rustclamp`](https://github.com/rustclamp/rustclamp) | Main framework facade and application entry point | Pico is implemented and measured |
| [`core`](https://github.com/rustclamp/core) | Shared, domain-neutral contracts | `Clock` contract |
| [`kernel`](https://github.com/rustclamp/kernel) | Composition and resolution | Typed single-capability resolution; no full module composition |
| [`runtime`](https://github.com/rustclamp/runtime) | Execution-environment contracts | Scaffold; no public contracts |
| [`docs.rustclamp.com`](https://github.com/rustclamp/docs.rustclamp.com) | User and architecture documentation | Supporting repository, not a framework component |
| [`rustclamp.com`](https://github.com/rustclamp/rustclamp.com) | Project website | Supporting repository, not a framework component |

Today, the demonstrated facade path is `Clamp::run`: it runs a closure
synchronously on the current thread. Core defines a `Clock` contract, and Kernel
can resolve one typed requirement with explicit selection. Full module composition,
process projection, and runtime integration are not implemented. The target
architecture below is a design direction, not a working end-to-end pipeline.
“Application modules” means modules in a user's application, not these companion
repositories.

## Phase 1 and Phase 2 Measurements

Phase 1 measured the Pico facade against plain Rust. Phase 2 measured Kernel's
typed capability resolver against direct typed access. The workloads differ, so
the phase columns below are context, not a before/after speedup comparison.

| Metric | Phase 1: plain Rust | Phase 1: Pico | Phase 2: Kernel |
| --- | ---: | ---: | ---: |
| Clean-build median | 232.40 ms | 269.18 ms | 467.44 ms |
| Unchanged rebuild median | 133.48 ms | 135.15 ms | 40.48 ms |
| Process wall-time median | 1.203 ms | 1.256 ms | 1.453 ms |
| Release binary size | 4,335,592 B | 4,335,592 B | 4,357,888 B |
| Dependency packages | 0 | 1 facade | 1 internal (Core) |

Phase 1's paired comparison found Pico added 36.78 ms to the clean-build
median, 1.67 ms to the unchanged-build median, and 0 bytes to the binary. Phase
2's latest resolver microbenchmark measured direct typed access at 2 ns/op,
unqualified resolution at 8 ns/op (+6 ns), selection from two providers at 12
ns/op (+10 ns), and selection from eight at 29 ns/op (+27 ns). Typed `Primary`
qualification measured 6 ns/op with one provider and 27 ns/op when selecting
among eight. Optional absence/one-provider paths measured 6/3 ns; collecting
all providers measured 17 ns for two and 40 ns for eight. These are
nine-sample medians, not end-to-end application latency. Earlier Phase 2 runs
recorded unqualified 5/12/31 ns and then 7/11/27 ns. The resolver and harness
changed between snapshots, so these are not controlled estimates of individual
feature costs. The many-provider timing includes output-vector allocation.

The first Phase 2 build snapshot was 416.90 ms clean, 40.37 ms unchanged,
4,365,760 B, and 1.516 ms process wall time. The latest follow-up on the same
example is +50.53 ms clean, +0.12 ms unchanged, -7,872 B, and -0.063 ms process
time. This compares source versions on one uncontrolled host; it does not
isolate feature costs. The process measurement includes launch and output.

The build and process numbers between phases are not apples-to-apples: Phase 1
uses matched plain/Pico hello-world consumers, while Phase 2 builds/runs a
Kernel clock-resolution example with Core. The Phase 2 process measurement also
includes process launch, output, and exit. Do not interpret the cross-phase
difference as a regression or improvement. See [Phase 1 evidence](docs/evidence/phase1.md)
and [Phase 2 evidence](docs/evidence/phase2.md) for methods, limitations, and raw
samples.

## Target Architecture

The target model grows from selected process roots. Reachability determines
which modules, providers, contributions, configuration, and resources participate.
The Kernel coordinates the resolved projection; a selected runtime drives work.
Pico is the only end-to-end facade path. Capability resolution is currently a
focused Kernel prototype; later stages remain experimental.

```mermaid
flowchart LR
    App[Application] --> Bootstrap
    Bootstrap --> Modules
    Modules --> Capabilities
    Modules --> Contributions
    Capabilities --> Resolved[Resolved model]
    Contributions --> Resolved
    Resolved --> Process[Process projection]
    Process --> Kernel[Required Kernel mechanisms]
    Kernel --> Runtime[Selected runtime]
    Runtime --> Execution
```

## Pico Comparison

These results are a paired baseline on one recorded host, not a universal
performance guarantee.

| Metric | Plain Rust | Pico | Difference |
| --- | ---: | ---: | ---: |
| Resolved dependency packages | 0 | 1 facade | +1 |
| Facade dependencies | — | 0 | — |
| Clean-build median | 232.40 ms | 269.18 ms | +36.78 ms |
| Unchanged rebuild median | 133.48 ms | 135.15 ms | +1.67 ms |
| Release binary size | 4,335,592 B | 4,335,592 B | 0 B |
| Entrypoint allocations | 0 | 0 | 0 |

Build results use five paired repetitions and fresh target directories. Process
timings include process creation, stdout and exit, so the small median difference
does not establish a reliable initialization penalty. See the [full Phase 1
report](docs/evidence/phase1.md) and [raw measurements](docs/evidence/reports/phase1-pico.json).

```mermaid
flowchart LR
    Plain[Plain Rust control] --> Compare[Matched toolchain, flags, and output]
    Pico[Pico facade] --> Compare
    Compare --> Dependencies
    Compare --> BinarySize[Binary size]
    Compare --> Timing[Build and process timings]
    Compare --> Allocations
```

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets --all-features -- -D warnings
cargo test --offline --locked --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --offline --locked --no-deps --all-features
```

For coordinated checkout, architecture checks, measurements, and release policy,
see the [facade contributor guide](https://github.com/rustclamp/rustclamp/blob/main/CONTRIBUTING.md).
The configured remote is `https://github.com/rustclamp/rustclamp.git`; repository existence
and public visibility were verified during Phase 0.
