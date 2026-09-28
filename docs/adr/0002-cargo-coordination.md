# ADR 0002: Cargo coordination and package policy

Status: Adopted, 2026-09-28.

## Conflict and evidence

The repository guide's section 26 proposes a `workspace/Cargo.toml` with members
such as `../core`. Cargo 1.96.1 rejects this arrangement:

```text
workspace member `…/core/Cargo.toml` is not hierarchically below
the workspace root `…/workspace/Cargo.toml`
```

Explicit `package.workspace` paths would bind independent package manifests to a
particular coordinated checkout. Workspace inheritance would similarly obstruct
standalone checkouts. Neither is needed for the initial packages.

## Decision

Generate a virtual manifest at the common ecosystem root using the facade's
`tools/workspace.py`. This is local Cargo coordination; Git ownership remains in
six sibling repositories. `workspace/` remains reserved for local coordination
notes. Do not commit the root as a framework monorepo. Generation refuses to
overwrite different existing coordination files.

Each library owns all its manifest settings and has no required sibling dependency
yet. `cargo metadata` must show exactly the four initial members and zero edges.
The check script verifies copies outside the parent workspace and relative-path
external consumers, and verifies Cargo package archives. Future justified sibling
dependencies carry versions; registry publish validation waits for a release.

Pin Rust 1.96.1, edition 2024, resolver 3, and MSRV 1.96.1. This is the installed
stable release tested here, not a claim that it is the latest release or lowest
theoretically possible MSRV. Compiler, rustfmt, Clippy, tests and rustdoc work on
this exact toolchain. Do not lower the MSRV without testing that compiler.

Public APIs require documentation; unsafe code is forbidden. New dependencies need
prototype evidence and an explicit allowlist update. Keep Core free of runtimes,
transports, databases and AI; Kernel free of domain integrations; Runtime free of
mandatory executors; and lower layers free of the facade. Apply this policy to
all dependency kinds, optional features, target conditions and transitive edges.

No facade entrypoint, placeholder traits or composition structures are needed for
Phase 0. Phase 1 introduces the measured closure entrypoint; later prototypes
introduce Core, Kernel and Runtime contracts. Publishing stays disabled while
licensing and registry access remain undecided.

## References

- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)
- [Rust 1.96.1 release](https://blog.rust-lang.org/2026/06/30/Rust-1.96.1/)
- [Phase 0 evidence](../evidence/phase0.md)
- [Release conventions](../releases.md)
