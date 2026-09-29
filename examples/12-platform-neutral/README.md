# Platform-neutral Core consumer

This `#![no_std]` library uses the alloc collection type and one Core module
identity while retaining application state through an ordinary Rust-owned
`Vec`. It has no Kernel or Runtime dependency. The host build demonstrates
consumer-side `no_std + alloc` source compatibility; it does not demonstrate
that the current Core package itself can build on a bare-metal target. Core's
`Clock` currently exposes `std::time::SystemTime`, which remains a target
portability boundary.
