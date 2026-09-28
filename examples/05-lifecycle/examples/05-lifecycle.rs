use rustclamp_example_lifecycle::{ShutdownReason, TestControl, shutdown, start};

fn main() {
    let outcome = shutdown(start(TestControl::default()), ShutdownReason::Requested);
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
