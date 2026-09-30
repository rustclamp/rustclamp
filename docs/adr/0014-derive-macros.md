# ADR 0014: Derive macros

Status: Accepted, 2026-09-30.

## Context

Every model repeats each field in `Model::from_row`:
`title: row.get("title")?`, once per column. That is the boilerplate left
in app code once config, startup and migrations moved into the framework.
Only a derive removes it, and a derive needs a proc-macro crate.

## Decision

- `rustclamp/macros` is the `rustclamp-macros` proc-macro crate, a path
  dependency of the facade in the same repository, so the facade's git
  dependency brings it along. Apps never name it: `rustclamp::db::Model` is
  both the trait and the derive, as `serde::Serialize` is.
- It is optional, enabled by the `db` feature. `syn`, `quote` and
  `proc-macro2` run in the compiler only and never link into the app;
  `tools/boundaries.py` allows exactly those for `rustclamp-macros`.
- `#[derive(Model)]` needs `#[model(table = "...")]`. The table is never
  guessed from the struct name: a naive plural gets names like `categories`
  wrong, and a wrong table fails only at run time.
- Each named field is read from the column of the same name. A struct whose
  fields differ from its columns implements `Model` by hand.
- `ToValue` stays hand-written: which fields a view sees, and in what form
  (a date instead of a timestamp, HTML instead of Markdown, never `id`), is
  a decision per model, not a default.

## Consequences

Models lose their `from_row` bodies. Further derives go in the same crate
and need their own entry here. A crate built without `db` compiles no
proc-macro code.
