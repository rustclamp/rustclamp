# RustClamp developer tooling

`rustclamp-tooling` provides a versioned inspection document API and the `clamp`
developer command. It consumes resolved process metadata from an application;
it does not parse source code, initialize the application, open connections, or
start services.

The application exports a document with `inspection_document(&[&projection])`
and writes the returned JSON value. A single document can contain multiple
processes from one application. Pass `--process ID` when it contains more than
one.

```sh
clamp inspect architecture.json
clamp tree architecture.json --process web
clamp graph architecture.json --json
clamp why architecture.json storage.postgres --process web
clamp doctor architecture.json
```

Exit status is 0 for a valid document, 1 when `doctor` reports an invalid
composition, and 2 for command, file, or schema errors. `--json` returns
structured JSON. The output includes stable semantic IDs, sorted process and
relationship collections, resolved inclusion paths, provider selection,
replacements, exclusions, contributions, and structural counts. Counts are
descriptive; they are not estimates of memory or runtime cost.

## Schema version 1

Every document has `schema_version: 1`, an `application` semantic ID, and a
`status`. Resolved documents have a `processes` array. Invalid documents have a
`process` ID and `diagnostics` array containing stable `code`, `severity`,
human `message`, and structured `context` fields. Diagnostic codes identify
categories; consumers should preserve unknown codes and fields for forward
compatibility. A changed meaning or incompatible shape requires a new schema
version. The command rejects unknown versions.

The inspection model contains architecture metadata only. It never serializes
configuration values, secrets, live runtime state, or provider credentials.
Adding fields to this public schema requires compatibility review.
