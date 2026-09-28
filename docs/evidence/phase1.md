# Phase 1: Pico and plain Rust

Date: 2026-09-28. Local CI-equivalent checks pass. Source snapshots are published
to the four package repositories. The CI workflow files remain local because the
GitHub token lacks the `workflow` scope. No hosted checks have run; publish the
workflow files after refreshing credentials for that scope.

## Finding

Pico runs with only the facade. Core, Kernel and Runtime remain outside its graph.
`Clamp::run` calls an `FnOnce` synchronously and returns its result unchanged.
The prelude exports just `Clamp`. There are no global framework values, allocation
sites, registries, executor, threads, lifecycle hooks, or composition objects in
this implementation. Adding a dependency to route this closure through Kernel
would have no useful work to perform. See [ADR 0003](../adr/0003-pico-entrypoint-and-measurement.md).

```sh
cargo run --offline --locked -p rustclamp --example 00-pico
# Hello Clamp
python3 rustclamp/tools/pico.py --check
python3 rustclamp/tools/pico.py --measure rustclamp/docs/evidence/reports/phase1-pico.json
python3 rustclamp/tools/check.py
```

Commands above run from the ecosystem root. In a standalone facade checkout,
use `python3 tools/pico.py ...` and omit the `rustclamp/` prefix on output paths.
The example is an actual Cargo target at `examples/00-pico/main.rs`.

## Measured costs

Raw samples and reproduction inputs: [phase1-pico.json](reports/phase1-pico.json).

Environment: Rust/Cargo 1.96.1, x86_64-unknown-linux-gnu, AMD Ryzen 5 PRO 4650U,
Linux. Both applications use edition 2024 and identical release settings:
opt-level 3, debug false, strip none, LTO false, 16 codegen units, panic unwind,
and incremental compilation enabled. No features are activated. Compiler wrappers
and additional Rust flags are disabled. Each application is built in its own
temporary Cargo workspace; the facade is copied without its ecosystem siblings.

| Metric | Plain Rust | Pico | Pico minus plain |
| --- | ---: | ---: | ---: |
| Dependency packages, excluding application | 0 | 1 (facade) | +1 |
| Facade's dependencies | — | 0 | — |
| Clean build median | 232.40 ms | 269.18 ms | +36.78 ms |
| Unchanged rebuild median | 133.48 ms | 135.15 ms | +1.67 ms |
| Comment-edit rebuild median | 217.30 ms | 228.16 ms | +10.86 ms |
| Unstripped binary bytes | 4,335,592 | 4,335,592 | 0 |
| Process wall-time median | 1.203 ms | 1.256 ms | +0.054 ms |
| Empty entrypoint allocations | 0 | 0 | 0 |
| First stdout call allocations | 1 | 1 | 0 |
| User `Box` allocation control | 1 | 1 | 0 |

Build medians use five repetitions per application, with fresh target directories,
then an unchanged rebuild and a comment-only source edit. The paired order alternates
between plain-first and Pico-first. There are 100 process repetitions per application
after five warmups, also alternating order. Output is redirected to the same null
sink. Correct output is checked separately with captured stdout.

Process samples overlap substantially: plain 0.899–2.292 ms, Pico 0.898–2.257 ms.
These include process creation, stdout and exit. The small median difference does
not establish a reliable initialization penalty. Compiler startup and Cargo work
are included in build timings. OS caches and concurrent machine load are uncontrolled;
the recorded load average and raw samples bound how much to infer from this run.

Allocation counts are equal in debug and release. The measurement binary observes
calls to Rust's global allocator; it separates the empty closure, stdout and user
work. Positive controls detect an allocating `Box`, zeroed allocation and
reallocation. The calibration initially used allocations the release compiler
optimized away; direct adapter calls and opaque pointers now verify instrumentation.
This does not claim an optimizer-independent count of all potential allocations.
The allocator adapter is confined to a standalone test tool and does not affect
the timed or sized applications.

## Initial cost budget

- Keep Pico's resolved graph at exactly application → facade, with zero facade
  dependencies and no activated features. `tests/pico.rs` enforces this for an
  isolated consumer, including exact output and successful exit.
- Keep entrypoint-only allocations at zero and stdout allocations equal to the
  plain control. Both debug and release instrumentation enforce these checks.
- The observed binary-size delta is zero. Preserve that budget on this recorded
  target/profile; investigate any increase with equivalent builds before accepting
  it. This is a comparison budget, not an absolute cross-platform binary-size cap.
- The observed clean-build median delta is +36.78 ms (about 16%). It is the initial
  review baseline. No hard timing gate is justified by five build samples and a
  shared machine. Any additional package, sustained timing increase, or background
  work requires fresh paired measurements and an explicit explanation.

There is no runtime RSS/CPU workload study: these programs only print a line and
exit. Such measurements belong with later sustained workloads. No universal
“zero overhead” claim follows from equal binary size or this startup sample.

## API and failure-path checks

The integration tests prove that the closure runs once on the caller's thread,
accepts owned non-`Send` captures, borrows mutable local data, returns a borrow,
and preserves a structured error value. Under the test profile's unwind strategy,
the panic payload propagates unchanged and the captured drop guard runs once.
The crate doctest exercises the public prelude and return value.

`tests/pico.rs` is a real Cargo integration target. It builds the isolated Pico,
plain control and allocation fixture, checks metadata and output, and runs the
allocation probes in both profiles. The allocation fixture receives explicit
rustfmt and Clippy checks. Formatting, Clippy, tests, rustdoc, independent-package
checks, relative consumers and archive verification run through the existing
Phase 0 tooling. Hosted results remain pending while the token lacks the `workflow`
scope.

Capability resolution remains Phase 2 work. No additional framework package,
integration or composition subsystem was introduced.
