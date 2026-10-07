//! Scheduled tasks (ADR 0035), with the `schedule` feature.
//!
//! The app lists its tasks ([`App::schedule`](crate::web::App::schedule)),
//! each with a name, a UTC time ([`At`]) and a `fn(&Config, &Db)`. The
//! schedule works in whole UTC minutes: a task runs in the minutes its `At`
//! matches, at most once per minute across every process sharing the
//! database, and never while its previous run is unfinished. A minute when
//! nothing ran is skipped, not caught up. A failure (an `Err` or a panic) is
//! logged as an error with the task's name.
//!
//! Where it runs:
//!
//! - `cargo run -- schedule:run` (the built binary in production) runs the
//!   tasks due this minute, one after another in name order, and exits 1 if
//!   one failed. Production runs it every minute from cron or a systemd
//!   timer: `* * * * * cd /srv/app && ./app schedule:run`.
//! - `App::run` starts a schedule thread beside the server when
//!   `SCHEDULE_THREAD` is true: by default outside production only.
//!
//! Settings:
//!
//! - `SCHEDULE_THREAD`: run tasks in the web process (`true`, or `false` when
//!   `APP_ENV=production`).
//! - `SCHEDULE_STALE_AFTER`: seconds after which an unfinished run counts as
//!   dead and the next minute runs the task again (3600). A task that takes
//!   longer should dispatch a queue job instead.
//!
//! ```
//! use rustclamp::config::Config;
//! use rustclamp::db::Db;
//! use rustclamp::schedule::{At, Task};
//!
//! pub const SCHEDULE: &[Task] = &[
//!     Task { name: "sessions:prune", at: At::Hourly { minute: 0 }, run: prune },
//!     Task { name: "report:daily", at: At::Daily { hour: 3, minute: 0 }, run: prune },
//! ];
//!
//! fn prune(_config: &Config, _db: &Db) -> Result<(), String> {
//!     Ok(())
//! }
//! ```

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::db::Db;
use crate::db::sqlite::{self, params};
use crate::log::Log;
use crate::time::{self, Weekday};

/// One scheduled task.
#[derive(Debug, Clone, Copy)]
pub struct Task {
    /// Unique among the app's tasks; logs and `schedule_runs` use it.
    pub name: &'static str,
    /// When it runs, in UTC.
    pub at: At,
    /// Runs it: `Ok` when done, `Err` (or a panic) for a failure, which is
    /// logged. Running a console command is calling its `run` from here.
    pub run: fn(&Config, &Db) -> Result<(), String>,
}

/// When a task runs, in UTC. No cron expressions, time zones or seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// Every `n` minutes from the full hour; `n` divides 60 (1, 5, 15, 30).
    EveryMinutes(u32),
    /// Once an hour, at `minute`.
    Hourly {
        /// 0 to 59.
        minute: u32,
    },
    /// Once a day.
    Daily {
        /// 0 to 23.
        hour: u32,
        /// 0 to 59.
        minute: u32,
    },
    /// Once a week.
    Weekly {
        /// The day of the week.
        day: Weekday,
        /// 0 to 23.
        hour: u32,
        /// 0 to 59.
        minute: u32,
    },
}

impl At {
    /// Whether the UTC minute of `now` is one of this schedule's.
    pub fn is_due(self, now: SystemTime) -> bool {
        let clock = time::hour_minute(now);
        match self {
            Self::EveryMinutes(n) => n > 0 && clock.1.is_multiple_of(n),
            Self::Hourly { minute } => clock.1 == minute,
            Self::Daily { hour, minute } => clock == (hour, minute),
            Self::Weekly { day, hour, minute } => {
                time::weekday(now) == day && clock == (hour, minute)
            }
        }
    }

    fn is_valid(self) -> bool {
        let clock = |hour: u32, minute: u32| hour < 24 && minute < 60;
        match self {
            Self::EveryMinutes(n) => n > 0 && 60_u32.is_multiple_of(n),
            Self::Hourly { minute } => clock(0, minute),
            Self::Daily { hour, minute } | Self::Weekly { hour, minute, .. } => clock(hour, minute),
        }
    }
}

/// The app's tasks on its database. Build one with
/// [`App::schedule`](crate::web::App::schedule).
#[derive(Debug, Clone)]
pub struct Schedule {
    tasks: &'static [Task],
    config: Config,
    db: Db,
    stale_after: i64,
}

impl Schedule {
    /// `tasks` on `db`, with `SCHEDULE_STALE_AFTER` from `config`.
    ///
    /// # Panics
    ///
    /// When two tasks share a name, an [`At`] is out of range, or
    /// `SCHEDULE_STALE_AFTER` is set but not a number.
    pub fn new(tasks: &'static [Task], config: &Config, db: Db) -> Self {
        for (index, task) in tasks.iter().enumerate() {
            assert!(
                !tasks[..index].iter().any(|other| other.name == task.name),
                "scheduled task {:?} is listed twice",
                task.name
            );
            assert!(
                task.at.is_valid(),
                "scheduled task {:?}: {:?} is out of range",
                task.name,
                task.at
            );
        }
        Self {
            tasks,
            config: config.clone(),
            db,
            stale_after: config.get_or("SCHEDULE_STALE_AFTER", 3600),
        }
    }

    /// Runs the tasks due in the UTC minute of `now` that no other run has
    /// claimed, one after another in name order, and returns each that ran
    /// with its result. Tests call it with their own clock.
    ///
    /// # Errors
    ///
    /// A database error; the tasks after it don't run.
    pub fn run_due(
        &self,
        now: SystemTime,
    ) -> sqlite::Result<Vec<(&'static str, Result<(), String>)>> {
        let now = seconds(now);
        let slot = now.div_euclid(60);
        self.db.with(create_table)?;
        let mut due: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|task| task.at.is_due(time_at(now)))
            .collect();
        due.sort_by_key(|task| task.name);
        let mut ran = Vec::new();
        for task in due {
            if !self.claim(task.name, slot, now)? {
                continue;
            }
            let result = crate::web::catch_panic(|| (task.run)(&self.config, &self.db));
            if let Err(problem) = &result {
                Log::error(format_args!(
                    "schedule: task {} failed: {problem}",
                    task.name
                ));
            }
            self.db.with(|sql| {
                sql.execute(
                    "UPDATE schedule_runs SET finished_at = ?1, error = ?2 WHERE task = ?3 AND slot = ?4",
                    params![seconds(SystemTime::now()), result.as_ref().err(), task.name, slot],
                )
            })?;
            ran.push((task.name, result));
        }
        Ok(ran)
    }

    /// Claims `slot` for `task`: true when this call may run it.
    fn claim(&self, task: &str, slot: i64, now: i64) -> sqlite::Result<bool> {
        self.db.with(|sql| {
            sql.execute(
                "INSERT OR IGNORE INTO schedule_runs (task, slot, started_at, finished_at) VALUES (?1, -1, 0, 0)",
                params![task],
            )?;
            let (last, started_at, finished): (i64, i64, Option<i64>) = sql.query_row(
                "SELECT slot, started_at, finished_at FROM schedule_runs WHERE task = ?1",
                params![task],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            // One statement decides, so two processes never both win a slot.
            let claimed = sql.execute(
                "UPDATE schedule_runs SET slot = ?1, started_at = ?2, finished_at = NULL, error = NULL \
                 WHERE task = ?3 AND slot < ?1 AND (finished_at IS NOT NULL OR started_at < ?4)",
                params![slot, now, task, now - self.stale_after],
            )? == 1;
            if last < slot && finished.is_none() {
                let started = time::format_rfc3339(time_at(started_at));
                if claimed {
                    Log::warning(format_args!(
                        "schedule: task {task} started at {started} never finished; running it again"
                    ));
                } else {
                    Log::warning(format_args!(
                        "schedule: task {task} still running since {started}; skipped this minute"
                    ));
                }
            }
            Ok(claimed)
        })
    }

    /// Starts a thread that runs each minute's due tasks, a moment after the
    /// minute begins. Nothing stops it; a task cut off by an exit runs again
    /// after `SCHEDULE_STALE_AFTER`.
    pub fn start(self) {
        std::thread::spawn(move || {
            loop {
                let into_minute = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() % 60_000)
                    as u64;
                std::thread::sleep(Duration::from_millis(60_000 - into_minute));
                if let Err(problem) = self.run_due(SystemTime::now()) {
                    Log::error(format_args!("schedule: {problem}"));
                }
            }
        });
    }

    /// Unix seconds of the latest run, if any task ever ran.
    pub(crate) fn last_run(&self) -> sqlite::Result<Option<i64>> {
        self.db.with(|sql| {
            create_table(sql)?;
            sql.query_row(
                "SELECT MAX(started_at) FROM schedule_runs WHERE slot >= 0",
                [],
                |row| row.get(0),
            )
        })
    }
}

/// Whether `App::run` runs the schedule in its own process:
/// `SCHEDULE_THREAD`, by default true, or false when `APP_ENV=production`.
///
/// # Panics
///
/// When `SCHEDULE_THREAD` is set but not `true` or `false`.
pub fn thread(config: &Config) -> bool {
    config.get_or("SCHEDULE_THREAD", !config.is_production())
}

fn create_table(sql: &sqlite::Connection) -> sqlite::Result<()> {
    sql.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedule_runs (
            task TEXT PRIMARY KEY,
            slot INTEGER NOT NULL,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            error TEXT
        )",
    )
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn time_at(seconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(_: &Config, db: &Db) -> Result<(), String> {
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE IF NOT EXISTS done (n INTEGER); INSERT INTO done VALUES (1)",
            )
        })
        .map_err(|error| error.to_string())
    }

    fn fail(_: &Config, _: &Db) -> Result<(), String> {
        Err("nope".into())
    }

    fn boom(_: &Config, _: &Db) -> Result<(), String> {
        panic!("kaboom")
    }

    const TASKS: &[Task] = &[
        Task {
            name: "report:daily",
            at: At::Daily { hour: 3, minute: 0 },
            run: record,
        },
        Task {
            name: "fails",
            at: At::EveryMinutes(15),
            run: fail,
        },
        Task {
            name: "panics",
            at: At::EveryMinutes(15),
            run: boom,
        },
    ];

    fn schedule(tasks: &'static [Task]) -> (Schedule, Db) {
        let config = Config::parse("DB_DATABASE=:memory:\n");
        let db = Db::open(&config);
        (Schedule::new(tasks, &config, db.clone()), db)
    }

    fn at(text: &str) -> SystemTime {
        time::parse_rfc3339(text).unwrap()
    }

    fn names(ran: &[(&'static str, Result<(), String>)]) -> Vec<&'static str> {
        ran.iter().map(|(name, _)| *name).collect()
    }

    #[test]
    fn frequencies_match_their_minutes() {
        let monday = at("2026-10-05T03:00:00Z");
        assert!(At::EveryMinutes(15).is_due(at("2026-10-05T03:45:30Z")));
        assert!(!At::EveryMinutes(15).is_due(at("2026-10-05T03:46:00Z")));
        assert!(At::Hourly { minute: 0 }.is_due(monday));
        assert!(At::Daily { hour: 3, minute: 0 }.is_due(monday));
        assert!(!At::Daily { hour: 4, minute: 0 }.is_due(monday));
        let weekly = |day| At::Weekly {
            day,
            hour: 3,
            minute: 0,
        };
        assert!(weekly(Weekday::Monday).is_due(monday));
        assert!(!weekly(Weekday::Tuesday).is_due(monday));
    }

    #[test]
    fn a_daily_task_runs_once_a_day() {
        let (schedule, db) = schedule(TASKS);
        let ran = schedule.run_due(at("2026-10-05T03:00:00Z")).unwrap();
        // 03:00 is also a quarter hour; name order, failures reported.
        assert_eq!(names(&ran), ["fails", "panics", "report:daily"]);
        assert_eq!(ran[0].1, Err("nope".into()));
        assert_eq!(ran[1].1, Err("panicked: kaboom".into()));
        assert_eq!(ran[2].1, Ok(()));
        let again = schedule.run_due(at("2026-10-05T03:00:59Z")).unwrap();
        assert!(again.is_empty(), "once per minute");
        assert!(
            schedule
                .run_due(at("2026-10-05T03:01:00Z"))
                .unwrap()
                .is_empty()
        );
        let next_day = schedule.run_due(at("2026-10-06T03:00:00Z")).unwrap();
        assert!(names(&next_day).contains(&"report:daily"));
        let done: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM done", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(done, 2);
        let error: Option<String> = db
            .with(|c| {
                c.query_row(
                    "SELECT error FROM schedule_runs WHERE task = 'fails'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(error.as_deref(), Some("nope"));
    }

    #[test]
    fn two_processes_run_a_slot_once() {
        let (first, db) = schedule(TASKS);
        let second = Schedule::new(TASKS, &Config::parse(""), db);
        let now = at("2026-10-05T03:00:00Z");
        assert_eq!(first.run_due(now).unwrap().len(), 3);
        assert!(second.run_due(now).unwrap().is_empty());
    }

    #[test]
    fn an_unfinished_run_blocks_until_it_is_stale() {
        let (schedule, db) = schedule(TASKS);
        let start = at("2026-10-05T03:00:00Z");
        schedule.run_due(start).unwrap();
        // The 03:00 run of `fails` never finished, as if its process died.
        db.with(|c| {
            c.execute(
                "UPDATE schedule_runs SET finished_at = NULL WHERE task = 'fails'",
                [],
            )
        })
        .unwrap();
        let quarter = schedule.run_due(at("2026-10-05T03:15:00Z")).unwrap();
        assert_eq!(names(&quarter), ["panics"]);
        let after_an_hour = schedule.run_due(at("2026-10-05T04:15:00Z")).unwrap();
        assert_eq!(names(&after_an_hour), ["fails", "panics"]);
    }

    #[test]
    fn schedule_run_is_a_built_in_command() {
        const EVERY_MINUTE: &[Task] = &[Task {
            name: "record",
            at: At::EveryMinutes(1),
            run: record,
        }];
        let app = crate::web::App {
            logging: crate::log::Settings::from_config,
            database: crate::db::Settings::from_config,
            filesystems: |_| unreachable!("no disks"),
            migrations: Vec::new,
            seeders: Vec::new,
            routes: |router, _, _| router,
            commands: &[],
            #[cfg(feature = "auth")]
            roles: &["admin", "user"],
            #[cfg(feature = "mail")]
            mail: crate::mail::Settings::from_config,
            #[cfg(feature = "queue")]
            jobs: Vec::new,
            schedule: EVERY_MINUTE,
        };
        let config = Config::parse("DB_DATABASE=:memory:\n");
        let db = Db::open(&config);
        assert!(app.list().contains("schedule:run"));
        assert_eq!(app.command(&config, &db, "schedule:run", &[]), 0);
        let done: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM done", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(done, 1);
    }

    #[test]
    #[should_panic(expected = "listed twice")]
    fn names_are_unique() {
        const TWICE: &[Task] = &[
            Task {
                name: "a",
                at: At::EveryMinutes(1),
                run: record,
            },
            Task {
                name: "a",
                at: At::EveryMinutes(5),
                run: record,
            },
        ];
        schedule(TWICE);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn every_minutes_divides_an_hour() {
        const SEVEN: &[Task] = &[Task {
            name: "a",
            at: At::EveryMinutes(7),
            run: record,
        }];
        schedule(SEVEN);
    }
}
