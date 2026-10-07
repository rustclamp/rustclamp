//! Tests the application blueprint and its process-root projections.

use rustclamp_core::{ApplicationId, Capability, ExecutionId};
use rustclamp_example_process::{
    APPLICATION, CLI_EXECUTION, CLI_ROOT, CLOCK_PROVIDER, CliProcess, DORMANT_MODULE,
    EPOCH_PROVIDER, GREETING_COMMAND, QUEUE_PROVIDER, StartupProbe, WORKER_EXECUTION, WORKER_ROOT,
    WorkerProcess, application, inspect, inspection_text, run, run_with_worker_setting, start_host,
    start_process,
};
use rustclamp_example_process::{StartupError, WORKER_CONCURRENCY_SETTING, WorkerSettingProblem};
use rustclamp_kernel::{InclusionReason, ProjectionError};

#[test]
fn cli_reaches_its_command_and_shared_clock_but_not_worker_infrastructure() {
    let projection = inspect(CliProcess::ID).unwrap();

    assert_eq!(projection.application(), APPLICATION);
    assert_eq!(projection.roots(), [CLI_EXECUTION]);
    for module in [CLI_ROOT, GREETING_COMMAND, CLOCK_PROVIDER, EPOCH_PROVIDER] {
        assert!(projection.includes(module), "missing {module:?}");
    }
    for module in [WORKER_ROOT, QUEUE_PROVIDER, DORMANT_MODULE] {
        assert!(!projection.includes(module), "unexpected {module:?}");
    }
    assert!(
        run(CliProcess::ID)
            .unwrap()
            .starts_with("example.process.cli projection")
    );
}

#[test]
fn worker_reaches_its_queue_and_shared_clock_but_not_cli_commands() {
    let projection = inspect(WorkerProcess::ID).unwrap();

    assert_eq!(projection.roots(), [WORKER_EXECUTION]);
    assert!(projection.includes(WORKER_ROOT));
    assert!(projection.includes(QUEUE_PROVIDER));
    assert!(projection.includes(CLOCK_PROVIDER));
    assert!(projection.includes(EPOCH_PROVIDER));
    assert!(!projection.includes(CLI_ROOT));
    assert!(!projection.includes(GREETING_COMMAND));
}

#[test]
fn included_clock_inspection_path_matches_its_contributor_requirement() {
    let projection = inspect(CliProcess::ID).unwrap();
    let clock = projection
        .included_modules()
        .iter()
        .find(|entry| entry.module() == CLOCK_PROVIDER)
        .unwrap();

    assert_eq!(clock.path(), [CLI_ROOT, GREETING_COMMAND, CLOCK_PROVIDER]);
    assert_eq!(
        clock.reason(),
        InclusionReason::CapabilityProvider {
            required_by: GREETING_COMMAND,
            capability: rustclamp_core::ClockCapability::ID,
            qualifier: None,
            selection: rustclamp_kernel::ProviderSelection::Explicit,
            replaced: None,
        }
    );
}

#[test]
fn inspection_text_exposes_resolved_model_and_inclusion_paths() {
    let text = inspection_text(CliProcess::ID).unwrap();
    assert!(text.contains("application: example.process.application"));
    assert!(text.contains(
        "example.process.cli-root -> example.process.greeting-command -> example.process.clock"
    ));
    assert!(text.contains("provider=Some(\"example.process.clock\")"));
    assert!(text.contains("example.process.dormant (Unreachable)"));
}

#[test]
fn dormant_invalid_requirement_does_not_break_cli_projection() {
    let projection = application().project(CliProcess::ID).unwrap();
    assert!(!projection.includes(DORMANT_MODULE));
}

#[test]
fn cli_does_not_read_or_validate_missing_or_invalid_worker_configuration() {
    for setting in [None, Some("not-a-number".to_owned())] {
        let mut read = false;
        let result = run_with_worker_setting(CliProcess::ID, || {
            read = true;
            setting
        });
        assert!(result.is_ok());
        assert!(!read, "CLI must not read Worker-only configuration");
    }
    let mut probe = StartupProbe::default();
    let mut setting_reads = 0;
    let instance = start_process(
        CliProcess::ID,
        &mut || {
            setting_reads += 1;
            Some("invalid".to_owned())
        },
        &mut probe,
    )
    .unwrap();
    assert_eq!(instance.process(), CliProcess::ID);
    assert_eq!(setting_reads, 0);
    assert_eq!(probe.worker_factory_calls(), 0);
    assert_eq!(probe.worker_target_builds(), 0);
    assert_eq!(probe.worker_startup_calls(), 0);
    assert_eq!(probe.cli_target_builds(), 1);

    let mut probe = StartupProbe::default();
    let mut host_setting_reads = 0;
    start_host(
        &mut || {
            host_setting_reads += 1;
            Some("3".to_owned())
        },
        &mut probe,
    )
    .unwrap();
    assert_eq!(host_setting_reads, 1, "only Worker reads its setting");
    assert_eq!(probe.worker_factory_calls(), 1);
    assert_eq!(probe.worker_target_builds(), 1);
    assert_eq!(probe.worker_startup_calls(), 1);
}

#[test]
fn worker_reports_missing_and_invalid_required_configuration() {
    assert_eq!(
        run(WorkerProcess::ID),
        Err(StartupError::WorkerSetting {
            process: WorkerProcess::ID,
            setting: WORKER_CONCURRENCY_SETTING,
            problem: WorkerSettingProblem::Missing,
        })
    );
    assert_eq!(
        run_with_worker_setting(WorkerProcess::ID, || Some("0".to_owned())),
        Err(StartupError::WorkerSetting {
            process: WorkerProcess::ID,
            setting: WORKER_CONCURRENCY_SETTING,
            problem: WorkerSettingProblem::Invalid,
        })
    );
    assert!(
        run_with_worker_setting(WorkerProcess::ID, || Some("4".to_owned()))
            .unwrap()
            .contains("worker concurrency: 4")
    );
}

#[test]
fn one_host_starts_both_projections_with_isolated_state_and_selected_callbacks() {
    let mut setting_reads = 0;
    let mut probe = StartupProbe::default();
    let mut instances = start_host(
        &mut || {
            setting_reads += 1;
            Some("2".to_owned())
        },
        &mut probe,
    )
    .unwrap();

    assert_eq!(setting_reads, 1);
    assert_eq!(probe.worker_factory_calls(), 1);
    assert_eq!(probe.worker_target_builds(), 1);
    assert_eq!(probe.worker_startup_calls(), 1);
    assert_eq!(probe.cli_target_builds(), 1);
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].process(), CliProcess::ID);
    assert_eq!(instances[1].process(), WorkerProcess::ID);

    instances[0].increment_state();
    instances[0].increment_state();
    assert_eq!(instances[0].state(), 2);
    assert_eq!(instances[1].state(), 0);
    instances[1].increment_state();
    assert_eq!(instances[0].state(), 2);
    assert_eq!(instances[1].state(), 1);
}

#[test]
fn absent_process_and_root_are_structured_errors() {
    let missing_process = rustclamp_core::ProcessId::new("example.process.absent");
    assert_eq!(
        application().project(missing_process),
        Err(ProjectionError::MissingProcess {
            application: APPLICATION,
            process: missing_process,
        })
    );

    let app_id = ApplicationId::new("test.missing-root");
    let process = rustclamp_core::ProcessId::new("test.process.missing-root");
    let root = ExecutionId::new("test.execution.missing-root");
    let mut app = rustclamp_kernel::ApplicationBlueprint::new(app_id);
    app.add_process(process, vec![root]);
    assert_eq!(
        app.project(process),
        Err(ProjectionError::MissingExecution {
            application: app_id,
            process,
            execution: root,
        })
    );
}
