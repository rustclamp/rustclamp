use rustclamp_example_lifecycle::{
    ApplicationResources, CleanupState, DATABASE, EXECUTION, ExecutionResources,
    ExternalDatabaseOwner, Health, LifecycleFailure, LifecycleState, MAINTENANCE_EXECUTION, Phase,
    ProcessResources, REPORTER, ResourceOwner, ShutdownReason, TestControl, USERS, WORKER,
    WORKER_ROOT, application, start,
};

fn modules_in_phase(
    events: &[rustclamp_example_lifecycle::Event],
    phase: Phase,
) -> Vec<rustclamp_core::ModuleId> {
    events
        .iter()
        .filter(|event| event.phase == phase)
        .map(|event| event.module)
        .collect()
}

#[test]
fn application_resource_is_shared_while_process_resources_are_isolated() {
    let [worker, reporter] = application()
        .freeze_processes(&[WORKER, REPORTER])
        .unwrap()
        .try_into()
        .unwrap();
    let application_resources = ApplicationResources::new();
    let worker_resources = ProcessResources::new(&worker, &application_resources);
    let reporter_resources = ProcessResources::new(&reporter, &application_resources);

    assert_ne!(worker_resources.process(), reporter_resources.process());
    assert!(worker_resources.start());
    assert!(reporter_resources.start());
    worker_resources.stop();

    assert!(!worker_resources.is_active());
    assert!(reporter_resources.is_active());
    assert!(application_resources.database_open());

    reporter_resources.stop();
    assert!(!worker_resources.is_active() && !reporter_resources.is_active());
    assert!(application_resources.database_open());
    application_resources.close();
    assert!(!application_resources.database_open());
    assert!(!worker_resources.start() && !reporter_resources.start());
}

#[test]
fn execution_resource_exists_only_for_its_frozen_root_and_stops_independently() {
    let worker = application().freeze(WORKER).unwrap();
    let application_resources = ApplicationResources::new();
    let request = ExecutionResources::new(&worker, EXECUTION, &application_resources).unwrap();
    let maintenance =
        ExecutionResources::new(&worker, MAINTENANCE_EXECUTION, &application_resources).unwrap();

    assert!(
        ExecutionResources::new(
            &worker,
            rustclamp_core::ExecutionId::new("example.lifecycle.unprojected"),
            &application_resources,
        )
        .is_none()
    );
    assert_ne!(request.execution(), maintenance.execution());
    assert!(request.start() && maintenance.start());
    request.stop();

    assert!(!request.is_active());
    assert!(maintenance.is_active());
    assert!(application_resources.database_open());
}

#[test]
fn runtime_selection_is_process_specific_and_manual_path_needs_no_async_runtime() {
    use rustclamp_example_lifecycle::{ProcessRuntimeKind, runtime_for_process, service_task};
    use rustclamp_runtime::{ManualRuntime, Supervision, Supervisor, TaskContext};

    assert_eq!(runtime_for_process(WORKER), Some(ProcessRuntimeKind::Tokio));
    assert_eq!(
        runtime_for_process(REPORTER),
        Some(ProcessRuntimeKind::Manual)
    );
    assert_eq!(
        runtime_for_process(rustclamp_core::ProcessId::new("example.lifecycle.unknown")),
        None
    );
    let outcome =
        Supervisor::new(ManualRuntime).supervise(&service_task(REPORTER), &TaskContext::new());
    assert_eq!(outcome, Supervision::Completed);
}

#[cfg(feature = "tokio-runtime")]
#[test]
fn worker_process_runs_through_the_optional_tokio_adapter() {
    use rustclamp_example_lifecycle::{service_task, shutdown_reason_for_signal};
    use rustclamp_runtime::{
        Supervision, Supervisor, TaskContext,
        tokio_runtime::{ShutdownSignal, TokioRuntime},
    };

    let runtime = TokioRuntime::managed().unwrap();
    assert_eq!(
        Supervisor::new(runtime).supervise(&service_task(WORKER), &TaskContext::new()),
        Supervision::Completed
    );
    assert_eq!(
        shutdown_reason_for_signal(ShutdownSignal::Interrupt),
        ShutdownReason::Requested
    );
}

#[test]
fn freeze_and_validation_finish_before_lifecycle_side_effects() {
    let projection = application().project(WORKER).unwrap();
    assert!(projection.includes(DATABASE));
    assert!(projection.includes(USERS));
    assert_eq!(
        start(TestControl::default()).events[0].phase,
        Phase::Initialize
    );
}

#[test]
fn dependencies_initialize_before_users_and_stop_after_consumers() {
    let result = start(TestControl::default());
    let inspection = result.inspection();
    assert_eq!(inspection.dependencies().len(), 2);
    let database = inspection
        .modules()
        .iter()
        .find(|module| module.module() == DATABASE)
        .unwrap();
    assert_eq!(database.owner(), ResourceOwner::Process);
    assert_eq!(database.cleanup(), CleanupState::Active);
    assert!(database.phases().contains(&Phase::Initialize));
    assert!(!database.phases().contains(&Phase::Start));
    assert_eq!(
        modules_in_phase(&result.events, Phase::Initialize),
        [DATABASE, USERS]
    );
    assert_eq!(
        modules_in_phase(&result.events, Phase::Start),
        [USERS, WORKER_ROOT]
    );
    assert_eq!(result.elapsed_ms(), 5);
    assert!(result.status.started && result.status.ready && result.status.accepting_work);
    assert!(result.resources.database_open && result.resources.users_started);

    let shutdown = rustclamp_example_lifecycle::shutdown(result, ShutdownReason::Requested);
    assert_eq!(
        modules_in_phase(&shutdown.events, Phase::Drain),
        [WORKER_ROOT, USERS]
    );
    assert_eq!(
        modules_in_phase(&shutdown.events, Phase::Stop),
        [WORKER_ROOT, USERS, DATABASE]
    );
    assert_eq!(shutdown.elapsed_ms(), 14);
    assert_eq!(shutdown.shutdown_reason, Some(ShutdownReason::Requested));
    assert!(!shutdown.status.alive && !shutdown.status.started && !shutdown.status.ready);
    assert_eq!(shutdown.resources, Default::default());
    assert!(
        shutdown
            .inspection()
            .modules()
            .iter()
            .all(|module| module.cleanup() == CleanupState::Stopped)
    );
}

#[test]
fn external_database_is_used_but_never_initialized_or_stopped_by_the_process() {
    let owner = ExternalDatabaseOwner::new();
    let result =
        rustclamp_example_lifecycle::start_with_external_database(TestControl::default(), &owner);

    assert!(owner.is_open());
    let database = result
        .inspection()
        .modules()
        .iter()
        .find(|module| module.module() == DATABASE)
        .unwrap();
    assert_eq!(database.owner(), ResourceOwner::External);
    assert_eq!(database.cleanup(), CleanupState::External);
    assert_eq!(result.elapsed_ms(), 3);
    assert_eq!(modules_in_phase(&result.events, Phase::Initialize), [USERS]);
    assert!(result.resources.database_open && result.resources.users_initialized);

    let stopped = rustclamp_example_lifecycle::shutdown(result, ShutdownReason::Requested);
    assert!(!stopped.resources.users_initialized);
    assert!(!stopped.events.iter().any(|event| {
        event.module == DATABASE && matches!(event.phase, Phase::Initialize | Phase::Stop)
    }));
    assert!(owner.is_open());
    assert_eq!(stopped.elapsed_ms(), 11);
}

#[test]
fn required_start_failure_never_becomes_ready_and_cleans_only_acquired_modules() {
    let result = start(TestControl {
        fail: Some((Phase::Start, WORKER_ROOT)),
        ..TestControl::default()
    });
    assert_eq!(result.status.state, LifecycleState::Stopped);
    assert!(!result.status.ready);
    assert_eq!(result.shutdown_reason, Some(ShutdownReason::StartupFailure));
    assert_eq!(
        modules_in_phase(&result.events, Phase::Stop),
        [USERS, DATABASE]
    );
}

#[test]
fn initialization_failure_cleans_only_successfully_acquired_dependencies() {
    let result = start(TestControl {
        fail: Some((Phase::Initialize, USERS)),
        ..TestControl::default()
    });
    assert_eq!(
        result.failure,
        Some(LifecycleFailure::Injected {
            phase: Phase::Initialize,
            module: USERS
        })
    );
    assert_eq!(modules_in_phase(&result.events, Phase::Stop), [DATABASE]);
    assert!(!result.status.ready);
}

#[test]
fn cleanup_errors_do_not_skip_remaining_cleanup_or_flush() {
    let result = rustclamp_example_lifecycle::shutdown(
        start(TestControl {
            cleanup_failure: Some((Phase::Drain, USERS)),
            ..TestControl::default()
        }),
        ShutdownReason::Requested,
    );
    assert_eq!(result.cleanup_failures, [(Phase::Drain, USERS)]);
    assert_eq!(
        result
            .inspection()
            .modules()
            .iter()
            .find(|module| module.module() == USERS)
            .unwrap()
            .cleanup(),
        CleanupState::Failed
    );
    assert!(
        result
            .events
            .iter()
            .any(|event| event.phase == Phase::Stop && event.module == DATABASE)
    );
    assert_eq!(result.events.last().unwrap().phase, Phase::Flush);
}

#[test]
fn diagnostic_flush_is_bounded_and_keeps_a_local_fallback() {
    let fallback = rustclamp_example_lifecycle::shutdown(
        start(TestControl {
            telemetry_unavailable: true,
            ..TestControl::default()
        }),
        ShutdownReason::Requested,
    );
    assert!(fallback.diagnostic_fallback);
    assert_eq!(fallback.events.last().unwrap().phase, Phase::Flush);

    let bounded = rustclamp_example_lifecycle::shutdown(
        start(TestControl {
            flush_ms: 5,
            deadlines: rustclamp_example_lifecycle::Deadlines {
                flush_ms: 2,
                ..Default::default()
            },
            ..TestControl::default()
        }),
        ShutdownReason::Requested,
    );
    assert!(bounded.diagnostic_fallback);
    assert_eq!(bounded.elapsed_ms(), 15);
    assert_eq!(bounded.events.last().unwrap().phase, Phase::Flush);
}

#[test]
fn deadlines_bound_startup_drain_and_cleanup() {
    let initialize_timeout = start(TestControl {
        deadlines: rustclamp_example_lifecycle::Deadlines {
            initialize_ms: 1,
            ..Default::default()
        },
        ..TestControl::default()
    });
    assert_eq!(
        initialize_timeout.failure,
        Some(LifecycleFailure::PhaseDeadline {
            phase: Phase::Initialize,
            module: DATABASE
        })
    );

    let cumulative_initialize_timeout = start(TestControl {
        deadlines: rustclamp_example_lifecycle::Deadlines {
            initialize_ms: 2,
            ..Default::default()
        },
        ..TestControl::default()
    });
    assert_eq!(
        cumulative_initialize_timeout.failure,
        Some(LifecycleFailure::PhaseDeadline {
            phase: Phase::Initialize,
            module: USERS
        })
    );

    let start_timeout = start(TestControl {
        deadlines: rustclamp_example_lifecycle::Deadlines {
            start_ms: 0,
            ..Default::default()
        },
        ..TestControl::default()
    });
    assert_eq!(
        start_timeout.failure,
        Some(LifecycleFailure::PhaseDeadline {
            phase: Phase::Start,
            module: USERS
        })
    );
    assert!(!start_timeout.status.ready);

    let drain_timeout = rustclamp_example_lifecycle::shutdown(
        start(TestControl {
            deadlines: rustclamp_example_lifecycle::Deadlines {
                drain_ms: 0,
                ..Default::default()
            },
            ..TestControl::default()
        }),
        ShutdownReason::Requested,
    );
    assert_eq!(
        drain_timeout.cleanup_failures,
        [(Phase::Drain, WORKER_ROOT), (Phase::Drain, USERS)]
    );
    assert!(!drain_timeout.status.alive);

    let task_stop_timeout = rustclamp_example_lifecycle::shutdown(
        start(TestControl {
            deadlines: rustclamp_example_lifecycle::Deadlines {
                task_stop_ms: 0,
                ..Default::default()
            },
            ..TestControl::default()
        }),
        ShutdownReason::Requested,
    );
    assert!(
        task_stop_timeout
            .cleanup_failures
            .contains(&(Phase::TaskStop, WORKER_ROOT))
    );
    assert!(
        task_stop_timeout
            .events
            .iter()
            .any(|event| event.phase == Phase::Stop && event.module == WORKER_ROOT)
    );
}

#[test]
fn required_health_failure_keeps_process_unready() {
    let result = start(TestControl {
        required_health: Some((USERS, Health::Unhealthy)),
        ..TestControl::default()
    });
    assert_eq!(
        result.failure,
        Some(LifecycleFailure::RequiredHealth(USERS))
    );
    assert!(!result.status.ready && !result.status.accepting_work);
    assert_eq!(result.status.state, LifecycleState::Stopped);
}

#[test]
fn ready_health_and_liveness_are_distinct_and_optional_degradation_is_allowed() {
    let degraded = start(TestControl {
        optional_health: Some((USERS, Health::Degraded)),
        ..TestControl::default()
    });
    assert_eq!(degraded.status.state, LifecycleState::Ready);
    assert!(degraded.status.alive && degraded.status.ready);
    assert_eq!(degraded.status.health, Health::Degraded);

    let unhealthy = start(TestControl {
        optional_health: Some((USERS, Health::Unhealthy)),
        ..TestControl::default()
    });
    assert_eq!(unhealthy.status.state, LifecycleState::Ready);
    assert_eq!(unhealthy.status.health, Health::Unhealthy);
}
