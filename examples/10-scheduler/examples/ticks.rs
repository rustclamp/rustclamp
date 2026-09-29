//! Drives a compiled scheduler with a controlled wall clock.

use rustclamp_core::{Clock, ModuleId, Qualifier, QualifierId};
use rustclamp_kernel::TargetComposition;
use rustclamp_scheduler::{JobDeclaration, MisfirePolicy, SchedulerTarget};
use std::sync::Mutex;
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
}
