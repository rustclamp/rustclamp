//! The `worker` profile: handles jobs one at a time and polls every
//! `WORKER_POLL_MS` (1000) when none is waiting. With `--once` it stops as
//! soon as none is waiting, for cron and tests.

use std::thread;
use std::time::Duration;

use rustclamp::config::Config;
use rustclamp::log::Log;

/// Works until the process is stopped, or with `--once` until no job waits.
pub fn run(args: &[String]) -> u8 {
    let poll = Duration::from_millis(Config::load().get_or("WORKER_POLL_MS", 1000));
    let once = args.iter().any(|arg| arg == "--once");
    Log::info("worker started");
    loop {
        match next_job() {
            Some(job) => {
                if let Err(error) = handle(&job) {
                    Log::error(format_args!("job {job} failed: {error}"));
                }
            }
            None if once => {
                println!("worker idle");
                return 0;
            }
            None => thread::sleep(poll),
        }
    }
}

// ponytail: no queue yet; claim from yours here (a database table, Redis, a
// broker). Retries, dead letters and drain on shutdown are rustclamp-worker's
// `WorkerService`: see the worker recipe on docs.rustclamp.com.
fn next_job() -> Option<String> {
    None
}

fn handle(job: &str) -> Result<(), String> {
    Log::info(format_args!("handled {job}"));
    Ok(())
}
