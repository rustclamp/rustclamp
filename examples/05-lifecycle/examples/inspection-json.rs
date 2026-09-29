//! Exports the example's Worker and Reporter process projections for `clamp`.

use rustclamp_example_lifecycle::{REPORTER, WORKER, application};
use rustclamp_tooling::inspection_document;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let blueprint = application();
    let worker = blueprint
        .project(WORKER)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let reporter = blueprint
        .project(REPORTER)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let document = inspection_document(&[&worker, &reporter]).map_err(std::io::Error::other)?;
    println!("{document}");
    Ok(())
}
