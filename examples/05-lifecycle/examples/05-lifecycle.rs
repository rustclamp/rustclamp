use rustclamp_example_lifecycle::{ShutdownReason, TestControl, shutdown, start};

fn main() {
    let outcome = shutdown(start(TestControl::default()), ShutdownReason::Requested);
    let inspection = outcome.inspection();
    println!(
        "application: {}; process: {}",
        inspection.application().as_str(),
        inspection.process().as_str()
    );
    for dependency in inspection.dependencies() {
        println!(
            "{} requires {}",
            dependency.consumer().as_str(),
            dependency.provider().as_str()
        );
    }
    for module in inspection.modules() {
        println!(
            "{}: owner={:?}, phases={:?}, cleanup={:?}",
            module.module().as_str(),
            module.owner(),
            module.phases(),
            module.cleanup()
        );
    }
    for event in &outcome.events {
        println!(
            "{:?}: {} ({} ms)",
            event.phase,
            event.module.as_str(),
            event.elapsed_ms
        );
    }
    println!(
        "state: {:?}; ready: {}; elapsed: {} ms",
        outcome.status.state,
        outcome.status.ready,
        outcome.elapsed_ms()
    );
}
