use rustclamp_example_lifecycle::{
    DATABASE, ExternalDatabaseOwner, Health, LifecycleFailure, LifecycleState, Phase,
    ShutdownReason, TestControl, USERS, WORKER, WORKER_ROOT, application, start,
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
    assert_eq!(shutdown.elapsed_ms(), 12);
    assert_eq!(shutdown.shutdown_reason, Some(ShutdownReason::Requested));
    assert!(!shutdown.status.alive && !shutdown.status.started && !shutdown.status.ready);
    assert_eq!(shutdown.resources, Default::default());
}

#[test]
fn external_database_is_used_but_never_initialized_or_stopped_by_the_process() {
    let owner = ExternalDatabaseOwner::new();
    let result =
        rustclamp_example_lifecycle::start_with_external_database(TestControl::default(), &owner);

    assert!(owner.is_open());
    assert_eq!(result.elapsed_ms(), 3);
    assert_eq!(modules_in_phase(&result.events, Phase::Initialize), [USERS]);
    assert!(result.resources.database_open && result.resources.users_initialized);

    let stopped = rustclamp_example_lifecycle::shutdown(result, ShutdownReason::Requested);
    assert!(!stopped.resources.users_initialized);
    assert!(!stopped.events.iter().any(|event| {
        event.module == DATABASE && matches!(event.phase, Phase::Initialize | Phase::Stop)
    }));
    assert!(owner.is_open());
    assert_eq!(stopped.elapsed_ms(), 9);
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
    assert!(
        result
            .events
            .iter()
            .any(|event| event.phase == Phase::Stop && event.module == DATABASE)
    );
    assert_eq!(result.events.last().unwrap().phase, Phase::Flush);
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
