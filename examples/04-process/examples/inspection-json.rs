//! Exports the example's actual CLI and Worker projections for `clamp`.

use rustclamp_example_process::{CliProcess, WorkerProcess, inspect};
use rustclamp_tooling::inspection_document;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let cli =
        inspect(CliProcess::ID).map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let worker =
        inspect(WorkerProcess::ID).map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let document = inspection_document(&[&cli, &worker]).map_err(std::io::Error::other)?;
    println!("{document}");
    Ok(())
}
