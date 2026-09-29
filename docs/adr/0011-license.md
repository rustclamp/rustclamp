# ADR 0011: License

Status: Accepted, 2026-09-29.

## Context

Publishing was blocked on a license decision (ADR 0001, ADR 0002). RustClamp
builds on crates that are almost all `MIT OR Apache-2.0`, as Rust itself is.

## Decision

RustClamp is licensed `MIT OR Apache-2.0`: `LICENSE-MIT` and
`LICENSE-APACHE` at the repository root, and `license` in every manifest.
MIT keeps it simple to adopt; Apache-2.0 adds an explicit patent grant.
Contributions are dual licensed on the same terms.

## Consequences

The sibling repositories (core, kernel, runtime, http, postgres, messaging,
worker, scheduler) need the same files and manifest field when they are next
changed. Registry ownership remains the last publication gate.
