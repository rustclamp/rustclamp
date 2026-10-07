//! Compares loop-driver overhead with an equivalent ordinary Rust loop.

use std::hint::black_box;
use std::time::Instant;

use rustclamp_example_device_loop::{Input, LoopRuntime, Seeded, Simulation, State};
use rustclamp_runtime::TaskContext;

const STEPS: usize = 100_000;
const SAMPLES: usize = 9;

fn direct() -> State {
    let mut state = State {
        position_mm: 0,
        velocity_mm_per_step: 2,
        steps: 0,
        noise: 0,
    };
    let mut random = Seeded::new(42);
    for delta in std::iter::repeat([1, -1, 3]).flatten().take(STEPS) {
        Simulation::step(&mut state, Input { delta }, &mut random);
    }
    state
}

fn driven() -> State {
    let mut state = State {
        position_mm: 0,
        velocity_mm_per_step: 2,
        steps: 0,
        noise: 0,
    };
    let mut runtime = LoopRuntime::new(42);
    let mut inputs = std::iter::repeat([1, -1, 3])
        .flatten()
        .take(STEPS)
        .map(|delta| Input { delta });
    runtime.run(&mut state, &mut inputs, &TaskContext::new());
    state
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[SAMPLES / 2]
}

fn main() {
    let measure = |run: fn() -> State| {
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let start = Instant::now();
            black_box(run());
            samples.push(start.elapsed().as_nanos() as f64 / STEPS as f64);
        }
        median(samples)
    };
    let direct_ns = measure(direct);
    let driven_ns = measure(driven);
    println!("direct_loop_ns_per_step={direct_ns}");
    println!("runtime_loop_ns_per_step={driven_ns}");
    println!("overhead_ns_per_step={}", (driven_ns - direct_ns).max(0.0));
}
