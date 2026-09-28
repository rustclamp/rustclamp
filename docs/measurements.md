# Prototype measurement protocol

The protocol compares an implementation with a control under matched conditions.
It is intended to expose costs and limits, not to manufacture a single
framework-performance score.

The current typed capability-resolution comparison is recorded in
[Phase 2 evidence](evidence/phase2.md), with raw resolver samples and build data.

```mermaid
flowchart LR
    Define[Define equivalent workloads] --> Isolate[Isolate consumers]
    Isolate --> Match[Match toolchain and flags]
    Match --> Alternate[Alternate paired runs]
    Alternate --> Preserve[Preserve raw samples and environment]
    Preserve --> Interpret[Interpret with limitations]
```

For the runnable Pico/plain comparison, use:

```sh
python3 rustclamp/tools/pico.py --measure rustclamp/docs/evidence/reports/phase1-pico.json
```

This builds isolated consumers with matched flags, alternates sample order,
records clean/unchanged/edited builds and process timings, and runs a separate
allocation probe with positive controls. It checks exact dependencies and output
through `cargo test` as well. See [Phase 1 results](evidence/phase1.md) for the
initial cost budget and [ADR 0003](adr/0003-pico-entrypoint-and-measurement.md) for
the instrumentation safety boundary. `tools/measure.py` remains the generic
Phase 0 library-build recording tool.

Run from the coordinated checkout with its pinned toolchain:

```sh
python3 rustclamp/tools/measure.py --manifest Cargo.toml --package rustclamp \
  --repetitions 3 --output rustclamp/docs/evidence/reports/phase0-facade.json
```

The script emits versioned JSON with compiler details, Cargo version, target,
machine/CPU/memory, source fingerprint, build commands and environment, profile,
features, cache conditions, repetition count and raw timings. It uses a new
temporary target directory per clean build, enables incremental compilation and
immediately repeats the unchanged build. OS page caches are uncontrolled. Disable
compiler caches for comparisons and document any unavoidable wrappers.

The initial release profile is explicit in the coordination generator: opt-level
3, debug false, strip none, LTO false, 16 codegen units and panic unwind. Preserve
the manifest, lockfile, relevant Cargo config and exact source revision with a
report. Record load on the machine and external compiler/linker flags when comparing
results. Absolute paths in reports identify where commands ran; package manifests
and checkout instructions must remain portable.

## Minimum results for every runnable prototype

| Metric | Required evidence |
| --- | --- |
| Dependency count | Resolved transitive packages and features for an isolated consumer; exclude the consumer itself |
| Clean compilation | Raw samples and median using fresh build artifacts; disclose download/compiler/OS caches |
| Incremental compilation | Unchanged rebuild and, when useful, a specified source edit; distinguish the two |
| Binary size | Bytes, debug symbols and stripping policy; match flags between comparisons |
| Startup | Process wall time plus observed initialization/background activity; do not equate subprocess timing with pure initialization |
| Allocations | Counts around the relevant entrypoint, separating framework work from stdout and user code |

Use `--example NAME` after adding an executable Cargo example. It adds executable
size and process wall measurements (including spawn, output redirected to a null
sink, and exit). Use the same payload, output and flags for Pico and plain Rust.
Run isolated consumer measurements as well as any workspace measurements to avoid
feature unification hiding dependencies. Use enough repetitions for the comparison,
and preserve raw samples rather than claiming precision from a single run.

Allocation counts require an instrumented harness in Phase 1. Record runtime RSS,
CPU use, sustained workload and compiler-detail measurements when relevant to the
prototype. Missing measurements use JSON `null` with an explanation, never zero.
Phase 0 has library scaffolds only, so it cannot establish binary, startup or
allocation overhead. Its report verifies the recording procedure.

## Comparison Rules

| Comparison | Hold constant | Report separately |
| --- | --- | --- |
| Pico vs plain Rust | Output, toolchain, profile, flags, host | Dependency graph, clean/edit/unchanged builds, binary size, process wall time, entrypoint allocations |
| Capability resolution vs direct typed access | Capability value, toolchain, profile, host | Unique resolution and explicit-selection cost at increasing candidate counts; structured failure paths separately |
| Combined vs isolated package | Package source and toolchain | Workspace feature unification and parent-manifest effects |
| Runtime adapter vs direct runtime use | Workload, concurrency, runtime configuration | Framework coordination work, task/thread count, readiness and shutdown time |
| Contribution assembly vs direct structure | Resulting routes/tree/table and conflict rules | Composition cost, retained runtime structure, target-owned validation |
| Simulation vs direct loop | Inputs, clock, random seed, update order | Repeatability, throughput, allocations, compile and binary cost |

Interpret paired medians with their raw sample range and machine conditions.
Do not infer a causal runtime penalty from Cargo build time, or initialization
cost from process timing that includes spawn and I/O. A missing measurement is
unknown; it is not zero. Set regression budgets from repeated evidence on a
controlled host, then keep platform comparisons advisory unless that host and
toolchain are part of the supported target.
