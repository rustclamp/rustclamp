# Phase 9: RustClamp site server

This example serves the existing Python-built RustClamp site output through
Clamp's public HTTP contribution target. It keeps the generated HTML, assets,
canonical metadata, and 404 page unchanged so the Rust and Python servers can be
compared byte for byte.

From the coordinated checkout:

```sh
python3 rustclamp.com/build.py
cargo run --offline --manifest-path rustclamp/examples/08-site-server/Cargo.toml \
  --example 08-site-server -- rustclamp.com/public
```

The server listens at `http://127.0.0.1:8002`; the Python preview uses port 8000.
It is a hosting proof, not a Rust rewrite of the Python static-site generator.
The example uses synchronous filesystem reads for clarity and is not intended as
a production asset server.

## What can I do now?

- [Observability](../09-observability/README.md): metrics and tracing through one request
- [Existing Axum app](../02-axum-app/README.md): mount an existing Axum app
