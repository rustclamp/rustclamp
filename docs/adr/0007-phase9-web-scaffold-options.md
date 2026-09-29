# ADR 0007: Separate Rust init from frontend starter kits

Status: Accepted

## Context

`clamp init` creates RustClamp applications. Frontend stacks such as Vue and
Vite+ are reusable starter kits and have their own Node toolchain. Inertia is
optional server/client integration for server-routed applications; it is not
needed for a standalone Vue frontend or a RustClamp backend using ordinary
HTTP APIs.

The Vite+ project creator supports Vue templates, and its current guide uses
`vp create` for new projects. The first repository starter kit therefore uses
Vite+ with Vue 3 and TypeScript, separate from the Rust project generator.

## Decision

- Keep `clamp init NAME` as the small RustClamp Hello World.
- Add `clamp init NAME --template application` for a larger RustClamp project
  layout with an application composition module and sample health module.
- Add `clamp init NAME --web`: a RustClamp server with a route table and a
  Vite+ and Tailwind 4 frontend built into `public/build/`, in a Laravel-style layout (`app/`, `resources/`, `public/`, `tests/`). It needs Node/npm; `--blank`,
  `--app` and `--tui` stay Rust-only. `clamp dev` runs `vp build --watch` and
  `cargo run` together from `Procfile.dev`, so the Rust server is the only origin.
  The server itself lives in the facade as `rustclamp::web` (the optional,
  std-only `web` feature), so a generated app holds only `app/main.rs`, `app/lib.rs` and `app/routes.rs`.
- Keep frontend kits in `starter-kits/`, with independent dependencies and
  setup instructions. The initial kit is a browser-only Vue Todo app using
  Vite+.
- Evaluate Inertia as a separate kit only when there is a concrete server
  routed use case. It is not required for the Vue kit.
- Treat a RustClamp landing page recreation as a demo project, separate from
  the production static sites and their Python generators.

## Consequences

The todo kit can be used immediately for frontend experiments and later wired
to RustClamp over HTTP. It currently stores tasks in browser localStorage and
does not demonstrate RustClamp server integration. The Rust server in `--web` compiles
without Node; only building its frontend needs npm.

The kit uses the current Vite+ release documented by the upstream guide. A
browser build still needs to be confirmed in a network-enabled environment.
