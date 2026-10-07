# ADR 0032: Profiles and recipes

Status: Accepted, 2026-10-07.

## Context

Stage 9 lists "profiles and recipes for each kind of software". The docs already promise what a profile is: a preset of defaults, recommended modules and policies for a kind of software (CLI, service, worker, game, simulation, device). Profiles are flat and combinable, never a separate edition of Clamp, and nothing a profile gives is out of reach without it. Phase 4 left open whether default precedence should become profile-scoped.

`clamp init` already writes projects from templates (`--blank`, `--app`, `--web`, `--tui`, `--package`), but only one at a time. The runnable examples (`01-users`, `03-email-worker`, `07-device-loop`, `08-site-server`, `09-observability`) already show each kind of software end to end, and `tools/check.py` runs them in CI. A generated project depends on the `rustclamp` facade from Git. `rustclamp-worker`, `-scheduler` and `-runtime` reach `core` by relative path, so a Git dependency on them doesn't build until they're on crates.io.

## Decision

- **A profile is a generator preset: `clamp init NAME --profile cli,service,worker`.** Each profile adds one file to `src/` (`cli.rs`, `service.rs`, `worker.rs`) with its defaults and policy written out as ordinary code, plus the facade features it needs. The output is code the app owns and can edit or delete. Profiles add no type, trait or feature to the framework, so nothing a profile gives is out of reach without it.
- **Combining is a merge in the generator.** Features are the union. `main.rs` declares each chosen profile's module, runs the first one by default and the others by subcommand (`serve`, `work`). `Procfile.dev` gets one line per long-running profile, so `clamp dev` starts the service and the worker together. Order is fixed (cli, service, worker), so `--profile worker,cli` and `--profile cli,worker` write the same project.
- **What each profile sets:**
  - `cli`: one command per run; stdout for results, stderr for usage errors, exit code 2 for bad usage. No features beyond the default.
  - `service`: `web` + `metrics`. HTTP on `PORT` (`web::serve`), `/up`, `/metrics` through `Router::metrics` (ADR 0030), the request log and `security_headers`. One in-process route test.
  - `worker`: `config` + `log`. One job at a time, polling every `WORKER_POLL_MS` (1000) when idle, and `--once` for cron and tests. Where jobs come from is left to the app: a comment points at `rustclamp-worker`'s `WorkerService` (ADR 0020) for retries, dead letters and drain.
- **A recipe is a docs page section** that links a kind of software to its profile and to the runnable example that goes further. docs.rustclamp.com/recipes has cli, service, worker and device. Device has an example (`07-device-loop`) but no profile yet.
- **Precedence stays explicit** (Phase 4's question): a profile writes code once and changes no resolution rule. Kernel precedence is not profile-scoped.
- **Rejected:**
  - A Kernel-level preset (a bundle of modules plus config defaults added in one line). It needs a new runtime concept and a precedence rule for preset against app defaults. Facade apps don't compose Kernel modules today, and the modules a worker or device preset would bundle can't be reached from a generated project until crates.io.
  - Cargo feature sets on the facade (`profile-service = ["web", "metrics"]`). They combine for free, but they only pick modules, not defaults or policy. Each alias would also be a facade API kept forever, and it hides which features are on. The generator writes the plain feature list instead.
  - Turning `--web`/`--tui` into profiles. They are full layouts (`app/`, Vite, views), not combinable presets, and they stay templates.

## Consequences

`tools/check.py` generates `--profile cli`, `--profile worker` and `--profile cli,service,worker` against the branch's facade, checks, tests and runs them (`work --once`). The service runs only its route test, like `--web`. Device, game and simulation profiles, and profiles that depend on `rustclamp-worker`, `-scheduler` or `-runtime`, wait until those crates are on crates.io. Profiles can't be combined with a template flag, and a project can't add a profile after `init`; `clamp make:profile` waits until someone needs it.
