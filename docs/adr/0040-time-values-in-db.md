# ADR 0040: Time values as query parameters and row values

Status: Proposed, 2026-10-21.

## Context

`Param` and `FromColumn` (`src/db/connection.rs`) cover integers, floats,
bools, text, bytes and `Uuid`, but not `std::time::SystemTime`
(rustclamp/rustclamp#178). Apps format each bound time with
`db::timestamp::format` or `time::format_rfc3339` and parse each read by
hand.

The framework's own convention is text. ADR 0034 (resolved question 4)
stores timestamps as UTC `YYYY-MM-DD HH:MM:SS` text on both engines.
`Schema::timestamp` is a `TEXT` column, and `default_now()` writes that form
(`CURRENT_TIMESTAMP` on SQLite, `to_char(now() AT TIME ZONE 'UTC', ..)` on
Postgres). ADR 0034 lists native `timestamptz` columns under "Not included".

A Postgres app that owns its schema still uses native types. A headless
data collector stores observation times in `timestamptz` and days in
`date`, and on Postgres the two representations disagree:

- Parameters go out untyped, in text (`postgres.rs`, `Client::query`). The
  server reads `2026-10-21 10:00:00` bound to a `timestamptz` in the
  session's `TimeZone`, which is the server default unless the app sets one.
  The same text bound to a `TEXT` column is stored as is.
- A `timestamptz` comes back in the session zone, such as
  `2026-10-21 12:00:00+02`. `time::parse_rfc3339` refuses that, because it
  wants `+02:00`.

So `impl Param for SystemTime` has to pick one text for both column kinds,
and the choice is a decision, not a fix.

## Options

| | (a) Convention text + UTC sessions | (b) A typed `Value::Time` | (c) Read only, document casts |
| --- | --- | --- | --- |
| Bind | `YYYY-MM-DD HH:MM:SS` UTC on both engines | SQLite: convention text. Postgres: parameter typed `timestamptz` (oid 1184) | none; apps write `?::timestamptz` with `+00` text |
| `TEXT` timestamp columns | correct | Postgres casts `timestamptz` to text in the session zone, so `+02` text lands in convention columns | unchanged |
| `timestamptz` / `timestamp` / `date` columns | correct, because the session zone is UTC | correct | correct with the cast |
| Behaviour change | Postgres sessions run in UTC: `now()::date` and `current_date` become UTC dates | `Value` gains a bind-only variant | none |
| Size | about 60 lines, a startup parameter, tests | about 100 lines and typed parameter oids in the wire client | about 30 lines |

## Recommendation: (a)

- **`impl Param for SystemTime`** binds `db::timestamp::format(t)`: UTC, whole
  seconds, the form `Schema::timestamp` columns hold. Sub-second precision is
  dropped, as `CURRENT_TIMESTAMP` does.
- **`impl FromColumn for SystemTime`** reads the convention text,
  RFC 3339, Postgres `timestamp`/`timestamptz` output with any offset (`+02`,
  `+05:30`, fractions), and a bare `date` (midnight UTC). Unix seconds in an
  integer column read too.
- **Postgres sessions run in UTC**: the startup message sends
  `TimeZone=UTC`. Then unzoned UTC text binds correctly into `timestamptz`,
  and `timestamptz` reads back with `+00`. Django does the same when
  `USE_TZ` is on. The framework is already UTC everywhere: `rustclamp::time`,
  `Schema::default_now` and the schedule.
- **Local days stay explicit.** An app that groups by a local day computes
  the bounds with `time::Zone` (rustclamp/rustclamp#177) and binds the
  instants. It does not rely on the server's zone.

## Open questions for review

1. Is a UTC session acceptable? It changes raw SQL that relies on the
   server's zone (`current_date`, `now()::date`, `to_char(now(), ..)`).
   Whether an app can still override it with `options=-c TimeZone=..` in
   `DATABASE_URL` needs a test of startup-parameter precedence.
2. Whole seconds, or microseconds when the value has them? Microseconds
   break SQLite's lexical order against whole-second rows (`.` sorts before
   end of string), so (a) keeps whole seconds.
3. Is there a separate `Date` type, or does a date stay a `SystemTime` at
   midnight UTC, as `time::parse_date` returns today?

## Consequences

- Apps on the convention `TEXT` columns gain typed binds and reads, with no
  change to what is stored.
- Native `timestamptz`/`date` columns work on Postgres without casts. This
  partly lifts ADR 0034's "Not included" line, for time values only.
- The wire client gains one startup parameter. No new crate, and no
  `tools/boundaries.py` change.
