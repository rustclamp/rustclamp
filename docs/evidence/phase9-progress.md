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
the public facade Git dependency and printed `Hello from Clamp!`.
The marketing site owns the single installer at `rustclamp.com/install.sh` and
shows its copyable command on the home page; docs link to that canonical URL.
The script builds from the three public Git repositories on Linux/macOS. It
uses `phase9-site-hosting` for RustClamp because `main` does not yet contain the
CLI. The exact Bash command has now run from the live URL in an isolated Cargo
home; `clamp --help`, `clamp init`, and `cargo run` from `/tmp` all passed.

The production deploy uploaded the RustClamp site, and live checks returned
HTTP 200 for `/install.sh` with bytes matching the tested script. The live
homepage includes the copyable install command. A separate docs-only deploy
first failed during SSH connection setup (port 22 timeout), then succeeded on
retry. The live getting-started page now shows the canonical Bash command.

The deployed installer was rerun with an isolated `HOME` and zsh setting. It
added the Cargo bin path to `.zshrc`; the installed binary ran from a different
directory. The real user's `.zshrc` also now contains that PATH entry. The
current terminal must reload it with `source ~/.zshrc; rehash`.

No prebuilt binary release exists yet: there are no release tags or binary
release workflow in this checkout. The source installer compiles `clamp` on
each machine. A local Linux installation was verified under `/tmp`; it is not a
published artifact. Cross-platform binary packaging and release verification
remain open. The installer now adds `~/.cargo/bin` to the user's zsh or bash
startup file when that directory is missing from `PATH`; a new terminal or
reloading the startup file makes `clamp` available from any folder. The public
site deployment and the docs deployment are complete; Search Console and
broader site QA remain open.
Output still needs comparison against every reference example; required
configuration and lifecycle boundaries need fuller presentation. Project
generation is implemented, while its ordinary registry build remains gated on
a publishable dependency target.

## Remaining release work

Package publication is blocked on the recorded license and registry ownership
decisions. Search Console verification is an external gate. Optional AI/MCP
work has no concrete provider use case yet and remains independent from
package/site release.
