# Deterministic loop and fake device

`Simulation::step` owns update order (position, input, random sample) and uses
integer units so replay is exact on supported Rust targets. The fixture runs
twice with the same seed and inputs and compares every state. This example
does not promise bit-identical floating-point results across platforms; replace
the integer model only with an explicitly chosen numeric determinism boundary.

`FakeSensor` validates raw input at ingestion, reports missing/stale samples,
and can be disconnected/recovered. `DeviceProcess::tick` performs bounded work
and emits to an optional telemetry sink. `CanTarget` compiles typed decoders
into a sorted runtime table and rejects duplicate CAN IDs. The loop/domain is
stdlib-only; the separate composition proof uses Kernel and runtime contracts.

Run with `cargo run --manifest-path examples/11-device-loop/Cargo.toml --example 11-device-loop`.
