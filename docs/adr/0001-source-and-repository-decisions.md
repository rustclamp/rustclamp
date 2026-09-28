# ADR 0001: Sources, names and repositories

Status: Adopted, 2026-09-28.

## Context

The historical specifications use Neo and Clamp package names, propose both
monorepo and sibling layouts, reuse phase numbers, and disagree on documentation
hosting. Preserve all 15 original files under `.project/`; implementation does not
rewrite those historical sources. Their links below resolve in a coordinated local
checkout. They are not bundled into published packages.

## Adopted resolutions

| Topic | Working decision | Basis |
| --- | --- | --- |
| Composition | Use the consolidated v1.5 architecture; retain compatible execution and lifecycle semantics from earlier versions | [Architecture v1.5](../../../.project/Neo%20Architecture%20v1.5%20%E2%80%94%20Composition%2C%20Minimal%20Kernel%20%26%20Progressive%20Control.md), [v1.5 Playtest](../../../.project/V1.5-PLAYTEST.md%20%E2%80%94%20Composition%20Architecture%20Findings.md), [Architecture Corrections](../../../.project/ARCHITECTURE-CORRECTIONS%20v1-v4.md%20%E2%80%94%20Findings%20After%20Bootstrap%20and%20Kernel%20Stress%20Testing.md) |
| Implementation order | Pico, capability, module, contribution, process; then lifecycle and integrations | [Implementation Start](../../../.project/IMPLEMENTATION-START.md%20%E2%80%94%20Framework%20Prototype%20Instructions.md), [Kickoff](../../../.project/KICKOFF.md%20%E2%80%94%20Neo%20Implementation%20%26%20AI-Native%20Foundation.md) |
| Public identity | RustClamp is the brand; Clamp is the framework; Neo remains historical terminology | [Brand Identity](../../../.project/BRAND.md%20%E2%80%94%20RustClamp%20Brand%20Identity.md) |
| Repositories | Use sibling repositories under this ecosystem root, starting with the four framework packages and two site repositories | [Repository Architecture](../../../.project/01%20-%20INIT%20-%20REPOSITORIES.md%20%E2%80%94%20RustClamp%20Ecosystem%20%26%20Developer%20Workspace.md) takes precedence for physical layout over older monorepo examples |
| Packages/imports | Use `rustclamp`, `rustclamp-core`, `rustclamp-kernel`, `rustclamp-runtime`; Rust imports use underscores | [Repository Architecture](../../../.project/01%20-%20INIT%20-%20REPOSITORIES.md%20%E2%80%94%20RustClamp%20Ecosystem%20%26%20Developer%20Workspace.md) section 30 takes precedence over `neo-*` and `clamp-*` examples |
| Entrypoint | Prototype `rustclamp::prelude::*` and `Clamp::run(...)`; exact signatures remain experimental | Repository package names plus brand terminology |
| Developer executable | Use `clamp` as a provisional name; validate the final binary name before release | [Repository Architecture](../../../.project/01%20-%20INIT%20-%20REPOSITORIES.md%20%E2%80%94%20RustClamp%20Ecosystem%20%26%20Developer%20Workspace.md) section 11 leaves this open |
| Documentation host | Plan the public documentation repository and `docs.rustclamp.com`; adapt earlier `/docs` SEO examples to that topology | [Repository Architecture](../../../.project/01%20-%20INIT%20-%20REPOSITORIES.md%20%E2%80%94%20RustClamp%20Ecosystem%20%26%20Developer%20Workspace.md) sections 20-21 are more specific than the initial-host recommendation in [Search and Indexing](../../../.project/GOOGLE.md%20%E2%80%94%20RustClamp%20Search%20%26%20Indexing%20Setup.md) |

The v1.5 model is authoritative for composition. Compatible earlier lifecycle and
execution semantics remain applicable. Repository Architecture overrides older
physical layouts and package names; Brand Identity controls public terminology.
The local phase checklists define implementation order. Discoveries refine v1.5
unless a genuinely new architectural concern warrants a new architecture version.

## Repository ownership

| Directory / remote | Initial responsibility | Required visibility |
| --- | --- | --- |
| `rustclamp/rustclamp` | Facade, examples, ADRs, shared checks and evidence | Public |
| `rustclamp/core` | Minimal shared contracts | Public |
| `rustclamp/kernel` | Composition and resolution | Public |
| `rustclamp/runtime` | Proven execution-environment contracts | Public |
| `rustclamp/rustclamp.com` | Marketing site, reserved only | Private |
| `rustclamp/docs.rustclamp.com` | Documentation and later reference application | Public |

Each directory is its own Git repository with an `origin` under the RustClamp
organization. Site implementation is deferred. Domains, DNS, Search Console and
production deployment are not verified by creating repositories.

`workspace/`, `experiments/` and `playground/` are local coordination, architecture
experiments and disposable apps respectively. They are not new framework packages.
See ADR 0002 for the experimentally validated Cargo layout and release conventions
for independent package versions. There are exactly four initial package manifests.

## Consequences

Historical snippets must be adapted to current package names and experimental
signatures. `rustclamp::prelude::*` and `Clamp::run(...)` are prototype targets,
not Phase 0 implemented APIs. The developer binary name `clamp` is provisional;
no CLI or integration package is created. Package name availability, licensing and
registry publication remain release gates distinct from local builds.

## Architecture Flow

~~~mermaid
flowchart TD
    Sources[Historical specifications] --> Reconcile[Record conflicts and evidence]
    Reconcile --> Composition[v1.5 composition baseline]
    Reconcile --> Names[Brand and package naming]
    Reconcile --> Ownership[Repository ownership]
    Composition --> Prototypes[Phased implementation proofs]
    Names --> Prototypes
    Ownership --> Prototypes
~~~

## Alternatives Considered

| Choice | Decision | Rationale |
| --- | --- | --- |
| Neo vs Clamp | Clamp for framework; retain Neo historically | Align implementation with the adopted public brand |
| Monorepo vs sibling repositories | Sibling repositories | Independent ownership and release boundaries are explicit |
| `neo-*` vs `rustclamp-*` packages | `rustclamp-*` | Matches the package namespace decision |
| Create every future package now | Four initial package repositories | New boundaries must be earned by working prototypes |
| Infer deployment from repository creation | Verify domains and hosting separately | A repository is not deployment evidence |

The decisions fix names and ownership boundaries while leaving Rust API
signatures open until experiments establish useful contracts.
