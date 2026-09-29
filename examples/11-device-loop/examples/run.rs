//! Runs the deterministic simulation fixture.

use rustclamp_example_device_loop::{Input, Seeded, Simulation, State};

fn main() {
    let mut state = State {
        position_mm: 0,
        velocity_mm_per_step: 2,
        steps: 0,
        noise: 0,
    };
    let mut random = Seeded::new(42);
    for delta in [1, -1, 3] {
        Simulation::step(&mut state, Input { delta }, &mut random);
        println!(
            "step={} position={}mm velocity={}mm/step",
            state.steps, state.position_mm, state.velocity_mm_per_step
        );
    }
}
