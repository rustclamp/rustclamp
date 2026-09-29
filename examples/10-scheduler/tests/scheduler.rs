//! Deterministic clock, misfire, admission, failure, and overlap behavior.

use rustclamp_core::{Clock, ContributionTarget, ModuleId};
use rustclamp_scheduler::{JobDeclaration, MisfirePolicy, SchedulerTarget};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

struct TestClock(Mutex<SystemTime>);

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().expect("clock lock")
    }
}

fn clock() -> TestClock {
    TestClock(Mutex::new(SystemTime::UNIX_EPOCH))
}

fn advance(clock: &TestClock, duration: Duration) {
    let mut now = clock.0.lock().expect("clock lock");
    *now += duration;
}

#[test]
fn controlled_clock_applies_run_once_and_skip_misfire_policies() {
    let run_once = JobDeclaration::new(
        "run-once",
        Duration::from_secs(10),
        MisfirePolicy::RunOnce,
        || async { Ok(()) },
    );
    let skip = JobDeclaration::new(
        "skip",
        Duration::from_secs(10),
        MisfirePolicy::Skip,
        || async { Ok(()) },
    );
    let scheduler = SchedulerTarget
        .build(&[
            (ModuleId::new("test.run-once"), run_once),
            (ModuleId::new("test.skip"), skip),
        ])
        .expect("valid schedules");
    let clock = clock();

    assert_eq!(
        futures_executor::block_on(scheduler.tick(&clock)).invoked,
        2
    );
    advance(&clock, Duration::from_secs(10));
    assert_eq!(
        futures_executor::block_on(scheduler.tick(&clock)).invoked,
        2
    );
    advance(&clock, Duration::from_secs(40));
    let late = futures_executor::block_on(scheduler.tick(&clock));

    assert_eq!(late.invoked, 1);
    assert_eq!(late.misfires_skipped, 1);
}

#[test]
fn stopped_admission_prevents_later_due_jobs() {
    let ran = Arc::new(Mutex::new(false));
    let ran_by_job = Arc::clone(&ran);
    let job = JobDeclaration::new(
        "job",
        Duration::from_secs(1),
        MisfirePolicy::RunOnce,
        move || {
            let ran = Arc::clone(&ran_by_job);
            async move {
                *ran.lock().expect("ran lock") = true;
                Ok(())
            }
        },
    );
    let scheduler = SchedulerTarget
        .build(&[(ModuleId::new("test.job"), job)])
        .expect("valid schedule");
    scheduler.stop_admission();

    let report = futures_executor::block_on(scheduler.tick(&clock()));
    assert!(report.admission_stopped);
    assert_eq!(report.invoked, 0);
    assert!(!*ran.lock().expect("ran lock"));
}

#[test]
fn failed_jobs_can_run_again_and_overlapping_ticks_skip_active_job() {
    let should_fail = Arc::new(Mutex::new(true));
    let fail_flag = Arc::clone(&should_fail);
    let failing = JobDeclaration::new(
        "fails-once",
        Duration::from_secs(1),
        MisfirePolicy::RunOnce,
        move || {
            let fail = Arc::clone(&fail_flag);
            async move {
                if std::mem::take(&mut *fail.lock().expect("failure lock")) {
                    Err(std::io::Error::other("expected test failure").into())
                } else {
                    Ok(())
                }
            }
        },
    );
    let failed_scheduler = SchedulerTarget
        .build(&[(ModuleId::new("test.failing"), failing)])
        .expect("valid schedule");
    let failure_clock = clock();
    let failed = futures_executor::block_on(failed_scheduler.tick(&failure_clock));
    assert_eq!(failed.failed, 1);
    advance(&failure_clock, Duration::from_secs(1));
    let retried = futures_executor::block_on(failed_scheduler.tick(&failure_clock));
    assert_eq!(retried.failed, 0);

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = Arc::clone(&gate);
    let (started, waiting) = mpsc::channel();
    let active = JobDeclaration::new(
        "active",
        Duration::from_secs(1),
        MisfirePolicy::RunOnce,
        move || {
            let gate = Arc::clone(&worker_gate);
            let started = started.clone();
            async move {
                started.send(()).expect("test is waiting");
                let (lock, wake) = &*gate;
                let mut released = lock.lock().expect("gate lock");
                while !*released {
                    released = wake.wait(released).expect("gate lock");
                }
                Ok(())
            }
        },
    );
    let scheduler = Arc::new(
        SchedulerTarget
            .build(&[(ModuleId::new("test.active"), active)])
            .expect("valid schedule"),
    );
    let clock = Arc::new(clock());

    let ticking_scheduler = Arc::clone(&scheduler);
    let ticking_clock = Arc::clone(&clock);
    let active_tick =
        thread::spawn(move || futures_executor::block_on(ticking_scheduler.tick(&*ticking_clock)));
    waiting.recv().expect("active job started");
    advance(&clock, Duration::from_secs(1));
    let overlap = futures_executor::block_on(scheduler.tick(&*clock));
    assert_eq!(overlap.overlaps_skipped, 1);

    *gate.0.lock().expect("gate lock") = true;
    gate.1.notify_one();
    assert_eq!(active_tick.join().expect("active tick finished").invoked, 1);
}
