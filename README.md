# rustclamp

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
