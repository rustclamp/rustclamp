# Phase 9 Progress

Date: 2026-09-29

## RustClamp hosted-site proof

Added `examples/13-site-server`, a small Axum server assembled through
`rustclamp-http`'s public `HttpRoutes<Public>` contribution target. The same app
serves either site's generated `public/` directory. It does not reconstruct the
site model or replace the Python static-site generator.

Both preview servers were run locally. The RustClamp and Python responses were
byte-identical for each checked route:

| Site | Routes compared | Result |
| --- | --- | --- |
| `rustclamp.com` | `/`, `/about`, `/assets/site.css`, `/missing` | 200/200/200/404; identical status and bodies |
| `docs.rustclamp.com` | `/`, `/getting-started`, `/architecture`, `/assets/site.css`, `/missing` | 200/200/200/200/404; identical status and bodies |

Focused `cargo clippy --all-targets -D warnings` and `cargo test` passed for the
example. The coordinated `python3 rustclamp/tools/check.py` run passed all 152
commands with 9 existing package-license warnings. The static builders produced
2 and 17 pages. A local artifact check
confirmed every sitemap URL maps to a generated page with the matching canonical
URL, the correct host sitemap is referenced by `robots.txt`, and the 404 page is
excluded from indexing.

The Ansible nginx template now redirects legacy `/docs` and `/docs/...` requests
from `rustclamp.com` to the matching path on `docs.rustclamp.com`. Its syntax
check passes. The playbook is an untracked local operator file. Check mode
passed preflight, canonical, robots, route, and Nginx syntax checks. The actual
deploy then stalled during upload: Ansible reported the `rustclamp.com` upload as
changed, but timed out reaching the host before it could finish the docs upload,
apply the redirecting vhost, or complete live verification. Direct public
requests from this environment returned HTTP 403, and SSH to the host timed out.
Treat production state as partial and unverified; rerun the playbook when SSH and
public HTTP access are available.

This is a local hosting prototype. Static file reads are synchronous, production
deployment is not switched to Rust, and the page generator remains Python.

## Developer inspection tooling

Added the standalone `rustclamp-tooling` package with the `clamp` binary and a
version 1 JSON document API over Kernel's resolved `ProcessProjection`. It
provides `inspect`, `tree`, `graph`, `why`, and `doctor`, process selection,
structured JSON output, stable diagnostic codes mapped from projection errors,
and count-only structural cost. Inspection reads a document and does not
initialize or execute the application. The schema guide records identity,
ordering, compatibility, and configuration redaction rules.

Focused formatting, Clippy, unit, rustdoc, and CLI smoke checks pass. A unit
test constructs a Kernel blueprint, resolves a provider, and verifies that the
exported document and `why` output reflect that projection. CLI smoke runs
covered human tree/why output and JSON inspect output. `tools/check.py` now
includes this standalone package. `clamp check`, `test`, `build`, and `run` pass
arguments and exit status through to Cargo; the check wrapper has a smoke run.
`cargo install --path tooling --root /tmp/clamp-global-install` installed the
binary successfully, and the installed command ran from `/tmp`. The no-argument
menu and `clamp init` flow both ran. The generated app compiled and ran against
the local facade; fetching its public Git dependency could not be verified in
this network-restricted environment.
Both site sources now carry an installer at `/install.sh`; the marketing
homepage shows the copyable install command and the docs explain prerequisites.
The script builds from the three public Git repositories on Linux/macOS. Its
GitHub main-branch source is ready to become usable when the tooling branch is
merged; live deployment remains unverified.
Output still needs comparison against every reference example; required
configuration and lifecycle boundaries need fuller presentation. Project
generation is implemented, while its ordinary registry build remains gated on
a publishable dependency target.

## Remaining release work

Package publication is blocked on the recorded license and registry ownership
decisions. Search Console verification and production deployment are separate
external gates. Optional AI/MCP work has no concrete provider use case yet and
remains independent from package/site release.
