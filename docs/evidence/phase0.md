# Phase 0 evidence

Date: 2026-09-28. Compiler/MSRV: Rust 1.96.1. Host: x86_64 Linux.

## Layout and ownership

The original ecosystem contained specifications only and no working Git repository
at the root. Created independent local Git repositories for `rustclamp`, `core`,
`kernel`, `runtime`, `rustclamp.com`, and `docs.rustclamp.com`, with short `origin`
URLs under `github.com/rustclamp`. GitHub repository creation returned public
visibility for the four libraries and documentation site, and private visibility
for the marketing site. DNS, hosting and registry ownership remain unverified.

Cargo rejected the proposed sibling members from `workspace/Cargo.toml` with exit
101 and “not hierarchically below the workspace root.” The generated virtual
manifest at the ecosystem root succeeds, without requiring manifest inheritance
or `package.workspace` in any package. See ADR 0002.

## Verification

Command: `python3 rustclamp/tools/check.py` from the ecosystem root.

| Check | Observed result |
| --- | --- |
| `cargo metadata` | Exactly `rustclamp`, `rustclamp-core`, `rustclamp-kernel`, `rustclamp-runtime`; zero dependency edges |
| Formatting and Clippy | Pass for combined checkout and all four isolated copies, warnings denied |
| Cargo tests | Pass with all features and no default features; facade integration target runs the architecture harness |
| rustdoc | Pass with warnings denied; public documentation and intra-doc link policy configured |
| Isolation | Each copied repository checks without a parent workspace or sibling checkout |
| Fresh checkout | Cloned all four local Git repositories into a new temporary root; full check script passes there |
| Relative consumers | Four temporary consumers use `version = "0.1"` plus `path = "../…"`; each builds offline |
| Package archives | All four `cargo package --offline --locked --allow-dirty` runs package and verify; expected missing-license warning |
| Runtime/integrations | No third-party crates, runtime implementation, composition subsystem or site implementation |

The library and doc-test targets currently contain no behavioral tests because no
public behavior exists yet. The facade has one real integration test executing
eight Cargo metadata fixtures:

| Fixture edge | Kind | Result |
| --- | --- | --- |
| Core → Tokio | Normal | Rejected |
| Core → reqwest | Optional, activated by all-features metadata | Rejected |
| Core → SQLx | Build | Rejected |
| Core → async-openai | Inactive target condition | Rejected |
| Kernel → Axum | Normal | Rejected |
| Runtime → Tokio | Normal | Rejected |
| Kernel → facade | Dev | Rejected |
| Kernel → Core | Versioned relative path | Accepted |

Fixture dependencies are renamed to `hidden_alias` to exercise actual package-name
checks. They are generated in temporary directories as real Cargo packages with
no network resolution. CI invokes them through `cargo test`, and the boundary
checker traverses resolved dependencies as well as checking declarations.

Metadata summary: [phase0-metadata.json](reports/phase0-metadata.json).
Measurement procedure output: [phase0-facade.json](reports/phase0-facade.json).
The report contains three fresh-target clean builds and three unchanged rebuilds.
It measures scaffold compilation only; binary size, startup and allocations await
Pico. No framework-overhead claim is made.

## Hosting and release limits

The four repository workflows each check their own checkout and a combined
checkout, including isolated consumers. Each remote first received a commit
containing only `README.md`. The owner subsequently authorized source publication;
the complete local histories are being joined to those README commits. Hosted run
results are recorded after the source pushes and workflows finish.

All manifests disable publishing; license selection,
registry availability/ownership and registry-based publish dry runs remain release
gates. Local package verification does not establish those properties.

Original `.project/` specifications are preserved. The initial entrypoint and
prelude are intentionally deferred to Phase 1, and unused placeholder contracts
were not introduced into Core, Kernel or Runtime.

`graphify update .` completed after the initial sandboxed attempt failed; its local
generated graph remains outside the six repositories.
