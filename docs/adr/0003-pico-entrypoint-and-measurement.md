# ADR 0003: Pico calls the closure directly

Status: Adopted, 2026-09-28.

## Decision

`Clamp::run<F, R>(run: F) -> R` accepts `F: FnOnce() -> R` and calls it directly.
The prelude exports only `Clamp`. The closure runs on the caller's thread and may
borrow data or consume owned captures. There are no `Send` or `'static` bounds.
Return values, including errors, pass through unchanged. Panics follow Rust's
normal panic strategy; this entrypoint does not catch or translate them. A future
returned by the closure remains a value for the caller to handle.

Pico needs neither Core nor Kernel nor Runtime. The facade manifest has no
dependencies. There is no composition or lifecycle work to perform, no registry
or state to retain, no thread to start, and no allocation in the implementation.
Later composition entrypoints must preserve this minimal closure path.

~~~mermaid
flowchart LR
    Caller[Caller thread] --> Once[Invoke FnOnce once]
    Once --> Result[Return R unchanged]
    Once -. no framework composition .-> Done[Return to caller]
~~~

| Property | Plain Rust | Pico |
| --- | --- | --- |
| Resolved packages | Consumer only | Consumer and facade |
| Facade dependencies | — | 0 |
| Entrypoint allocation observation | 0 | 0 |
| Binary size, recorded profile | 4,335,592 B | 4,335,592 B |
| Error and panic behavior | Native Rust | Preserved from closure |

The size and allocation equality apply to the measured target and harness.
Compiler cost differs, and process timing includes launch/output/exit. Full paired
samples and limitations are in [Phase 1 evidence](../evidence/phase1.md).

`examples/00-pico/main.rs` is an explicit Cargo example. The plain control has
identical output in `benchmarks/fixtures/plain.rs`. The cost harness builds each
as an isolated application with identical package names, profile settings and
compiler flags. Its Pico graph must contain exactly the consumer and the facade,
with no activated features; the plain graph contains only its consumer.

## Allocation instrumentation boundary

All framework libraries continue to forbid unsafe code. The standalone allocation
measurement binary in `benchmarks/fixtures/allocations.rs` implements a counting
`GlobalAlloc` adapter. Its unsafe code is confined to that tool, never linked into
the framework, Pico or the plain timing/size binaries. `tests/pico.rs` builds the
fixture as a temporary Cargo binary and executes it in debug and release profiles.
The same fixture runs with and without the facade dependency.

Safety review: all allocation methods forward their pointers, layouts and sizes
unchanged to `System`. The counters use atomic operations that do not allocate or
panic. Deallocation pairs with the same allocator and layout. The calibration
handles allocation failure, reads only within allocated memory, and pairs a resized
block with its new layout. `unsafe_op_in_unsafe_fn` is denied. The harness has a
user `Box` positive control and directly exercises zeroed allocation, reallocation
and deallocation. The direct calibration calls avoid the compiler eliminating an
otherwise unused allocation. No safety property relies on a measured count.

This is a narrow measurement-tool exception to the package unsafe policy, justified
by the explicit requirement to measure allocations without adding a framework
dependency. It introduces no allocator implementation beyond forwarding to System.

Allocator counts are observations of the compiled binary, not semantic guarantees
about all optimized Rust programs. The first calibration using ordinary allocation
functions was optimized away in release mode; the corrected positive controls make
that measurement limitation explicit. Stdout and user allocations are measured
separately from the entrypoint-only interval.

## References

- [FnOnce](https://doc.rust-lang.org/std/ops/trait.FnOnce.html)
- [GlobalAlloc safety and optimization limits](https://doc.rust-lang.org/std/alloc/trait.GlobalAlloc.html)
- [Phase 1 evidence](../evidence/phase1.md)
