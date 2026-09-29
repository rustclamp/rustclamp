//! Synchronous fake-resource proof for process-scoped lifecycle coordination.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Duration;

use rustclamp_core::{
    ApplicationId, Capability, CapabilityId, Drain, ExecutionId, Initialize, LifecycleContext,
    Module, ModuleId, ProcessId, Provides, Ready, Requires, Start, Stop,
};
use rustclamp_kernel::{ApplicationBlueprint, FrozenProcess, ProjectionError};
use rustclamp_runtime::{FailurePolicy, TaskContext, TaskDefinition, TaskKind};

pub const APPLICATION: ApplicationId = ApplicationId::new("example.lifecycle.application");
pub const WORKER: ProcessId = ProcessId::new("example.lifecycle.worker");
pub const REPORTER: ProcessId = ProcessId::new("example.lifecycle.reporter");
pub const EXECUTION: ExecutionId = ExecutionId::new("example.lifecycle.worker-root");
pub const MAINTENANCE_EXECUTION: ExecutionId =
    ExecutionId::new("example.lifecycle.maintenance-root");
pub const REPORTER_EXECUTION: ExecutionId = ExecutionId::new("example.lifecycle.reporter-root");
pub const WORKER_ROOT: ModuleId = ModuleId::new("example.lifecycle.worker");
pub const USERS: ModuleId = ModuleId::new("example.lifecycle.users");
pub const DATABASE: ModuleId = ModuleId::new("example.lifecycle.database");

/// Runtime implementation selected for one process projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessRuntimeKind {
    /// Executor-free synchronous driver for deterministic work.
    Manual,
    /// Optional Tokio adapter for supervised task work.
    Tokio,
}

/// Returns this example's explicit process-to-runtime mapping.
pub fn runtime_for_process(process: ProcessId) -> Option<ProcessRuntimeKind> {
    match process {
        WORKER => Some(ProcessRuntimeKind::Tokio),
        REPORTER => Some(ProcessRuntimeKind::Manual),
        _ => None,
    }
}

/// Builds one process-owned service task using a supplied runtime.
pub fn service_task(process: ProcessId) -> TaskDefinition {
    TaskDefinition::new(
        process,
        "lifecycle-example-service",
        TaskKind::Service,
        process == WORKER,
        Some(Duration::from_secs(1)),
        if process == WORKER {
            FailurePolicy::RestartOnce
        } else {
            FailurePolicy::Degrade
        },
        |context: TaskContext| {
            if context.is_cancelled() {
                Err("service cancelled".to_owned())
            } else {
                Ok(())
            }
        },
    )
}

const USERS_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.users-service");
const DATABASE_CAPABILITY: CapabilityId = CapabilityId::new("example.lifecycle.database");

type SharedResources = Rc<RefCell<FakeResources>>;

/// Application-owned fake database shared by coexisting process projections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationResources {
    database_open: Rc<Cell<bool>>,
}

impl ApplicationResources {
    /// Creates the application resource before starting its process projections.
    pub fn new() -> Self {
        Self {
            database_open: Rc::new(Cell::new(true)),
        }
    }

    /// Reports whether the application owner still holds the database open.
    pub fn database_open(&self) -> bool {
        self.database_open.get()
    }

    /// Closes the application-owned database after its users have stopped.
    pub fn close(&self) {
        self.database_open.set(false);
    }
}

impl Default for ApplicationResources {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-owned state backed by the application database.
#[derive(Debug, Eq, PartialEq)]
pub struct ProcessResources {
    process: ProcessId,
    database: Rc<Cell<bool>>,
    active: Cell<bool>,
}

impl ProcessResources {
    /// Creates isolated process state from a frozen projection.
    pub fn new(projection: &FrozenProcess, application: &ApplicationResources) -> Self {
        Self {
            process: projection.inspection().process(),
            database: application.database_open.clone(),
            active: Cell::new(false),
        }
    }

    /// Starts this process if its shared application database is available.
    pub fn start(&self) -> bool {
        if !self.database.get() {
            return false;
        }
        self.active.set(true);
        true
    }

    /// Stops only this process's state.
    pub fn stop(&self) {
        self.active.set(false);
    }

    /// Returns the projected process identity.
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// Reports whether this process's state is active.
    pub fn is_active(&self) -> bool {
        self.active.get()
    }
}

/// One explicitly owned resource for a declared execution root.
#[derive(Debug, Eq, PartialEq)]
pub struct ExecutionResources {
    execution: ExecutionId,
    database: Rc<Cell<bool>>,
    active: Cell<bool>,
}

impl ExecutionResources {
    /// Creates a resource only for an execution in the frozen process projection.
    pub fn new(
        projection: &FrozenProcess,
        execution: ExecutionId,
        application: &ApplicationResources,
    ) -> Option<Self> {
        projection
            .inspection()
            .roots()
            .contains(&execution)
            .then(|| Self {
                execution,
                database: application.database_open.clone(),
                active: Cell::new(false),
            })
    }

    /// Starts this execution while its application database is available.
    pub fn start(&self) -> bool {
        if !self.database.get() {
            return false;
        }
        self.active.set(true);
        true
    }

    /// Stops only this execution's state.
    pub fn stop(&self) {
        self.active.set(false);
    }

    /// Returns the execution identity that owns this resource.
    pub const fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// Reports whether this execution's state is active.
    pub fn is_active(&self) -> bool {
        self.active.get()
    }
}

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
        .add_execution(MAINTENANCE_EXECUTION, WORKER_ROOT)
        .add_execution(REPORTER_EXECUTION, WORKER_ROOT)
        .add_process(WORKER, vec![EXECUTION, MAINTENANCE_EXECUTION])
        .add_process(REPORTER, vec![REPORTER_EXECUTION])
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
    TaskStop,
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

/// Converts an adapter signal into the lifecycle's platform-neutral reason.
#[cfg(feature = "tokio-runtime")]
pub const fn shutdown_reason_for_signal(
    _signal: rustclamp_runtime::tokio_runtime::ShutdownSignal,
) -> ShutdownReason {
    ShutdownReason::Requested
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
    pub task_stop_ms: u64,
    pub cleanup_ms: u64,
    pub flush_ms: u64,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            initialize_ms: 100,
            start_ms: 100,
            drain_ms: 100,
            stop_ms: 100,
            task_stop_ms: 100,
            cleanup_ms: 100,
            flush_ms: 10,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub phase: Phase,
    pub module: ModuleId,
    pub elapsed_ms: u64,
}

/// Resource owner reported by the lifecycle inspection view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOwner {
    /// The process lifecycle coordinator creates and releases this resource.
    Process,
    /// The caller created the resource and retains its lifecycle responsibility.
    External,
}

/// Last known acquisition and cleanup state for a module resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupState {
    /// The coordinator has not acquired this resource.
    NotAcquired,
    /// The resource is still active or has not yet been stopped.
    Active,
    /// Cleanup completed successfully.
    Stopped,
    /// Cleanup was attempted but failed or exceeded its deadline.
    Failed,
    /// The resource belongs to its external owner and was not acquired here.
    External,
}

/// One module's ownership, lifecycle participation, and cleanup state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedModule {
    module: ModuleId,
    owner: ResourceOwner,
    phases: Vec<Phase>,
    cleanup: CleanupState,
}

impl InspectedModule {
    /// Returns the module identity.
    pub const fn module(&self) -> ModuleId {
        self.module
    }

    /// Returns the owner responsible for the resource.
    pub const fn owner(&self) -> ResourceOwner {
        self.owner
    }

    /// Returns phases implemented by this process participant.
    pub fn phases(&self) -> &[Phase] {
        &self.phases
    }

    /// Returns its current cleanup state.
    pub const fn cleanup(&self) -> CleanupState {
        self.cleanup
    }
}

/// A provider dependency in the lifecycle graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleDependency {
    provider: ModuleId,
    consumer: ModuleId,
}

impl LifecycleDependency {
    /// Returns the required provider module.
    pub const fn provider(&self) -> ModuleId {
        self.provider
    }

    /// Returns the dependent consumer module.
    pub const fn consumer(&self) -> ModuleId {
        self.consumer
    }
}

/// Inspectable ownership and lifecycle plan for one frozen process projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleInspection {
    application: ApplicationId,
    process: ProcessId,
    roots: Vec<ExecutionId>,
    modules: Vec<InspectedModule>,
    dependencies: Vec<LifecycleDependency>,
}

impl LifecycleInspection {
    /// Returns the owning application identity.
    pub const fn application(&self) -> ApplicationId {
        self.application
    }

    /// Returns the inspected process identity.
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// Returns the process execution roots.
    pub fn roots(&self) -> &[ExecutionId] {
        &self.roots
    }

    /// Returns lifecycle participants and their current cleanup states.
    pub fn modules(&self) -> &[InspectedModule] {
        &self.modules
    }

    /// Returns provider-to-consumer dependency edges.
    pub fn dependencies(&self) -> &[LifecycleDependency] {
        &self.dependencies
    }

    fn module_mut(&mut self, module: ModuleId) -> Option<&mut InspectedModule> {
        self.modules.iter_mut().find(|entry| entry.module == module)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub status: ProcessStatus,
    pub resources: FakeResources,
    pub shutdown_reason: Option<ShutdownReason>,
    pub events: Vec<Event>,
    pub failure: Option<LifecycleFailure>,
    pub cleanup_failures: Vec<(Phase, ModuleId)>,
    /// Whether final diagnostics used the in-memory fallback path.
    pub diagnostic_fallback: bool,
    inspection: LifecycleInspection,
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
                Phase::Start
                    | Phase::Ready
                    | Phase::Drain
                    | Phase::Cancel
                    | Phase::TaskStop
                    | Phase::Stop
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
    task_stop: bool,
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
    task_stop: false,
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
    task_stop: false,
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
    task_stop: true,
    initialize_ms: 0,
    start_ms: 1,
    drain_ms: 2,
    stop_ms: 1,
    fail: None,
    health: Health::Healthy,
    required_health: true,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestControl {
    pub deadlines: Deadlines,
    pub fail: Option<(Phase, ModuleId)>,
    pub required_health: Option<(ModuleId, Health)>,
    pub optional_health: Option<(ModuleId, Health)>,
    pub cleanup_failure: Option<(Phase, ModuleId)>,
    /// Simulated duration of final diagnostics delivery.
    pub flush_ms: u64,
    /// Simulates unavailable telemetry while preserving local diagnostics.
    pub telemetry_unavailable: bool,
    /// Simulated duration of stopping the process's supervised task.
    pub task_stop_ms: u64,
}

impl Default for TestControl {
    fn default() -> Self {
        Self {
            deadlines: Deadlines::default(),
            fail: None,
            required_health: None,
            optional_health: None,
            cleanup_failure: None,
            flush_ms: 1,
            telemetry_unavailable: false,
            task_stop_ms: 1,
        }
    }
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

    /// Returns the ownership and lifecycle inspection snapshot.
    pub const fn inspection(&self) -> &LifecycleInspection {
        &self.inspection
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
        inspection: outcome.inspection,
        shared: outcome.shared,
        participants: outcome.participants,
        initialized: outcome.initialized,
        started: outcome.started,
        status: outcome.status,
        failure: outcome.failure,
        cleanup_failures: outcome.cleanup_failures,
        diagnostic_fallback: outcome.diagnostic_fallback,
        shutdown_reason: None,
        phase_elapsed: BTreeMap::new(),
        cleanup_started_ms: None,
    };
    driver.stop(reason);
    driver.outcome()
}

struct Driver {
    ordered: Vec<Step>,
    control: TestControl,
    elapsed_ms: u64,
    events: Vec<Event>,
    inspection: LifecycleInspection,
    initialized: Vec<Step>,
    started: Vec<Step>,
    status: ProcessStatus,
    shared: SharedResources,
    participants: Vec<Participant>,
    failure: Option<LifecycleFailure>,
    cleanup_failures: Vec<(Phase, ModuleId)>,
    diagnostic_fallback: bool,
    shutdown_reason: Option<ShutdownReason>,
    phase_elapsed: BTreeMap<Phase, u64>,
    cleanup_started_ms: Option<u64>,
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
        let participants: Vec<_> = lifecycle_order(&frozen)
            .into_iter()
            .filter_map(|module| Participant::new(module, &shared, managed_database))
            .collect();
        let inspection = lifecycle_inspection(&frozen, &participants);
        Self {
            ordered,
            control,
            elapsed_ms: 0,
            events: Vec::new(),
            inspection,
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
            diagnostic_fallback: control.telemetry_unavailable,
            shutdown_reason: None,
            phase_elapsed: BTreeMap::new(),
            cleanup_started_ms: None,
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
        self.cleanup_started_ms = Some(self.elapsed_ms);
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
        for step in self.started.clone().into_iter().rev() {
            if step.task_stop {
                self.perform_task_stop(step);
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
        self.flush_diagnostics();
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
        let used = self.phase_elapsed.get(&phase).copied().unwrap_or_default();
        if used.saturating_add(duration) > deadline_ms {
            return Err(LifecycleFailure::PhaseDeadline {
                phase,
                module: step.module,
            });
        }
        self.elapsed_ms += duration;
        self.phase_elapsed.insert(phase, used + duration);
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
        if matches!(phase, Phase::Initialize | Phase::Start)
            && let Some(module) = self.inspection.module_mut(step.module)
        {
            module.cleanup = CleanupState::Active;
        }
        Ok(())
    }

    fn perform_cleanup(&mut self, step: Step, phase: Phase, deadline_ms: u64) {
        let duration = match phase {
            Phase::Drain => step.drain_ms,
            Phase::Stop => step.stop_ms,
            _ => 0,
        };
        let used = self.phase_elapsed.get(&phase).copied().unwrap_or_default();
        let cleanup_used = self
            .elapsed_ms
            .saturating_sub(self.cleanup_started_ms.unwrap_or(self.elapsed_ms));
        if used.saturating_add(duration) > deadline_ms
            || cleanup_used.saturating_add(duration) > self.control.deadlines.cleanup_ms
        {
            self.cleanup_failures.push((phase, step.module));
            self.mark_cleanup(step.module, CleanupState::Failed);
            return;
        }
        self.elapsed_ms += duration;
        self.phase_elapsed.insert(phase, used + duration);
        self.record(phase, step.module);
        if self.control.cleanup_failure == Some((phase, step.module)) {
            self.cleanup_failures.push((phase, step.module));
            self.mark_cleanup(step.module, CleanupState::Failed);
        } else {
            if let Some(Err(_)) = self.invoke(step.module, phase) {
                self.cleanup_failures.push((phase, step.module));
                self.mark_cleanup(step.module, CleanupState::Failed);
            } else if phase == Phase::Stop {
                self.mark_cleanup(step.module, CleanupState::Stopped);
            }
        }
    }

    fn mark_cleanup(&mut self, module: ModuleId, state: CleanupState) {
        if let Some(module) = self.inspection.module_mut(module)
            && (module.cleanup != CleanupState::Failed || state == CleanupState::Failed)
        {
            module.cleanup = state;
        }
    }

    fn perform_task_stop(&mut self, step: Step) {
        let duration = self.control.task_stop_ms;
        let used = self
            .phase_elapsed
            .get(&Phase::TaskStop)
            .copied()
            .unwrap_or_default();
        let cleanup_used = self
            .elapsed_ms
            .saturating_sub(self.cleanup_started_ms.unwrap_or(self.elapsed_ms));
        if used.saturating_add(duration) > self.control.deadlines.task_stop_ms
            || cleanup_used.saturating_add(duration) > self.control.deadlines.cleanup_ms
        {
            self.cleanup_failures.push((Phase::TaskStop, step.module));
            self.mark_cleanup(step.module, CleanupState::Failed);
            return;
        }
        self.elapsed_ms += duration;
        self.phase_elapsed.insert(Phase::TaskStop, used + duration);
        self.record(Phase::TaskStop, step.module);
    }

    fn flush_diagnostics(&mut self) {
        let mut duration = self.control.flush_ms;
        if self.control.telemetry_unavailable {
            self.diagnostic_fallback = true;
            duration = 0;
        }
        let phase_used = self
            .phase_elapsed
            .get(&Phase::Flush)
            .copied()
            .unwrap_or_default();
        let cleanup_used = self
            .elapsed_ms
            .saturating_sub(self.cleanup_started_ms.unwrap_or(self.elapsed_ms));
        let allowed = self
            .control
            .deadlines
            .flush_ms
            .saturating_sub(phase_used)
            .min(
                self.control
                    .deadlines
                    .cleanup_ms
                    .saturating_sub(cleanup_used),
            );
        if duration > allowed {
            self.diagnostic_fallback = true;
            duration = allowed;
        }
        self.elapsed_ms += duration;
        self.phase_elapsed
            .insert(Phase::Flush, phase_used + duration);
        // The event log is the dependency-free fallback when telemetry is unavailable.
        self.events.push(Event {
            phase: Phase::Flush,
            module: WORKER_ROOT,
            elapsed_ms: self.elapsed_ms,
        });
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
            inspection: self.inspection,
            failure: self.failure,
            cleanup_failures: self.cleanup_failures,
            diagnostic_fallback: self.diagnostic_fallback,
            control: self.control,
            elapsed_ms: self.elapsed_ms,
            initialized: self.initialized,
            started: self.started,
            shared: self.shared,
            participants: self.participants,
        }
    }
}

fn lifecycle_inspection(
    frozen: &FrozenProcess,
    participants: &[Participant],
) -> LifecycleInspection {
    let phases = [
        Phase::Initialize,
        Phase::Start,
        Phase::Ready,
        Phase::Drain,
        Phase::Cancel,
        Phase::TaskStop,
        Phase::Stop,
    ];
    let mut dependencies: Vec<_> = frozen
        .inspection()
        .requirements()
        .iter()
        .filter_map(|requirement| {
            requirement.provider().map(|provider| LifecycleDependency {
                provider,
                consumer: requirement.consumer(),
            })
        })
        .collect();
    dependencies.sort_by_key(|edge| (edge.provider, edge.consumer));
    dependencies.dedup_by_key(|edge| (edge.provider, edge.consumer));

    LifecycleInspection {
        application: frozen.inspection().application(),
        process: frozen.inspection().process(),
        roots: frozen.inspection().roots().to_vec(),
        modules: participants
            .iter()
            .map(|participant| {
                let (module, owner) = match participant {
                    Participant::Database(database) => (
                        DATABASE,
                        if database.managed {
                            ResourceOwner::Process
                        } else {
                            ResourceOwner::External
                        },
                    ),
                    Participant::Users(_) => (USERS, ResourceOwner::Process),
                    Participant::Worker(_) => (WORKER_ROOT, ResourceOwner::Process),
                };
                InspectedModule {
                    module,
                    owner,
                    phases: phases
                        .iter()
                        .copied()
                        .filter(|phase| participant.supports(*phase))
                        .collect(),
                    cleanup: if owner == ResourceOwner::External {
                        CleanupState::External
                    } else {
                        CleanupState::NotAcquired
                    },
                }
            })
            .collect(),
        dependencies,
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
        inspection: LifecycleInspection {
            application: APPLICATION,
            process: WORKER,
            roots: Vec::new(),
            modules: Vec::new(),
            dependencies: Vec::new(),
        },
        failure: Some(LifecycleFailure::Projection(error)),
        cleanup_failures: Vec::new(),
        diagnostic_fallback: true,
        shared: Rc::new(RefCell::new(FakeResources::default())),
        participants: Vec::new(),
        control: TestControl::default(),
        elapsed_ms: 0,
        initialized: Vec::new(),
        started: Vec::new(),
    }
}
