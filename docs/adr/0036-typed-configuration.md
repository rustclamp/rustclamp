# ADR 0036: Typed configuration in Runtime

Status: Proposed, 2026-10-21.

## Context

`rustclamp::config` (feature `config`) is the only configuration in the
ecosystem. It is std-only and string-keyed: `get`, `get_or`, `require`, each
reporting one key at a time. Apps built on the component crates (Axum through
`rustclamp-http`, SQLx through `rustclamp-postgres`, NATS workers) don't use
the facade, so they read `std::env::var` by hand (`examples/06-create-order`,
`03-email-worker`, `01-users/examples/http.rs`) or reach for figment, config
or confique and wire the result into their own state (rustclamp/runtime#13).

They need one thing: build a `serde::Deserialize` struct from env, `.env` and
an optional file, and stop at startup with every bad key listed, not the first.

## Decision

- **Runtime owns it**, as `rustclamp_runtime::config` behind an optional
  `config` feature. Loading settings is part of starting a process, which is
  Runtime's job ("execution environment"). Core holds domain-neutral contracts
  and has no dependencies on Unix; Kernel resolves capabilities and depends
  only on Core; `rustclamp-http` is one interface of several, and workers need
  the same loader. The default Runtime build stays dependency-free.
- **API.** `Config::new("APP").load::<T>()`, with `env_file`,
  `without_env_file`, `toml_file` (feature `config-toml`) and `load_from(vars)`
  for tests. The name matches the facade's `Config::load`.
- **Layers:** process environment > `.env` > TOML. `APP__DB__URL` fills
  `db.url`; segments are lowercased to match field names. A missing `.env` is
  skipped; a named TOML file that is missing is an error. Loading never
  changes the process environment.
- **Every problem at once.** Serde stops at the first error, so the loader
  retries: each failure becomes a `Problem` and its key is replaced by a
  placeholder that deserializes to a dummy value, until the type builds.
  `ConfigError { problems }` mirrors `RunError { failures }`: a composition-time
  error carrying the whole list, `; `-separated in `Display`. Messages name
  keys in environment form and never values; TOML parse errors keep the
  message and line, not toml's quoted source line.
- **`Secret<T>`** prints `[redacted]` in `Debug` and `Display`; `expose()`
  and `into_inner()` are the only ways to the value.
- **Dependencies.** `config` adds `serde` (`std`, no derive; apps bring their
  own derive). `config-toml` adds `toml` with only its parser (`std`, `serde`,
  `parse`): `toml_parser`, `toml_datetime`, `serde_spanned`, `winnow`.
  `tools/boundaries.py` allowlists them for `rustclamp-runtime` and keeps
  `serde` and `toml` optional there. Dev-dependencies (`serde` with derive for
  tests) are exempt from the optional rule; they never reach consumers.

## Deferred

- **Config as a Kernel capability.** A `Provision` of loaded settings needs a
  capability type per settings struct, and a home for it (Core has no serde).
  Apps pass `&Settings` or an `Arc<Settings>` into state for now.
- **Reload on SIGHUP**, `${VAR}` interpolation, multi-line `.env` values,
  enum variants with data, YAML/JSON files.
- **Sharing the `.env` parser with the facade.** The facade's parser is 15
  lines and the facade is std-only by design; Runtime's adds escapes and
  inline comments. Merge them if they drift.
- **Its own crate.** If config grows past this, `rustclamp-config` would let
  apps take it without Runtime. Not worth a repository yet.

## Alternatives

- **figment / config / confique.** Mature, but each pulls a larger graph
  (figment: `pear`, `uncased`, `atomic`; config: several format crates by
  default) and reports the first error.
- **The facade's `Config`.** Requires the facade, has no typed loading, and
  panics or returns per key.
- **A derive macro** generating per-field loaders could report all errors
  without retries, but adds a proc-macro crate for what serde already does.
