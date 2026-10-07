# ADR 0019: Queue

Status: Proposed, 2026-10-07.

## Context

A `rustclamp` web app has no way to run work after the response: resizing an
upload, calling a slow API, sending a webhook. Today a handler does it inline,
so the visitor waits and a failure becomes a 500, or the app spawns a thread
and loses the work on a restart.

Two queues exist nearby, and neither fits:

- **The worker crate** (ADRs 0020 and 0026) has `WorkerService` and a durable
  `SqliteQueue`. Both are async and run on Tokio. `web::App` is std-only, with
  blocking handlers on threads. Using them would add `rustclamp-worker`,
  `rustclamp-runtime` and `tokio` to the facade. ADR 0026's table already
  argues against those edges.
- **The mail outbox** (ADR 0015) is a background thread that delivers rows
  from one framework table. Its daily cap and once-a-minute pace are specific
  to mail.

The phase 11 plan (2026-09-30) chose a sync, std-only queue in the facade, on
the same pattern as the mail outbox.

## Decision

- **An optional `queue` feature** (`web`, `db`), with `rustclamp::queue`. It
  adds no new dependency. The Redis driver also needs the `redis` feature
  (ADR 0027).
- **Named handlers on the app.** `App::job("thumbnail", |job| ...)` registers
  a handler for a name. The handler gets the payload as a `&str` (the app
  chooses the format, usually JSON) and the attempt number, and returns
  `Result<(), String>`. A panic counts as a failure.
- **Dispatch from a handler.** `request.queue().dispatch(name, payload)` and
  `dispatch_later(name, payload, delay)` store the job and return a
  `Result`: when the driver can't be reached, the caller decides what to do.
  Dispatching a name that has no handler is an error at dispatch time, not a
  dead job later.
- **At-least-once delivery.** A worker reserves a job before running it. A
  reservation older than `QUEUE_RETRY_AFTER` (default 90 s) makes the job
  available again, so a job survives a crash or a kill. A job can therefore
  run twice, and handlers must be idempotent. A std thread can't be stopped,
  so a handler that runs longer than `QUEUE_RETRY_AFTER` can run alongside
  its own retry. The docs say to set the value above the slowest job.
- **Retries, then `failed_jobs`.** A failed attempt is retried after a
  backoff (10 s, 60 s, 300 s), up to `QUEUE_TRIES` attempts in total
  (default 3). After the last attempt, the job moves to the `failed_jobs`
  table in the app's database (name, payload, error, failed_at) and the
  failure is logged as an error. `failed_jobs` lives in the database for both
  drivers, so failures survive a Redis flush. Retrying a failed job, or
  forgetting it, waits for console commands (#122).
- **Two drivers, chosen by `QUEUE_CONNECTION`** (`database` by default, or
  `redis`).
  - **database:** a framework-owned `jobs` table (id, name, payload,
    attempts, available_at, reserved_at), created by the framework's
    migrations like `mail_outbox`. Claiming is one `UPDATE ... RETURNING` on
    the oldest available row, so concurrent workers never take the same job.
    Workers poll once a second while the table is empty.
  - **redis:** built on the existing `rustclamp::redis::Redis`. A job is a
    list item, delayed jobs are in a sorted set by due time, and reserved
    jobs are in a sorted set by reservation time. One Lua script moves due
    delayed jobs and expired reservations back to the list, then pops and
    reserves the next job, so it is atomic. The Redis client is one
    connection behind a mutex, so each worker thread opens its own
    connection and polls rather than blocking on `BLPOP`. Keys start with
    `REDIS_PREFIX`. When `APP_ENV=production`, the app refuses to start the
    queue without a prefix, because the fleet shares Redis servers and two
    apps would otherwise run each other's jobs.
- **Where workers run.**
  - Outside production, `App::run` starts `QUEUE_WORKERS` threads (default 1)
    beside the HTTP server, like the mail thread.
  - In production, `QUEUE_WORKERS` defaults to 0. The same binary, started
    with the argument `queue:work`, runs only the workers. Deploys run it as
    its own service, so a slow job never takes HTTP threads and the two can
    restart separately. `queue:work` is a fixed argument for now; it becomes
    an ordinary app command when #122 lands.
  - On shutdown a worker stops taking jobs and finishes the one it is
    running. A job cut off by a kill is retried after `QUEUE_RETRY_AFTER`.
- **Tests.** `App::test` starts no worker. A test dispatches through the app,
  then calls `queue.work_once()` to run the next due job on the test thread,
  with a given clock, as mail tests call the delivery step directly.
- **Not included:**
  - several named queues and priorities: a job's name already routes it;
  - unique jobs, chains, batches and per-job rate limits;
  - a `sync` driver: tests use `work_once`;
  - a dashboard;
  - moving mail onto the queue: the outbox keeps its daily cap.

  Each one waits until an app needs it.

## Consequences

- The repository has two queues: this one for std web apps, and the worker
  crate's for async services. They share no code. A job written for one does
  not run on the other.
- The `queue` feature needs no change to `tools/boundaries.py`: it adds no
  crate and no dependency.
- Production deploys of an app that dispatches jobs need a second service
  running `queue:work`. Without it, jobs pile up in the `jobs` table or the
  Redis list and nothing runs them. `clamp doctor`, or a startup warning when
  `QUEUE_WORKERS=0` and jobs are waiting, can surface that; it is left to the
  implementation PR.
- The Redis driver gets the same CI coverage as ADR 0027: a fake RESP server
  in unit tests and a live test against the `redis:8-alpine` service.
