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
example. The coordinated `python3 rustclamp/tools/check.py` run passed all 166
commands with 9 existing package-license warnings. The static builders produced
2 and 18 pages. A local artifact check
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
initialize or execute the application. The tooling guide records schema
identity, ordering, compatibility, and configuration redaction rules. It maps
execution roots, inclusion paths/reasons, provider defaults/replacements,
exclusions, contribution targets, and the application-owned configuration and
lifecycle information to their authoritative examples. The JSON projection is
the IDE-consumable contract; no parallel architecture database is maintained.

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

The first production deploy attempt only partially uploaded `rustclamp.com` and
timed out before the docs upload, redirect, or live verification. A retry later
completed the docs-only deploy after another SSH timeout. At the time of the
successful checks, live `/install.sh` returned HTTP 200 with bytes matching the
tested script, the homepage showed its copyable install command, and the live
getting-started page showed the canonical Bash command.

The deployed installer was rerun with an isolated `HOME` and zsh setting. It
added the Cargo bin path to `.zshrc`; the installed binary ran from a different
directory. The real user's `.zshrc` also now contains that PATH entry. The
current terminal must reload it with `source ~/.zshrc; rehash`.

No prebuilt binary release exists yet. Added
`.github/workflows/release-clamp.yml` to build and smoke-check Linux x86_64,
Intel and Apple Silicon macOS, and Windows x86_64 archives with SHA-256 files.
Pushing a `clamp-v*` tag creates a GitHub release; manual dispatch builds
temporary artifacts without publishing. The workflow has not run on GitHub and
no release tag has been created, so the source installer still compiles `clamp`
on each machine. The Linux x86_64 release binary built locally with Rust 1.96.1,
passed `clamp --help`, and its tarball checksum verified; the other target
artifacts await a hosted workflow run. A local Linux installation was verified
under `/tmp`; it is not a published artifact. The installer now adds
`~/.cargo/bin` to the user's zsh or bash startup file when that directory is
missing from `PATH`; a new
terminal or reloading the startup file makes `clamp` available from any folder.
Those deployment checks establish the state at that time. Search Console and
broader site QA remain open.
The coordinated check runner compares `inspect`, `tree`, `graph`, and
`why` against real resolved projections from every reference example that
exports a `ProcessProjection`: `04-process`, `05-lifecycle`, `09-create-order`,
and `11-device-loop`. This exposed and fixed quoted IDs in graph output, with a
regression test. The opt-in exporters leave each example's default dependency
graph unchanged. Examples without a `ProcessProjection` do not have comparable
output for this API. Configuration evaluation, lifecycle outcomes, message
delivery, database transaction state, and device state remain outside the
inspection schema.

Focused Clippy and tests passed for each exporter; the tooling package's
Clippy and all five tests passed. The coordinated comparison passed for every
exported process projection. The runner also generates both `hello-world` and
`application` projects, then runs Cargo check, test, and run on each. Its offline
smoke substitutes the local facade path only in the temporary generated copy;
the emitted Git dependency is asserted before substitution. The default Hello
World had also previously been run against the public Git dependency.

Project generation is implemented, while its ordinary registry build remains
gated on a publishable dependency target. `clamp init NAME` creates a Rust
Hello World; `clamp init NAME --template application` creates a larger Rust
layout with an application composition module and health module. Frontend kits
are separate in `starter-kits/`. The first is a Vue 3 todo demo using Vite+;
install and browser build could not be verified because this environment could
not reach package registries. ADR 0007 records these boundaries and the next
starter-kit directions.

The source websites previously described all of Phase 9 as complete. Their
homepage and roadmap copy now says Phase 9 is in progress and identifies the
CLI and installer as available; the CLI page calls it available rather than
released. Getting-started and examples pages now identify the actual source
branches for examples 00–13. The public sites have not been redeployed with
these corrections. Both static builds pass, all 923 checked internal links and
assets resolve, sitemap entries map to generated pages, and only the custom
404 has `noindex`.

Static CSS inspection confirms responsive breakpoints, visible keyboard focus,
reduced-motion handling, and horizontally scrollable code and tables. A
contrast calculation found white text on the light orange primary button was
below 4.5:1; light-theme button text now uses near-black (5.6:1), and small
orange text uses the darker accent (5.2:1). CSS is identical on both sites.
Headless Chromium and Firefox could not start under the environment's process
restrictions, so rendered viewport, keyboard interaction, and full contrast
checks remain open. The preview servers also cannot bind local ports here.
There is no staging host configured; production indexing headers and current
live route status still need an external check.

## Web packages

ADR 0008 adds self-contained web packages. `Router::package` mounts a
`web::Package` as a group, and a facade test shows package middleware stays on
package routes. `package_view` prefers the app's built
`views/vendor/{package}/` override, tested against a temporary web root. The
coordinated runner generates a `--package` crate, checks that it emits the Git
dependency, then checks and tests it against the local facade; its three tests
run on a bare `Router`. A generated `--web` app with a generated package mounted
also served the package route with the app's security headers, and served a
`vendor/` override in place of the embedded view. Package migrations, workers,
scheduled jobs, commands and a publish command are deferred.

## Remaining release work

Package publication is blocked on the recorded license and registry ownership
decisions. Search Console verification is an external gate. Optional AI/MCP
work has no concrete provider use case yet and remains independent from
package/site release.
