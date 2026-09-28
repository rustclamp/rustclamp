use rustclamp_example_process::{
    CliProcess, WORKER_CONCURRENCY_SETTING, WorkerProcess, inspection_text, run_with_worker_setting,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let inspect = first.as_deref() == Some("inspect");
    let selected = if inspect { args.next() } else { first };
    let process = match selected.as_deref() {
        None | Some("cli") => CliProcess::ID,
        Some("worker") => WorkerProcess::ID,
        Some(other) => {
            eprintln!("unknown process {other:?}; choose cli or worker");
            std::process::exit(2);
        }
    };

    if inspect {
        match inspection_text(process) {
            Ok(inspection) => println!("{inspection}"),
            Err(error) => {
                eprintln!("inspection failed: {error:?}");
                std::process::exit(1);
            }
        }
    } else {
        match run_with_worker_setting(process, || std::env::var(WORKER_CONCURRENCY_SETTING).ok()) {
            Ok(description) => println!("{description}"),
            Err(error) => {
                eprintln!("startup failed: {error:?}");
                std::process::exit(1);
            }
        }
    }
}
