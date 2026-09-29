//! Drives a compiled scheduler with a controlled wall clock.

use rustclamp_core::{Clock, ModuleId, Qualifier, QualifierId};
use rustclamp_kernel::TargetComposition;
use rustclamp_scheduler::{JobDeclaration, MisfirePolicy, SchedulerTarget};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

const CLEANUP_MODULE: ModuleId = ModuleId::new("example.scheduler.cleanup");
const PRUNE_MODULE: ModuleId = ModuleId::new("example.scheduler.prune");

struct ScheduledJobs;

impl Qualifier for ScheduledJobs {
    const ID: QualifierId = QualifierId::new("example.scheduler.jobs");
}

struct ManualClock(Mutex<SystemTime>);

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        self.0
            .lock()
            .map(|now| *now)
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

fn main() {
    let catch_up_job = JobDeclaration::new(
        "cleanup-expired-sessions",
        Duration::from_secs(10),
        MisfirePolicy::RunOnce,
        || async {
            println!("cleanup operation invoked");
            Ok(())
        },
    );
    let skip_missed_job = JobDeclaration::new(
        "prune-old-sessions",
        Duration::from_secs(10),
        MisfirePolicy::Skip,
        || async {
            println!("prune operation invoked");
            Ok(())
        },
    );
    let scheduler = TargetComposition::<SchedulerTarget, ScheduledJobs>::new(vec![
        (CLEANUP_MODULE, catch_up_job),
        (PRUNE_MODULE, skip_missed_job),
    ])
    .build(Some(&SchedulerTarget))
    .expect("job declarations are valid")
    .expect("selected target produces a scheduler");

    let clock = ManualClock(Mutex::new(SystemTime::UNIX_EPOCH));
    let first = futures_executor::block_on(scheduler.tick(&clock));
    *clock.0.lock().expect("clock is not poisoned") += Duration::from_secs(10);
    let second = futures_executor::block_on(scheduler.tick(&clock));
    *clock.0.lock().expect("clock is not poisoned") += Duration::from_secs(40);
    let after_misfire = futures_executor::block_on(scheduler.tick(&clock));
    println!(
        "invocations: {}, {}, {}; skipped misfires: {}",
        first.invoked, second.invoked, after_misfire.invoked, after_misfire.misfires_skipped
    );

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = Arc::clone(&gate);
    let (started, waiting) = mpsc::channel();
    let active_job = JobDeclaration::new(
        "active-job",
        Duration::from_secs(1),
        MisfirePolicy::RunOnce,
        move || {
            let gate = Arc::clone(&worker_gate);
            let started = started.clone();
            async move {
                started.send(()).expect("main thread is waiting");
                let (lock, wake) = &*gate;
                let mut released = lock.lock().expect("gate is not poisoned");
                while !*released {
                    released = wake.wait(released).expect("gate is not poisoned");
                }
                Ok(())
            }
        },
    );
    let tail_calls = Arc::new(AtomicUsize::new(0));
    let job_calls = Arc::clone(&tail_calls);
    let tail_job = JobDeclaration::new(
        "queued-job",
        Duration::from_secs(1),
        MisfirePolicy::RunOnce,
        move || {
            let calls = Arc::clone(&job_calls);
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        },
    );
    let draining = Arc::new(
        TargetComposition::<SchedulerTarget, ScheduledJobs>::new(vec![
            (CLEANUP_MODULE, active_job),
            (PRUNE_MODULE, tail_job),
        ])
        .build(Some(&SchedulerTarget))
        .expect("job declarations are valid")
        .expect("selected target produces a scheduler"),
    );
    let drain_clock = Arc::new(ManualClock(Mutex::new(SystemTime::UNIX_EPOCH)));
    let worker_scheduler = Arc::clone(&draining);
    let worker_clock = Arc::clone(&drain_clock);
    let tick =
        thread::spawn(move || futures_executor::block_on(worker_scheduler.tick(&*worker_clock)));
    waiting.recv().expect("active job started");
    draining.stop_admission();
    *gate.0.lock().expect("gate is not poisoned") = true;
    gate.1.notify_one();
    let drained = tick.join().expect("active tick completed");
    println!(
        "shutdown drained {}; later invocations: {}",
        drained.invoked,
        futures_executor::block_on(draining.tick(&*drain_clock)).invoked
    );
}
