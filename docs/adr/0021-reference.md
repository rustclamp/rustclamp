# ADR 0021: Reference — a UUIDv7 on every request, command and action

Status: Proposed, 2026-09-30.

## Context

When something goes wrong, the first question is "which request was it?",
and today there is no single answer:

- The facade web server (`rustclamp::web`) gives a request no identity at
  all. Log lines carry the method and path, and a 500 page says nothing a
  user could report back.
- `rustclamp-http` mints `request-{N}` from a process-wide counter. It
  restarts at 0 on every deploy, repeats across processes, and says nothing
  about when the request happened. It also takes a client's
  `x-correlation-id` as the id, so any caller can choose or collide it.
- CLI commands, scheduled tasks and worker jobs have no id, and a job
  cannot be traced back to the request that queued it.

A UUIDv7 fixes all three. It is unique without coordination, and it starts
with its creation time in Unix milliseconds, so references sort by time.
Given only a time window, the lowest and highest possible reference in that
window can be computed. "Everything between 10:00 and 10:05" then becomes a
plain range comparison: `BETWEEN` in SQL, or string comparison over log
lines. The facade already generates v7 IDs (`rustclamp::uuid`); only the
core crates cannot reach them.

## Decision

1. **`rustclamp_core::Reference`**, std-only, no dependencies. It wraps a UUIDv7:
   - `Reference::new()`: a v7 ID, strictly increasing within the process
     (RFC 9562 §6.2, method 1: a 12-bit counter right after the time; a
     full counter moves into the next millisecond);
   - `Display` and `FromStr` in the hyphenated lowercase form;
   - `created_at() -> SystemTime`: the time the reference was minted;
   - `Reference::range(from, to) -> (Reference, Reference)`: the lowest and
     highest reference inside `[from, to]`, for range filters.

   The generator moves from the facade to core. `rustclamp::uuid::Uuid`
   keeps its API and uses core's v7 bytes, so there is one implementation.
   The facade depends on core only under its `uuid` feature, so Pico
   (ADR 0003) keeps zero dependencies.
2. **We always mint our own.** An incoming `X-Request-Id` or
   `x-correlation-id` is never used as the reference. It is logged next to
   the reference as `client_ref=…`, so a trace from another service can
   still be joined. The value is capped in length and never parsed, so an
   outside caller cannot choose, collide or inject a reference.
3. **Every entry point gets a reference, and every log line under it carries
   the reference:**
   - **Facade web:** `Router::handle` mints one per request, and
     `Request::reference()` returns it. The response carries it in
     `X-Request-Id`. Log lines written while handling the request add
     `ref=<reference>`, and the built-in 5xx error page shows it.
   - **`rustclamp-http`:** the request-context middleware mints a
     `Reference`, which replaces `request-{N}`. It is exposed the same way:
     request extension, `X-Request-Id` header, log field.
   - **CLI (`clamp`) and app commands:** one reference per invocation. On a
     failure, it is printed with the error.
   - **Worker and scheduler:** each job and each tick gets its own
     reference. A job queued while handling a request also stores that
     request's reference as `parent`, so one ID traces the whole chain.
4. **Finding things by time:**
   - `clamp ref <reference>` prints when the reference was created.
   - `clamp logs --from <time> --to <time>` (or `--ref <reference>`)
     filters log lines by reference range, without parsing timestamps.

## Order of work

1. Core type, and the facade `uuid` module switched onto it.
2. Facade web: minting, header, log field, error page. This is where the
   facade gains the per-request log context that the log line needs.
3. `rustclamp-http` middleware.
4. CLI, worker and scheduler.
5. `clamp ref` and `clamp logs` filters.

Each step ships on its own.

## Consequences

- Every log line under a request, command or job can be found from one ID,
  and a time window narrows to a reference range.
- The creation time can be read from a reference. That is fine for
  request, command and job references, which are operational IDs, not
  secrets. Anything that must hide its time (tokens) keeps using v4.
- Breaking for `rustclamp-http` users: a client's `x-correlation-id` is no
  longer the request's ID. It moves to `client_ref`.
- Minting costs one clock read, one lock, and 16 bytes from
  `/dev/urandom` per request. To keep that off the hot path, the random
  source is read in batches.
- The facade gains `rustclamp-core` as an optional dependency. The boundary
  table needs `rustclamp → rustclamp-core` in `OPTIONAL`.
