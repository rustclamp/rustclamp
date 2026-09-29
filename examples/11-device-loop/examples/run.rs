//! Runs the deterministic simulation fixture.

use rustclamp_core::{ApplicationId, ExecutionId, ModuleId, ProcessId};
use rustclamp_example_device_loop::{Input, LoopRuntime, State};
use rustclamp_kernel::ApplicationBlueprint;
use rustclamp_runtime::TaskContext;

fn main() {
    const APP: ApplicationId = ApplicationId::new("example.device-loop");
    const PROCESS: ProcessId = ProcessId::new("example.device-loop.simulation");
    const EXECUTION: ExecutionId = ExecutionId::new("example.device-loop.root");
    const ROOT: ModuleId = ModuleId::new("example.device-loop.root");
    let mut blueprint = ApplicationBlueprint::new(APP);
    blueprint.add_module(ROOT);
    blueprint
        .add_execution(EXECUTION, ROOT)
        .add_process(PROCESS, vec![EXECUTION]);
    let process = blueprint.freeze(PROCESS).expect("valid simulation process");
    assert_eq!(process.runtime().modules(), &[ROOT]);

    let mut state = State {
        position_mm: 0,
        velocity_mm_per_step: 2,
        steps: 0,
        noise: 0,
    };
    let inputs = [Input { delta: 1 }, Input { delta: -1 }, Input { delta: 3 }];
    let mut input = inputs.into_iter();
    let mut loop_runtime = LoopRuntime::new(42);
    let context = TaskContext::new();
    for next in &mut input {
        loop_runtime.run(&mut state, &mut std::iter::once(next), &context);
        println!(
            "step={} position={}mm velocity={}mm/step",
            state.steps, state.position_mm, state.velocity_mm_per_step
        );
    }
}
