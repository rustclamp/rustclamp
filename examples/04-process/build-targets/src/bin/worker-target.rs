use rustclamp_core::{ApplicationId, ExecutionId, ModuleId, ProcessId};
use rustclamp_kernel::ApplicationBlueprint;

const APP: ApplicationId = ApplicationId::new("build-comparison.worker-application");
const WORKER: ProcessId = ProcessId::new("build-comparison.worker");
const EXECUTION: ExecutionId = ExecutionId::new("build-comparison.worker-exec");
const ROOT: ModuleId = ModuleId::new("build-comparison.worker-root");

fn main() {
    let mut app = ApplicationBlueprint::new(APP);
    app.add_module(ROOT)
        .add_execution(EXECUTION, ROOT)
        .add_process(WORKER, vec![EXECUTION]);
    let projection = app.project(WORKER).expect("known process");
    println!(
        "{} includes {} module(s)",
        WORKER.as_str(),
        projection.included_modules().len()
    );
}
