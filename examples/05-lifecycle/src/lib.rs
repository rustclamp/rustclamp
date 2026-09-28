//! Synchronous fake-resource proof for process-scoped lifecycle coordination.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use rustclamp_core::{
    ApplicationId, Capability, CapabilityId, Drain, ExecutionId, Initialize, LifecycleContext,
    Module, ModuleId, ProcessId, Provides, Ready, Requires, Start, Stop,
};
use rustclamp_kernel::{ApplicationBlueprint, FrozenProcess, ProjectionError};

pub const APPLICATION: ApplicationId = ApplicationId::new("example.lifecycle.application");
pub const WORKER: ProcessId = ProcessId::new("example.lifecycle.worker");
pub const EXECUTION: ExecutionId = ExecutionId::new("example.lifecycle.worker-root");
pub const WORKER_ROOT: ModuleId = ModuleId::new("example.lifecycle.worker");
pub const USERS: ModuleId = ModuleId::new("example.lifecycle.users");
pub const DATABASE: ModuleId = ModuleId::new("example.lifecycle.database");

const USERS_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.users-service");
const DATABASE_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.database");

type SharedResources = Rc<RefCell<FakeResources>>;

pub struct DatabaseCapability;

impl Capability for DatabaseCapability {
    type Value = FakeDatabase;

    const ID: CapabilityId = DATABASE_CAPABILITY;
}

pub struct UsersCapability;

impl Capability for UsersCapability {
    type Value = FakeUsers;

    const ID: CapabilityId = USERS_CAPABILITY;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeDatabase {
    resources: SharedResources,
    managed: bool,
}

/// An already-open database whose lifetime remains with the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalDatabaseOwner {
    resources: SharedResources,
}

impl ExternalDatabaseOwner {
    /// Creates an external database before the lifecycle coordinator starts.
    pub fn new() -> Self {
        Self {
            resources: Rc::new(RefCell::new(FakeResources {
                database_open: true,
                ..FakeResources::default()
            })),
        }
    }

    /// Reports whether the caller-owned database is still open.
    pub fn is_open(&self) -> bool {
        self.resources.borrow().database_open
    }
}

impl Default for ExternalDatabaseOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FakeDatabase {
    const ID: ModuleId = DATABASE;
}

impl Provides<DatabaseCapability> for FakeDatabase {
    fn provided_value(&self) -> &FakeDatabase {
        self
    }
}

impl Initialize for FakeDatabase {
    type Error = &'static str;

    fn initialize(&mut self, context: &LifecycleContext) -> Result<(), Self::Error> {
        if !valid_context(context) {
            return Err("unexpected application or process");
        }
        self.resources.borrow_mut().database_open = true;
        Ok(())
    }
}

impl Ready for FakeDatabase {
    type Error = &'static str;

    fn ready(&mut self, context: &LifecycleContext) -> Result<(), Self::Error> {
        if valid_context(context) && self.resources.borrow().database_open {
            Ok(())
        } else {
            Err("database is not initialized for this process")
        }
    }
}

impl Stop for FakeDatabase {
    type Error = &'static str;

    fn stop(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        let mut resources = self.resources.borrow_mut();
        if resources.users_initialized {
            return Err("Users must stop before Database");
        }
        resources.database_open = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeUsers {
    resources: SharedResources,
}

impl Module for FakeUsers {
    const ID: ModuleId = USERS;
}

impl Requires<DatabaseCapability> for FakeUsers {}

impl Provides<UsersCapability> for FakeUsers {
    fn provided_value(&self) -> &FakeUsers {
        self
    }
}

impl Initialize for FakeUsers {
    type Error = &'static str;

    fn initialize(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        let mut resources = self.resources.borrow_mut();
        if !resources.database_open {
            return Err("Database must initialize before Users");
        }
        resources.users_initialized = true;
        Ok(())
    }
}

impl Start for FakeUsers {
    type Error = &'static str;

    fn start(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        let mut resources = self.resources.borrow_mut();
        if !resources.users_initialized || !resources.database_open {
            return Err("Users cannot start before Database is available");
        }
        resources.users_started = true;
        Ok(())
    }
}

impl Ready for FakeUsers {
    type Error = &'static str;

    fn ready(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        if self.resources.borrow().users_started {
            Ok(())
        } else {
            Err("Users has not started")
        }
    }
}

impl Drain for FakeUsers {
    type Error = &'static str;

    fn drain(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Stop for FakeUsers {
    type Error = &'static str;

    fn stop(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        let mut resources = self.resources.borrow_mut();
        resources.users_started = false;
        resources.users_initialized = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Worker {
    resources: SharedResources,
}

impl Module for Worker {
    const ID: ModuleId = WORKER_ROOT;
}

impl Requires<UsersCapability> for Worker {}

impl Start for Worker {
    type Error = &'static str;

    fn start(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        let users_started = self.resources.borrow().users_started;
        if users_started {
            self.resources.borrow_mut().worker_started = true;
            Ok(())
        } else {
            Err("Worker cannot start before Users")
        }
    }
}

impl Ready for Worker {
    type Error = &'static str;

    fn ready(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        if self.resources.borrow().worker_started {
            Ok(())
        } else {
            Err("Worker has not started")
        }
    }
}

impl Drain for Worker {
    type Error = &'static str;

    fn drain(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Stop for Worker {
    type Error = &'static str;

    fn stop(&mut self, _context: &LifecycleContext) -> Result<(), Self::Error> {
        self.resources.borrow_mut().worker_started = false;
        Ok(())
    }
}

fn valid_context(context: &LifecycleContext) -> bool {
    context.application() == APPLICATION && context.process() == WORKER
}

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
    shared: SharedResources,
    participants: Vec<Participant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleFailure {
    Projection(ProjectionError),
    PhaseDeadline {
        phase: Phase,
        module: ModuleId,
    },
    Injected {
        phase: Phase,
        module: ModuleId,
    },
    RequiredHealth(ModuleId),
    Hook {
        phase: Phase,
        module: ModuleId,
        message: &'static str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Participant {
    Database(FakeDatabase),
    Users(FakeUsers),
    Worker(Worker),
}

impl Participant {
    fn new(module: ModuleId, resources: &SharedResources, managed_database: bool) -> Option<Self> {
        Some(match module {
            DATABASE => Self::Database(FakeDatabase {
                resources: resources.clone(),
                managed: managed_database,
            }),
            USERS => Self::Users(FakeUsers {
                resources: resources.clone(),
            }),
            WORKER_ROOT => Self::Worker(Worker {
                resources: resources.clone(),
            }),
            _ => return None,
        })
    }

    fn supports(&self, phase: Phase) -> bool {
        match self {
            Self::Database(module) => match phase {
                Phase::Initialize | Phase::Stop => module.managed,
                Phase::Ready => true,
                _ => false,
            },
            Self::Users(_) => matches!(
                phase,
                Phase::Initialize
                    | Phase::Start
                    | Phase::Ready
                    | Phase::Drain
                    | Phase::Cancel
                    | Phase::Stop
            ),
            Self::Worker(_) => matches!(
                phase,
                Phase::Start | Phase::Ready | Phase::Drain | Phase::Cancel | Phase::Stop
            ),
        }
    }

    fn invoke(
        &mut self,
        phase: Phase,
        context: &LifecycleContext,
    ) -> Option<Result<(), &'static str>> {
        Some(match (self, phase) {
            (Self::Database(module), Phase::Initialize) => Initialize::initialize(module, context),
            (Self::Database(module), Phase::Ready) => Ready::ready(module, context),
            (Self::Database(module), Phase::Stop) => Stop::stop(module, context),
            (Self::Users(module), Phase::Initialize) => Initialize::initialize(module, context),
            (Self::Users(module), Phase::Start) => Start::start(module, context),
            (Self::Users(module), Phase::Ready) => Ready::ready(module, context),
            (Self::Users(module), Phase::Drain) => Drain::drain(module, context),
            (Self::Users(module), Phase::Stop) => Stop::stop(module, context),
            (Self::Worker(module), Phase::Start) => Start::start(module, context),
            (Self::Worker(module), Phase::Ready) => Ready::ready(module, context),
            (Self::Worker(module), Phase::Drain) => Drain::drain(module, context),
            (Self::Worker(module), Phase::Stop) => Stop::stop(module, context),
            _ => return None,
        })
    }

    fn cancel(&mut self) {
        match self {
            Self::Users(module) => module.resources.borrow_mut().users_started = false,
            Self::Worker(module) => module.resources.borrow_mut().worker_started = false,
            Self::Database(_) => {}
        }
    }
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
    Driver::new(frozen, control, None).start()
}

/// Starts with a database that the caller owns and must close.
pub fn start_with_external_database(
    control: TestControl,
    database: &ExternalDatabaseOwner,
) -> Outcome {
    let frozen = match application().freeze(WORKER) {
        Ok(frozen) => frozen,
        Err(error) => return failed_projection(error),
    };
    Driver::new(frozen, control, Some(database.resources.clone())).start()
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
        shared: outcome.shared,
        participants: outcome.participants,
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
    shared: SharedResources,
    participants: Vec<Participant>,
    failure: Option<LifecycleFailure>,
    cleanup_failures: Vec<(Phase, ModuleId)>,
    shutdown_reason: Option<ShutdownReason>,
}

impl Driver {
    fn new(
        frozen: FrozenProcess,
        control: TestControl,
        external_resources: Option<SharedResources>,
    ) -> Self {
        let modules = lifecycle_order(&frozen);
        let managed_database = external_resources.is_none();
        let shared = external_resources.unwrap_or_default();
        let ordered = modules
            .into_iter()
            .filter_map(|module| step(module, control))
            .collect();
        let participants = lifecycle_order(&frozen)
            .into_iter()
            .filter_map(|module| Participant::new(module, &shared, managed_database))
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
            shared,
            participants,
            failure: None,
            cleanup_failures: Vec::new(),
            shutdown_reason: None,
        }
    }

    fn start(mut self) -> Outcome {
        self.status.alive = true;
        self.status.state = LifecycleState::Initializing;
        for step in self.ordered.clone() {
            if !step.initialize || !self.participates(step.module, Phase::Initialize) {
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
            if !step.start || !self.participates(step.module, Phase::Start) {
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
            if !step.ready || !self.participates(step.module, Phase::Ready) {
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
                if let Some(participant) = self.participant_mut(step.module) {
                    participant.cancel();
                }
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
        if let Some(Err(message)) = self.invoke(step.module, phase) {
            return Err(LifecycleFailure::Hook {
                phase,
                module: step.module,
                message,
            });
        }
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
            if let Some(Err(_)) = self.invoke(step.module, phase) {
                self.cleanup_failures.push((phase, step.module));
            }
        }
    }

    fn participant_mut(&mut self, module: ModuleId) -> Option<&mut Participant> {
        self.participants
            .iter_mut()
            .find(|participant| match participant {
                Participant::Database(_) => module == DATABASE,
                Participant::Users(_) => module == USERS,
                Participant::Worker(_) => module == WORKER_ROOT,
            })
    }

    fn participates(&mut self, module: ModuleId, phase: Phase) -> bool {
        self.participant_mut(module)
            .is_some_and(|participant| participant.supports(phase))
    }

    fn invoke(&mut self, module: ModuleId, phase: Phase) -> Option<Result<(), &'static str>> {
        let context = LifecycleContext::new(APPLICATION, WORKER);
        self.participant_mut(module)?.invoke(phase, &context)
    }

    fn record(&mut self, phase: Phase, module: ModuleId) {
        self.events.push(Event {
            phase,
            module,
            elapsed_ms: self.elapsed_ms,
        });
    }

    fn outcome(self) -> Outcome {
        let resources = *self.shared.borrow();
        Outcome {
            status: self.status,
            resources,
            shutdown_reason: self.shutdown_reason,
            events: self.events,
            failure: self.failure,
            cleanup_failures: self.cleanup_failures,
            control: self.control,
            elapsed_ms: self.elapsed_ms,
            initialized: self.initialized,
            started: self.started,
            shared: self.shared,
            participants: self.participants,
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
        shared: Rc::new(RefCell::new(FakeResources::default())),
        participants: Vec::new(),
        control: TestControl::default(),
        elapsed_ms: 0,
        initialized: Vec::new(),
        started: Vec::new(),
    }
}
