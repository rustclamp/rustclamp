//! Minimal Core consumer that needs no Kernel or execution runtime.

use rustclamp_example_platform_neutral::{TemperatureHistory, module_id};

fn main() {
    let mut history = TemperatureHistory::default();
    history.record(21_000);
    println!("{}: {} mC", module_id().as_str(), history.readings()[0]);
}
