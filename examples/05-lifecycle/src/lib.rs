//! Synchronous fake-resource proof for process-scoped lifecycle coordination.

use std::collections::{BTreeMap, BTreeSet};

use rustclamp_core::{ApplicationId, CapabilityId, ExecutionId, ModuleId, ProcessId};
use rustclamp_kernel::{ApplicationBlueprint, FrozenProcess, ProjectionError};

pub const APPLICATION: ApplicationId = ApplicationId::new("example.lifecycle.application");
pub const WORKER: ProcessId = ProcessId::new("example.lifecycle.worker");
pub const EXECUTION: ExecutionId = ExecutionId::new("example.lifecycle.worker-root");
pub const WORKER_ROOT: ModuleId = ModuleId::new("example.lifecycle.worker");
pub const USERS: ModuleId = ModuleId::new("example.lifecycle.users");
pub const DATABASE: ModuleId = ModuleId::new("example.lifecycle.database");

const USERS_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.users-service");
const DATABASE_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.database");

pub fn application() -> ApplicationBlueprint {
    let mut app = ApplicationBlueprint::new(APPLICATION);
    app.add_module(WORKER_ROOT)
        .add_module(USERS)
        .add_module(DATABASE)
        .add_execution(EXECUTION, WORKER_ROOT)
        .add_process(WORKER, vec![EXECUTION])
        .require_provider(WORKER_ROOT, USERS_CAPABILITY, None, USERS)
        .require_provider(USERS, DATABASE_CAPABILITY, None, DATABASE);
    app
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    Initialize,
    Start,
    Ready,
    Drain,
    Cancel,
    Stop,
    Flush,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    Configured,
    Initializing,
    Starting,
    Ready,
    Draining,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Health {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownReason {
    Requested,
    StartupFailure,
    RequiredDependencyFailed(ModuleId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessStatus {
    pub application: ApplicationId,
    pub process: ProcessId,
    pub state: LifecycleState,
    pub alive: bool,
    pub started: bool,
    pub ready: bool,
    pub accepting_work: bool,
    pub health: Health,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FakeResources {
    pub database_open: bool,
    pub users_initialized: bool,
    pub users_started: bool,
    pub worker_started: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    pub initialize_ms: u64,
    pub start_ms: u64,
    pub drain_ms: u64,
    pub stop_ms: u64,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            initialize_ms: 100,
            start_ms: 100,
            drain_ms: 100,
            stop_ms: 100,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub phase: Phase,
    pub module: ModuleId,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub status: ProcessStatus,
    pub resources: FakeResources,
    pub shutdown_reason: Option<ShutdownReason>,
    pub events: Vec<Event>,
    pub failure: Option<LifecycleFailure>,
    pub cleanup_failures: Vec<(Phase, ModuleId)>,
    control: TestControl,
    elapsed_ms: u64,
    initialized: Vec<Step>,
    started: Vec<Step>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleFailure {
    Projection(ProjectionError),
    PhaseDeadline { phase: Phase, module: ModuleId },
    Injected { phase: Phase, module: ModuleId },
    RequiredHealth(ModuleId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Step {
    module: ModuleId,
    initialize: bool,
    start: bool,
    ready: bool,
    drain: bool,
    cancel: bool,
    stop: bool,
    initialize_ms: u64,
    start_ms: u64,
    drain_ms: u64,
    stop_ms: u64,
    fail: Option<Phase>,
    health: Health,
    required_health: bool,
}

const DATABASE_STEP: Step = Step {
    module: DATABASE,
    initialize: true,
    start: false,
    ready: true,
    drain: false,
    cancel: false,
    stop: true,
    initialize_ms: 2,
    start_ms: 0,
    drain_ms: 0,
    stop_ms: 1,
    fail: None,
    health: Health::Healthy,
    required_health: true,
};
const USERS_STEP: Step = Step {
    module: USERS,
    initialize: true,
    start: true,
    ready: true,
    drain: true,
    cancel: true,
    stop: true,
    initialize_ms: 1,
    start_ms: 1,
    drain_ms: 2,
    stop_ms: 1,
    fail: None,
    health: Health::Healthy,
    required_health: true,
};
const WORKER_STEP: Step = Step {
    module: WORKER_ROOT,
    initialize: false,
    start: true,
    ready: true,
    drain: true,
    cancel: true,
    stop: true,
    initialize_ms: 0,
    start_ms: 1,
    drain_ms: 2,
    stop_ms: 1,
    fail: None,
    health: Health::Healthy,
    required_health: true,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TestControl {
    pub deadlines: Deadlines,
    pub fail: Option<(Phase, ModuleId)>,
    pub required_health: Option<(ModuleId, Health)>,
    pub optional_health: Option<(ModuleId, Health)>,
    pub cleanup_failure: Option<(Phase, ModuleId)>,
}

pub fn start(control: TestControl) -> Outcome {
    let frozen = match application().freeze(WORKER) {
        Ok(frozen) => frozen,
        Err(error) => return failed_projection(error),
    };
    Driver::new(frozen, control).start()
}

impl Outcome {
    /// Returns elapsed virtual milliseconds recorded by the deterministic test clock.
    pub const fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
}

pub fn shutdown(outcome: Outcome, reason: ShutdownReason) -> Outcome {
    if outcome.status.state != LifecycleState::Ready {
        return outcome;
    }
    let mut driver = Driver {
        ordered: Vec::new(),
        control: outcome.control,
        elapsed_ms: outcome.elapsed_ms,
        events: outcome.events,
        resources: outcome.resources,
        initialized: outcome.initialized,
        started: outcome.started,
        status: outcome.status,
        failure: outcome.failure,
        cleanup_failures: outcome.cleanup_failures,
        shutdown_reason: None,
    };
    driver.stop(reason);
    driver.outcome()
}

struct Driver {
    ordered: Vec<Step>,
    control: TestControl,
    elapsed_ms: u64,
    events: Vec<Event>,
    initialized: Vec<Step>,
    started: Vec<Step>,
    status: ProcessStatus,
    resources: FakeResources,
    failure: Option<LifecycleFailure>,
    cleanup_failures: Vec<(Phase, ModuleId)>,
    shutdown_reason: Option<ShutdownReason>,
}

impl Driver {
    fn new(frozen: FrozenProcess, control: TestControl) -> Self {
        let ordered = lifecycle_order(&frozen)
            .into_iter()
            .filter_map(|module| step(module, control))
            .collect();
        Self {
            ordered,
            control,
            elapsed_ms: 0,
            events: Vec::new(),
            initialized: Vec::new(),
            started: Vec::new(),
            status: ProcessStatus {
                application: frozen.inspection().application(),
                process: frozen.inspection().process(),
                state: LifecycleState::Configured,
                alive: false,
                started: false,
                ready: false,
                accepting_work: false,
                health: Health::Healthy,
            },
            resources: FakeResources::default(),
            failure: None,
            cleanup_failures: Vec::new(),
            shutdown_reason: None,
        }
    }

    fn start(mut self) -> Outcome {
        self.status.alive = true;
        self.status.state = LifecycleState::Initializing;
        for step in self.ordered.clone() {
            if !step.initialize {
                continue;
            }
            if let Err(failure) = self.perform(
                step,
                Phase::Initialize,
                self.control.deadlines.initialize_ms,
            ) {
                self.failure = Some(failure);
                self.stop(ShutdownReason::StartupFailure);
                return self.outcome();
            }
            self.initialized.push(step);
        }

        self.status.state = LifecycleState::Starting;
        for step in self.ordered.clone() {
            if !step.start {
                continue;
            }
            if let Err(failure) = self.perform(step, Phase::Start, self.control.deadlines.start_ms)
            {
                self.failure = Some(failure);
                self.stop(ShutdownReason::StartupFailure);
                return self.outcome();
            }
            self.started.push(step);
        }
        self.status.started = true;

        for step in self.ordered.clone() {
            if !step.ready {
                continue;
            }
            if step.required_health && step.health == Health::Unhealthy {
                self.failure = Some(LifecycleFailure::RequiredHealth(step.module));
                self.stop(ShutdownReason::StartupFailure);
                return self.outcome();
            }
            self.status.health = combine_health(self.status.health, step.health);
            if let Err(failure) = self.perform(step, Phase::Ready, self.control.deadlines.start_ms)
            {
                self.failure = Some(failure);
                self.stop(ShutdownReason::StartupFailure);
                return self.outcome();
            }
        }
        if let Some((_, health)) = self.control.optional_health {
            self.status.health = combine_health(self.status.health, health);
        }
        self.status.state = LifecycleState::Ready;
        self.status.ready = true;
        self.status.accepting_work = true;
        self.outcome()
    }

    fn stop(&mut self, reason: ShutdownReason) {
        self.status.ready = false;
        self.status.accepting_work = false;
        self.status.state = LifecycleState::Draining;
        for step in self.started.clone().into_iter().rev() {
            if step.drain {
                self.perform_cleanup(step, Phase::Drain, self.control.deadlines.drain_ms);
            }
        }
        for step in self.started.clone().into_iter().rev() {
            if step.cancel {
                self.record(Phase::Cancel, step.module);
            }
        }
        self.status.state = LifecycleState::Stopping;
        let mut stopped = BTreeSet::new();
        for step in self.started.clone().into_iter().rev() {
            if step.stop {
                self.perform_cleanup(step, Phase::Stop, self.control.deadlines.stop_ms);
                stopped.insert(step.module);
            }
        }
        for step in self.initialized.clone().into_iter().rev() {
            if step.stop && stopped.insert(step.module) {
                self.perform_cleanup(step, Phase::Stop, self.control.deadlines.stop_ms);
            }
        }
        self.record(Phase::Flush, WORKER_ROOT);
        self.status.state = LifecycleState::Stopped;
        self.status.alive = false;
        self.status.started = false;
        self.status.ready = false;
        self.shutdown_reason = Some(reason);
    }

    fn perform(
        &mut self,
        step: Step,
        phase: Phase,
        deadline_ms: u64,
    ) -> Result<(), LifecycleFailure> {
        let duration = match phase {
            Phase::Initialize => step.initialize_ms,
            Phase::Start => step.start_ms,
            _ => 0,
        };
        if duration > deadline_ms {
            return Err(LifecycleFailure::PhaseDeadline {
                phase,
                module: step.module,
            });
        }
        self.elapsed_ms += duration;
        self.record(phase, step.module);
        if self.control.fail == Some((phase, step.module)) || step.fail == Some(phase) {
            return Err(LifecycleFailure::Injected {
                phase,
                module: step.module,
            });
        }
        self.apply_resource_state(phase, step.module);
        Ok(())
    }

    fn perform_cleanup(&mut self, step: Step, phase: Phase, deadline_ms: u64) {
        let duration = match phase {
            Phase::Drain => step.drain_ms,
            Phase::Stop => step.stop_ms,
            _ => 0,
        };
        if duration > deadline_ms {
            self.cleanup_failures.push((phase, step.module));
            return;
        }
        self.elapsed_ms += duration;
        self.record(phase, step.module);
        if self.control.cleanup_failure == Some((phase, step.module)) {
            self.cleanup_failures.push((phase, step.module));
        } else {
            self.apply_resource_state(phase, step.module);
        }
    }

    fn apply_resource_state(&mut self, phase: Phase, module: ModuleId) {
        match (phase, module) {
            (Phase::Initialize, DATABASE) => self.resources.database_open = true,
            (Phase::Initialize, USERS) => self.resources.users_initialized = true,
            (Phase::Start, USERS) => self.resources.users_started = true,
            (Phase::Start, WORKER_ROOT) => self.resources.worker_started = true,
            (Phase::Cancel, WORKER_ROOT) | (Phase::Stop, WORKER_ROOT) => {
                self.resources.worker_started = false;
            }
            (Phase::Cancel, USERS) => self.resources.users_started = false,
            (Phase::Stop, USERS) => {
                self.resources.users_started = false;
                self.resources.users_initialized = false;
            }
            (Phase::Stop, DATABASE) => self.resources.database_open = false,
            _ => {}
        }
    }

    fn record(&mut self, phase: Phase, module: ModuleId) {
        self.events.push(Event {
            phase,
            module,
            elapsed_ms: self.elapsed_ms,
        });
    }

    fn outcome(self) -> Outcome {
        Outcome {
            status: self.status,
            resources: self.resources,
            shutdown_reason: self.shutdown_reason,
            events: self.events,
            failure: self.failure,
            cleanup_failures: self.cleanup_failures,
            control: self.control,
            elapsed_ms: self.elapsed_ms,
            initialized: self.initialized,
            started: self.started,
        }
    }
}

fn lifecycle_order(frozen: &FrozenProcess) -> Vec<ModuleId> {
    let modules = frozen.runtime().modules();
    let mut dependencies = BTreeMap::<ModuleId, BTreeSet<ModuleId>>::new();
    for module in modules {
        dependencies.entry(*module).or_default();
    }
    for requirement in frozen.inspection().requirements() {
        if let Some(provider) = requirement.provider() {
            dependencies
                .entry(requirement.consumer())
                .or_default()
                .insert(provider);
        }
    }
    let mut ordered = Vec::with_capacity(modules.len());
    let mut pending = modules.to_vec();
    while !pending.is_empty() {
        pending.sort();
        let Some(index) = pending.iter().position(|module| {
            dependencies[module]
                .iter()
                .all(|dependency| ordered.contains(dependency))
        }) else {
            return modules.to_vec();
        };
        ordered.push(pending.remove(index));
    }
    ordered
}

fn step(module: ModuleId, control: TestControl) -> Option<Step> {
    let mut step = match module {
        DATABASE => DATABASE_STEP,
        USERS => USERS_STEP,
        WORKER_ROOT => WORKER_STEP,
        _ => return None,
    };
    if let Some((optional_module, health)) = control.optional_health
        && optional_module == module
    {
        step.health = health;
        step.required_health = false;
    }
    if let Some((required_module, health)) = control.required_health
        && required_module == module
    {
        step.health = health;
        step.required_health = true;
    }
    Some(step)
}

fn combine_health(current: Health, next: Health) -> Health {
    match (current, next) {
        (_, Health::Unhealthy) | (Health::Unhealthy, _) => Health::Unhealthy,
        (_, Health::Degraded) | (Health::Degraded, _) => Health::Degraded,
        _ => Health::Healthy,
    }
}

fn failed_projection(error: ProjectionError) -> Outcome {
    Outcome {
        status: ProcessStatus {
            application: APPLICATION,
            process: WORKER,
            state: LifecycleState::Failed,
            alive: false,
            started: false,
            ready: false,
            accepting_work: false,
            health: Health::Unhealthy,
        },
        resources: FakeResources::default(),
        shutdown_reason: Some(ShutdownReason::StartupFailure),
        events: Vec::new(),
        failure: Some(LifecycleFailure::Projection(error)),
        cleanup_failures: Vec::new(),
        control: TestControl::default(),
        elapsed_ms: 0,
        initialized: Vec::new(),
        started: Vec::new(),
    }
}
