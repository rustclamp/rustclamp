# ADR 0035: Scheduled tasks in web apps

Status: Proposed, 2026-10-07.

## Context

A `rustclamp` web app can't run anything on a schedule (rustclamp/rustclamp#120).
Pruning old sessions, a nightly report or a retry sweep needs its own process
or a system cron line that calls a console command. Laravel apps in the fleet
do it with one cron line, `* * * * * php artisan schedule:run`, and the
schedule lives in the app's code. That is the model Lex knows.

The issue asks for this "via the scheduler crate". `rustclamp-scheduler`
doesn't fit the web app:

- **It is async.** A `JobDeclaration` handler returns a boxed `Future`, and
  `Scheduler::tick` is an `async fn`. The driver, `Scheduler::run_until`, is
  the crate's `tokio` feature (ADR 0020, item 3). `web::App` is std-only with
  blocking handlers on threads. ADRs 0019 and 0026 turned down the facade ->
  Tokio edge, and nothing here changes that argument.
- **Its schedule is intervals from process start.** A job runs every
  `Duration`, first on the first tick, with `next_run` kept in memory. "Daily
  at 03:00 UTC" can't be expressed, and the next run moves with every restart.
- **It is per process.** Each instance keeps its own `next_run` and its own
  "already running" flag, so N instances run each job N times.
- **It isn't published.** It reaches `rustclamp-core` by a relative path and
  has `publish = false` (#84), so an app built from Git can't depend on it
  through the facade.

The crate stays what ADR 0020 made it: the contribution target and tick
driver for async services built on the kernel. The web app needs the same
idea with std threads, wall-clock times and a database lock, as the queue
(ADR 0019) did next to the worker crate.

## Decision

- **An optional `schedule` feature** (`web`, `db`) with `rustclamp::schedule`.
  It adds no dependency and no crate, and does not depend on
  `rustclamp-scheduler`. It borrows the crate's vocabulary where it matches:
  a task has a name, a missed slot is skipped (the crate's
  `MisfirePolicy::Skip`), and a task never overlaps itself. The issue should
  be retitled "Run scheduled tasks from `web::App`".
- **Tasks on the app.** `App` gets one more field, like `commands`:
  `schedule: &'static [Task]`, kept in `app/console/schedule.rs` in the web
  template.

  ```rust
  pub const SCHEDULE: &[Task] = &[
      Task { name: "sessions:prune", at: At::Hourly { minute: 0 }, run: prune },
      Task { name: "report:daily", at: At::Daily { hour: 3, minute: 0 }, run: report },
  ];

  fn prune(config: &Config, db: &Db) -> Result<(), String> { .. }
  ```

  A task's action is one `fn(&Config, &Db) -> Result<(), String>`, the
  console command's arguments without `args`. Running a console command is
  calling its `run` from that function. Dispatching a queue job from a task
  is open question 2. A panic counts as a failure. Failures are logged as
  errors with the task's name. Names must be unique, checked on the first
  run like command names.
- **Frequencies, all in UTC:** `At::EveryMinutes(n)` (n divides 60:
  1, 5, 15, 30), `At::Hourly { minute }`, `At::Daily { hour, minute }` and
  `At::Weekly { day, hour, minute }`. Those cover what the fleet's Laravel
  schedules use. **No cron expressions**: a parser, and five fields to get
  wrong, for schedules these four already express. They can be added as
  `At::Cron("...")` later without changing the rest. `rustclamp::time` gains
  the UTC minute, hour and weekday of a `SystemTime`.
- **Slots.** The schedule works in whole UTC minutes. A task is due when the
  current minute matches its `At`. Nothing runs between minutes, and a task
  never runs twice in one minute.
- **Where it runs.**
  - `schedule:run` is a built-in console command: run the tasks due in the
    current minute, one after another in name order, then exit 0 (1 if one
    failed). Production runs it from system cron or a systemd timer every
    minute, Laravel style.
  - Outside production, `App::run` starts one schedule thread beside the
    HTTP server, like the mail thread. It sleeps to the next minute and runs
    that minute's tasks. `SCHEDULE_THREAD` (`true` by default, `false` when
    `APP_ENV=production`) turns it on or off.
  - No `schedule:work` long-running command. In production, cron starts
    `schedule:run`, or `SCHEDULE_THREAD=true` runs the thread in the web
    process (open question 1). A third way waits until a deploy needs it.
- **Once per slot, across instances.** A framework table, created like
  `jobs` and `mail_outbox`:

  ```sql
  CREATE TABLE IF NOT EXISTS schedule_runs (
      task TEXT PRIMARY KEY,
      slot INTEGER NOT NULL,    -- the claimed minute, seconds since the epoch / 60
      started_at INTEGER NOT NULL,
      finished_at INTEGER,
      error TEXT
  )
  ```

  Before a task runs, the runner inserts its row if missing (`slot` -1, `finished_at` set) and claims the
  slot with one statement:
  `UPDATE schedule_runs SET slot = ?, started_at = ?, finished_at = NULL, error = NULL WHERE task = ? AND slot < ? AND (finished_at IS NOT NULL OR started_at < ?)`.
  Only the instance whose update changes a row runs the task. Two web
  instances with the thread on, a cron line on two servers, or a dev thread
  plus a manual `schedule:run` all run each task once per slot. The
  statement updates one existing row, so it is atomic under SQLite's single
  writer and under Postgres `READ COMMITTED` (the row lock re-checks the
  `WHERE`), which keeps it valid if ADR 0034 lands. The row also records the
  last run and its error.
- **No overlap.** The second half of the `WHERE` skips a slot while the
  task's previous run hasn't finished, as Laravel's `withoutOverlapping()`
  does but on by default. A run whose process died never sets
  `finished_at`, so a run older than `SCHEDULE_STALE_AFTER` (default 1 hour)
  counts as dead: the next slot takes over and logs a warning. A task that
  takes longer than that should dispatch a queue job instead.
- **Missed slots are skipped, not caught up.** If the app is down at 03:00,
  the 03:00 run doesn't happen. That is what cron and Laravel do, and what
  the crate's `MisfirePolicy::Skip` does. A catch-up after a long outage
  would fire every daily and hourly task at once on startup. A task that
  must not miss a period (a daily cleanup) should work on "everything since
  the last run" rather than "yesterday", so the next run covers the gap.
- **Tests.** `App::test` starts no thread. A test calls
  `app.schedule(&config, db).run_due(now)` with a given clock, like
  `Queue::work_once(now)`. It returns each task that ran with its result, so
  a test can check that `report:daily` runs at 03:00 on day one and not
  again until 03:00 on day two.
- **Not included:**
  - cron expressions, seconds, time zones other than UTC and
    daylight-saving rules;
  - per-task options: `without_overlapping(false)`, run on one server only
    (the lock already does that), `between`, environment filters, ping
    URLs;
  - running a task in the background of `schedule:run`: tasks run one after
    another, and a slow one belongs on the queue;
  - `schedule:list` and `schedule:test`;
  - catching up missed slots.

  Each one waits until an app needs it.

## Consequences

- A production deploy of an app with tasks needs one cron line or systemd
  timer per app that runs the binary with `schedule:run` every minute, next
  to the `queue:work` service when the app uses the queue. Without it,
  nothing runs. A startup warning when tasks exist, `SCHEDULE_THREAD` is off
  and `schedule_runs` hasn't changed for an hour can surface that; it is
  left to the implementation PR.
- No change to `tools/boundaries.py`: no new crate and no new dependency.
  The facade still doesn't depend on `rustclamp-scheduler`, so #84 doesn't
  block this.
- The repository has two schedulers, as it has two queues: this one for std
  web apps and the crate for async kernel services. They share names and
  the skip and no-overlap rules, not code. A task written for one does not
  run on the other.
- Tasks run one after another in one `schedule:run`, so a slow task at
  03:00 delays the others due that minute. They still run, because their
  slot is already claimed by this process.

## Open questions

1. **Production runner.** Cron or systemd timer with `schedule:run`
   (recommended: familiar, survives a web restart at 03:00, the Ansible role
   adds one line), or the thread in every web process with
   `SCHEDULE_THREAD=true` (no extra unit; the lock keeps it to one run per
   slot, but a deploy restart at the due minute skips the slot)?
2. **Dispatching jobs.** Should a task get the app's queue, for example
   `run: fn(&Context)` with `config()`, `db()` and `queue()`, so it can
   dispatch heavy work? Or is `fn(&Config, &Db)` enough, with the task
   building what it needs? The first changes the signature away from
   `Command`'s.
3. **Frequencies.** Are the four `At` forms enough for the fleet's current
   Laravel schedules, or is there a `->cron(...)` or a time-zone schedule
   that must move over?
4. **`SCHEDULE_STALE_AFTER`.** Is one hour right for the fleet, or should
   the timeout be per task?
