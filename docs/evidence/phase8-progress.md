# Phase 8: Deterministic Loops and Device Proofs

## Deterministic and device proof

`examples/07-device-loop` demonstrates explicit target-owned integer steps,
injected simulation time, seeded randomness, fixed inputs, and replay of the
same fixture with equal state traces. Integer millimeter and milli-Celsius
units keep this small model deterministic without a math dependency. No
cross-platform floating-point replay is promised.

`LoopRuntime` consumes explicit input ticks under the existing runtime
`TaskContext`; it checks cancellation between ticks, advances only the
controlled clock, and stops in response to its owner. The simulation state is
borrowed from the caller. The fake device uses replaceable Sensor, Display,
Clock, and hardware-I/O contracts. Its owned sensor queue is bounded at eight,
shutdown clears managed input and rejects later ticks, and telemetry is an
optional effect.

The CAN contribution target sorts its runtime decoder table and rejects
duplicate frame IDs. Dispatch reports unknown IDs, malformed payload sizes,
and invalid sensor values as target-local errors. A two-process blueprint
separates the synchronous simulation projection and service projection. The
optional Tokio feature runs a cancellable async service; the deterministic
path and default graph do not enable Tokio.

## Checks

- Device loop default and all-feature Clippy passed with warnings denied.
- Device loop tests: 8 default, 9 with `tokio-service`; all passed.
- The composed and direct demos print the same three-step integer trajectory.
- The platform-neutral consumer passed Clippy and its incremental-adoption test.
- The full workspace runner passed 149 commands with 9 existing metadata
  warnings.

The device package's default dependency closure consists of Core, Kernel, and
Runtime; Tokio is activated only by `tokio-service`. The platform-neutral
consumer's closure contains Core only and excludes Kernel. It is a `#![no_std]`
consumer using `alloc::Vec` and an ordinary Rust-owned resource.

## Portability and architecture findings

The `no_std + alloc` consumer passes a host `cargo check`. A focused attempt to
build it for `x86_64-unknown-none` fails while compiling Core with E0463 because
that target's standard library is not installed. Only
`x86_64-unknown-linux-gnu` is installed in the validation environment. Core
also exposes `Clock` through `std::time::SystemTime`; no bare-metal Core support
is claimed. This is a compiler/target availability and API portability limit,
not evidence that an embedded target works.

Kernel contracts were unchanged in Phase 8. The server and daemon examples
continue to use their own request/service and lifecycle meanings; loop update
order, CAN identifiers, units, freshness rules, and decoder conflicts stay in
the device example. Existing Core module/capability contracts support this
without a universal `Operation` or `Execution` trait. The projection
benchmark retains named structured errors. Clippy flagged the raw decoder
function-pointer tuple as overly complex, so it is exposed as `DecoderTable`.

Dependency absence is checked in the automated runner: Pico has no Tokio; the
default loop excludes Tokio; the Core-only consumer excludes Kernel and
Runtime; lifecycle is opt-in to its example; and the manual lifecycle path
remains Tokio-free until its explicit adapter feature is enabled. No-hook
modules continue to implement only `Module`.

## Measurements

These local release-build observations are not performance promises. The
Kernel benchmark resolves one reachable chain through the selected number of
declared modules; composition construction is outside the timed region.

| Modules | Median projection time |
| ---: | ---: |
| 1 | 354 ns |
| 5 | 2,953 ns |
| 20 | 18,450 ns |
| 50 | 71,168 ns |
| 100 | 201,434 ns |
| 500 | 1,860,911 ns |

The 100,000-step direct Rust loop measured 0.627 ns/step, while the explicit
runtime loop measured 5.661 ns/step, a 5.034 ns/step difference in this fixture.
The runtime case includes the cancellation check, controlled-clock advance,
and iterator boundary. For the same release profile, the composed process
example binary was 669,872 bytes and the direct Rust example was 446,760 bytes
(223,112 bytes larger). The composed binary exercises Kernel process freezing
and the runtime loop. These values depend on this code, compiler, and host.

The benchmark matrix completed at 1/5/20/50/100/500 modules. Public Kernel
errors remain structured enums with named context. No type-complexity warning
remains in the measured public path. Review found no failing reproduction or
domain contract leak, so no architecture correction was needed.

## Optional MQTT decision

MQTT was not added. This proof is a fake device with no actual broker-connected
IoT use case; transport code would not strengthen deterministic device or
composition evidence. Telemetry remains a replaceable optional sink.
