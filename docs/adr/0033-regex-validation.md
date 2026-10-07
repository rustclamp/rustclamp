# ADR 0033: Regex validation

Status: Accepted, 2026-10-07.

## Context

#117 added validation rules to `Request::validate` but left out `regex` (#125). `web` is std-only, and std has no regular expressions. A pattern in the rule string also clashes with the rule separator: in `"required|regex:^(cat|dog)$"`, the `|` splits the pattern into two rules.

## Decision

- **An optional `regex` feature** (implies `web`) with the [`regex`](https://crates.io/crates/regex) crate: `default-features = false` plus `std`, `unicode-perl` (Unicode `\d`, `\w`, `\s` and `\b`) and `unicode-case` (`(?i)` on non-ASCII text). The `perf` features are off: form fields are short, so the literal search and lazy DFA aren't worth their extra crate and code (not measured).
- **Closure:** `regex`, `regex-automata` and `regex-syntax`, all from the Rust project's regex repository (`cargo tree -e normal -p rustclamp --features regex`). `aho-corasick` appears in `Cargo.lock` and in metadata, because regex's `std` feature names it weakly (`aho-corasick?/std`, rust-lang/cargo#10801), but it is never built. `tools/boundaries.py` allows all four.
- **Patterns are registered, never parsed out of a rule string.** `web::Patterns::new(&[("slug", r"^[a-z0-9-]+$")])` goes into `Router::state`, and the rule is `regex:slug`, so a pattern can hold any character, `|` included. `Patterns::new` compiles each pattern once and panics on one that doesn't compile, naming it: a bad pattern is a bug in the app and shows at startup, not on a request. A `regex:NAME` with no `Patterns` in state, or no pattern by that name, panics like an unknown rule. This follows `unique`, which reads the `Db` from `Router::state`.
- **Behaviour like the other rules:** an empty field that isn't `required` passes, the default message is "The {label} field format is invalid." (Laravel's text), and `validate_with` overrides it with the key `field.regex`. A pattern matches anywhere in the value unless it is anchored with `^…$`, as with Laravel's `preg_match`.
- **Why the crate and not a hand-rolled matcher:** a small backtracking matcher is easy to write and easy to make exponential. Form input comes from anyone, so a pattern such as `^(a+)+$` would let one request pin a thread (ReDoS). The `regex` crate guarantees matching in time linear in the input and has no backreferences or look-around, which are the features that make that guarantee impossible. That guarantee is the reason to take the dependency.

## Not included

Backreferences and look-around (the crate refuses them by design), the `not_regex` rule, patterns given inline in the rule string, and regex anywhere else in the facade (routing, views). Each waits until an app needs it.

## Consequences

Apps without the feature build exactly as before; `regex:...` without it panics as an unknown rule. The feature adds three crates to compile.
