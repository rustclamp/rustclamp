# Phase 7 Progress: Messaging and Workers

The initial `rustclamp-messaging` package defines a transport-neutral serialized
envelope with stable message identity, semantic name, schema version, correlation
and optional causation identity, and JSON payload. Local Rust events remain
ordinary in-process values. The `07-messaging` example maps a local `UserCreated`
event explicitly to this envelope before serialization.

The boundary and JSON payload choice are recorded in
[ADR 0006](../adr/0006-phase7-message-boundary.md). The package is added to the
local workspace and isolated-package tooling. Its `MessageBus` capability is
typed through Core, while transport and serialization remain outside Core and
Kernel. The bounded in-memory bus is for tests and development; it does not
claim cross-process delivery. NATS JetStream is selected as the real transport,
using durable pull consumers, explicit acknowledgments, and bounded batches.
Its delivery is at least once, so duplicate-safe handling is required.

The initial `rustclamp-worker` target compiles message-name/schema-version
handlers, rejects invalid or duplicate declarations, decodes serialized
envelopes, and preserves handler errors. It depends on public Core and Messaging
contracts; routing has no broker-specific message type.

The `08-email-worker` example wires the domain `EmailSender` port to a
`QueuedEmailSender`, routes its envelope through the in-memory bus, and invokes
the worker handler, which calls the email operation with a memory gateway. It
also defines independent `email-api` and `email-worker` executables that exchange
the versioned envelope over JetStream. The API waits for the broker's publish
acknowledgement; the Worker uses a durable pull consumer with one in-flight
message and ACKs only after handler success. The example README documents a
two-terminal run. Live broker execution is pending because the local Docker
daemon is unavailable. Delivery outcomes are explicit: success ACKs; transient
handler errors retry with bounded backoff for five total attempts; invalid
envelopes are copied to a dead-letter subject before source ACK; and a valid
message without a route is terminally rejected. The consumer lease is 45 seconds
with at most one unacknowledged delivery. Worker handlers classify failures as
retryable, permanent, or unknown outcome. Only retryable failures are authorized
to retry; unknown outcomes are preserved in dead-letter records and are not
retried automatically. The envelope propagates an optional Unix-millisecond
deadline; the Worker checks it before execution and times out work at the
deadline. A timeout during execution is recorded as unknown outcome. Queue depth,
unacknowledged work, and concurrent handler execution are bounded by stream
retention and a single in-flight delivery. The Worker stops fetching on Ctrl-C,
then completes or times out its current delivery before exiting; an unacknowledged
delivery after process loss is left for lease-based redelivery.
The example's opt-in tracing feature emits an API publish event and a Worker
handler span with the same correlation and message IDs.

## Live process and failure evidence

On PostgreSQL 18.6 and NATS JetStream 2.11.0, the independent `email-api` and
`email-worker` binaries ran as separate processes. The API ran with only
`NATS_URL`; the Worker also needed only `NATS_URL` and exposes no HTTP listener.
The API's published email was handled by the Worker.

The CreateOrder API committed orders while the publisher was stopped and while
JetStream was unavailable. In both cases the outbox row remained pending and
was published after the publisher or broker returned. One test held the inbox
unique-key lock after delivery, killed the Worker before its transaction could
commit, rolled back the lock holder, and restarted the Worker. JetStream
redelivered the unacknowledged message after the 45-second lease and the retry
committed the inbox and fulfillment once. A second replay was induced by
clearing `published_at` after a successful publish; the Worker logged a
duplicate and did not create another fulfillment. The broker restart also
caused a publish acknowledgement timeout followed by a duplicate delivery,
which the inbox handled safely.

The final temporary database contained four orders, four outbox rows, four
inbox rows, four fulfillment rows, and no pending outbox rows. This verifies
the tested crash windows under the example's PostgreSQL transaction and
JetStream durable-consumer assumptions; it does not imply exactly-once broker
delivery.

A separate policy run injected a PostgreSQL trigger failure on outbox insert.
The API returned the database error, while inventory remained at 100 and both
orders and outbox remained empty, demonstrating rollback of the earlier
inventory update and order insert. Zero quantity and an unknown SKU were also
rejected without creating business rows. Payment timeout persisted
`payment_unknown` and retained the reservation; a definite decline persisted
`declined` and restored exactly one unit. Final policy-run state was two orders,
two outbox rows, and 99 available widgets.

Infrastructure-free tests cover incompatible order schema versions, malformed
order payloads, expired email deadlines, missing routes, retry backoff and
exhaustion, permanent errors, and unknown outcomes. They confirm policy
classification; the live runs above cover process restart, broker downtime,
redelivery, and durable deduplication.

## Phase 7 measurements

The checked-in `measure-jetstream` binary publishes 1,000 messages serially and
awaits each JetStream confirmation. Three runs measured 3,792, 4,510, and 5,238
messages/second (median 4,510) on this local host. This is a
publish-confirmation microbenchmark, not end-to-end Worker throughput. In each
run, a separate stream configured for two messages with `Discard New` accepted
two publishes and rejected the third. The production examples bound email
work to one in-flight delivery and outbox batches to ten rows.

Three one-shot email API runs from process start through broker confirmation
and exit took 11, 6, and 8 ms (median 8 ms). Scheduler drain behavior is covered
by the controlled active-job demonstration and unit test. Three idle email
Worker signal-to-exit runs took 1.0, 1.0, and 1.0 ms; active-job shutdown
latency remains workload-dependent and is not represented by the idle number.

Resolved normal dependency graph sizes from `cargo tree` were 122 packages for
the email-worker example, 201 for CreateOrder, 10 for Scheduler, and 2 for the
capability-only example. These include transitive dependencies and are local
lockfile measurements. The corresponding unstripped debug binaries were 60.6
MB (email API), 68.8 MB (email Worker), 85.8 MB (orders API), 95.6 MB (orders
Worker), and 12.3 MB (scheduler example). These are development artifacts, not
release-size estimates.

## Inspection

The `phase7-inspect` binary in `09-create-order` derives four Kernel process
projections: API, outbox publisher, orders Worker, and scheduler. Its output
includes reachable-module paths and selected resource providers, exact Worker
handler and Scheduler job contribution IDs, the `orders.order-created/v1`
boundary through JetStream, and which process owns its database/broker clients
and shutdown gate. A test checks each of these sections against the blueprint.

The `09-create-order` example adds an atomic PostgreSQL transaction for
inventory decrement, order insert, and serialized outbox intent. A supervised
publisher claims at most ten rows with `FOR UPDATE SKIP LOCKED`, publishes each
message to a bounded JetStream stream, and marks it sent only after broker
confirmation. If the publish succeeds but the database commit fails, the row
remains pending and can publish again. Live database/broker and crash-window
evidence is recorded below.
The same example now includes an `orders-worker` with a durable PostgreSQL inbox.
It commits the inbox ID and fulfillment record atomically, then ACKs the broker
delivery. Duplicate message IDs skip the effect; a transaction failure rolls
both records back. The example compiles and passes Clippy, and crash windows are
covered by the live replay and Worker-crash runs below.
The order example also models payment approval, definite decline, retryable
pre-effect failure, and timeout-as-unknown. Only a definite decline releases the
inventory reservation; an ambiguous timeout records `payment_unknown` without
retrying, while later reconciliation can settle that state explicitly.

The new `rustclamp-scheduler` target validates unique names, positive fixed
intervals, and a 128-job capacity. It defines skip/run-once misfire behavior,
sequential tick execution, and process-local overlap suppression; it does not
claim distributed locking. The `10-scheduler` example composes an unchanged job
operation and advances an injected Core `Clock` through regular and missed
intervals without Tokio or cron. The scheduler crate and example compile cleanly,
pass Clippy with warnings denied, and the controlled-clock example runs. Focused
clock/misfire and shutdown tests also pass. Its shutdown demonstration closes the
admission gate while one job is active, lets that job finish, then confirms the
next due job does not start.

## Verification so far

- `cargo check --offline --locked` passes for an isolated copy of the messaging
  package.
- `cargo fmt -- --check` passes for the messaging package.
- The messaging package's resolved dependency closure passes the boundary policy.
- An isolated Worker package build with its Core and Messaging dependency closure
  passes, and its resolved dependency closure passes the boundary policy.
- The event-mapping example prints the explicit envelope's JSON representation.
- The in-memory bus is bounded and returns a full-queue envelope to the caller
  for retry rather than dropping it.
- The email-worker example runs successfully and passes Clippy with warnings
  denied.
- Both JetStream process projections compile and pass Clippy with warnings
  denied; their separate-process flows also ran against local JetStream.
- The combined email example still runs successfully after the deadline and
  handler-classification changes.
- The optional tracing and default email-worker feature sets compile and pass
  Clippy with warnings denied.
- The CreateOrder/outbox example passes offline Cargo check, Clippy with warnings
  denied, and Cargo metadata resolution. Its crash windows also ran against
  temporary PostgreSQL and JetStream services.
- Controlled-clock Scheduler, Worker retry/deadline, CreateOrder payload/payment,
  and process-inspection tests pass.

## Next

Phase 7 implementation and acceptance evidence are complete. The complete
shared check passed with the nine existing package-metadata warnings; live
services and microbenchmark commands/results are recorded above.
