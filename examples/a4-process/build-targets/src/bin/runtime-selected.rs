use rustclamp_core::{ApplicationId, ExecutionId, ModuleId, ProcessId};
use rustclamp_kernel::ApplicationBlueprint;

const APP: ApplicationId = ApplicationId::new("build-comparison.application");
const CLI: ProcessId = ProcessId::new("build-comparison.cli");
const WORKER: ProcessId = ProcessId::new("build-comparison.worker");
const CLI_EXEC: ExecutionId = ExecutionId::new("build-comparison.cli-exec");
const WORKER_EXEC: ExecutionId = ExecutionId::new("build-comparison.worker-exec");
const CLI_ROOT: ModuleId = ModuleId::new("build-comparison.cli-root");
const WORKER_ROOT: ModuleId = ModuleId::new("build-comparison.worker-root");

fn main() {
    let process = match std::env::args().nth(1).as_deref() {
        Some("worker") => WORKER,
        _ => CLI,
    };
    let mut app = ApplicationBlueprint::new(APP);
    app.add_module(CLI_ROOT)
        .add_module(WORKER_ROOT)
        .add_execution(CLI_EXEC, CLI_ROOT)
        .add_execution(WORKER_EXEC, WORKER_ROOT)
        .add_process(CLI, vec![CLI_EXEC])
        .add_process(WORKER, vec![WORKER_EXEC]);
    let projection = app.project(process).expect("known process");
    println!(
        "{} includes {} module(s)",
        process.as_str(),
        projection.included_modules().len()
    );
}
