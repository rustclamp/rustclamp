# Phase 8 Progress: Deterministic Loops and Device Proofs

## Implemented slice

`examples/11-device-loop` demonstrates explicit target-owned integer steps,
injected simulation time, seeded randomness, fixed inputs, and replay of the
same fixture with equal state traces. Integer millimeter and milli-Celsius
units keep this small model deterministic without adding a math dependency.
The evidence makes no promise about cross-platform floating-point replay.

The fake device validates raw temperatures at ingestion, reports missing and
stale samples, detects disconnection, and queues at most eight inputs. A tick
consumes one sample, updates the fake display, and optionally emits telemetry.
The CAN contribution target sorts its runtime decoder table and rejects
duplicate frame IDs. A two-process blueprint keeps the direct simulation
projection separate from a service projection; an opt-in Tokio service test
proves cancellation without enabling Tokio in the default device graph.

## Checks

From the isolated example manifest:

- `cargo fmt --manifest-path examples/11-device-loop/Cargo.toml`
- `cargo clippy --offline --locked --manifest-path examples/11-device-loop/Cargo.toml --all-targets -- -D warnings`
- `cargo test --offline --locked --manifest-path examples/11-device-loop/Cargo.toml`: 6 passed
- `cargo clippy --offline --locked --manifest-path examples/11-device-loop/Cargo.toml --all-targets --all-features -- -D warnings`
- `cargo test --offline --locked --manifest-path examples/11-device-loop/Cargo.toml --all-features`: 7 passed
- The demo prints the same three-step integer trajectory on each run.

The package's default dependency closure consists of Core, Kernel, and Runtime;
Tokio is activated only by `tokio-service`. It uses no HTTP, database, or
filesystem package. The example still uses the standard library and is not a
`no_std` claim.

## Remaining work

The complete Process-owned loop/resource lifecycle and managed-participant
shutdown proof remain open (P8-01's runtime integration and P8-07). The
blueprint currently proves process projection separation, while the optional
async test independently proves Runtime cancellation; fuller service/loop
assembly is still needed for P8-08. The fake device proof still needs an
explicit platform-capability composition diagnostic and a completed malformed
CAN, managed shutdown, and stream recovery matrix (P8-09, P8-13, P8-14).

No `no_std` foundation subset has been demonstrated. Cross-domain review,
platform-neutral external consumer, absence matrix, 1/5/20/50/100/500 scale
benchmark, and direct-Rust footprint comparison are not yet complete
(P8-16, P8-18 through P8-23). No architecture correction was needed in this
slice.
