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
check passes. The playbook is an untracked local operator file and has not been
deployed as part of this evidence.

This is a local hosting prototype. Static file reads are synchronous, production
deployment is not switched to Rust, and the page generator remains Python.

## Remaining release work

The developer inspection CLI and versioned inspection/error schema remain open.
Package publication is blocked on the recorded license and registry ownership
decisions. Search Console verification and production deployment are separate
external gates. Optional AI/MCP work has no concrete provider use case yet and
remains independent from package/site release.
