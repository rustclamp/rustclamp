//! The `cli` profile: one command per run. Results go to stdout, usage errors
//! to stderr, and the outcome is the exit code: 0 done, 2 bad usage.

/// Runs the command in `args` and returns the process exit code.
pub fn run(args: &[String]) -> u8 {
    match args.first().map(String::as_str) {
        None | Some("hello") => {
            println!("Hello from Clamp!");
            0
        }
        Some(command) => {
            eprintln!("unknown command {command:?}");
            2
        }
    }
}
