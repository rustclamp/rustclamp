# Prototype measurement protocol

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
