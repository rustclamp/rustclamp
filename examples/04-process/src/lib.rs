use rustclamp_core::{
    ApplicationId, Capability, CapabilityId, ClockCapability, ContributionId, ContributionTargetId,
    ExecutionId, ModuleId, ProcessId, QualifierId,
};
use rustclamp_kernel::{ApplicationBlueprint, FrozenProcess, ProcessProjection, ProjectionError};

pub const APPLICATION: ApplicationId = ApplicationId::new("example.process.application");
pub const CLI_PROCESS: ProcessId = ProcessId::new("example.process.cli");
pub const WORKER_PROCESS: ProcessId = ProcessId::new("example.process.worker");
pub const CLI_EXECUTION: ExecutionId = ExecutionId::new("example.execution.cli");
pub const WORKER_EXECUTION: ExecutionId = ExecutionId::new("example.execution.worker");

pub const CLI_ROOT: ModuleId = ModuleId::new("example.process.cli-root");
pub const WORKER_ROOT: ModuleId = ModuleId::new("example.process.worker-root");
pub const GREETING_COMMAND: ModuleId = ModuleId::new("example.process.greeting-command");
pub const CLOCK_PROVIDER: ModuleId = ModuleId::new("example.process.clock");
pub const EPOCH_PROVIDER: ModuleId = ModuleId::new("example.process.epoch");
pub const QUEUE_PROVIDER: ModuleId = ModuleId::new("example.process.queue");
pub const DORMANT_MODULE: ModuleId = ModuleId::new("example.process.dormant");
pub const UNAVAILABLE_DATABASE: ModuleId = ModuleId::new("example.process.unavailable-database");
pub const WORKER_CONCURRENCY_SETTING: &str = "WORKER_CONCURRENCY";

const EPOCH_CAPABILITY: CapabilityId = CapabilityId::new("example.process.epoch-source");
const QUEUE_CAPABILITY: CapabilityId = CapabilityId::new("example.process.queue");
const DATABASE_CAPABILITY: CapabilityId = CapabilityId::new("example.process.database");
const CLI_COMMANDS: ContributionTargetId =
    ContributionTargetId::new("example.process.cli-commands");
const PUBLIC_QUALIFIER: QualifierId = QualifierId::new("example.process.public");
const GREETING_CONTRIBUTION: ContributionId =
    ContributionId::new("example.process.greeting-command");

/// The CLI execution root in the application blueprint.
pub struct CliProcess;

impl CliProcess {
    /// Stable identity for this process projection.
    pub const ID: ProcessId = CLI_PROCESS;
}

/// The Worker execution root in the application blueprint.
pub struct WorkerProcess;

impl WorkerProcess {
    /// Stable identity for this process projection.
    pub const ID: ProcessId = WORKER_PROCESS;
}

/// Declares one application containing CLI and Worker process projections.
pub fn application() -> ApplicationBlueprint {
    let mut application = ApplicationBlueprint::new(APPLICATION);
    for module in [
        CLI_ROOT,
        WORKER_ROOT,
        GREETING_COMMAND,
        CLOCK_PROVIDER,
        EPOCH_PROVIDER,
        QUEUE_PROVIDER,
        DORMANT_MODULE,
    ] {
        application.add_module(module);
    }

    application
        .add_execution(CLI_EXECUTION, CLI_ROOT)
        .add_execution(WORKER_EXECUTION, WORKER_ROOT)
        .add_process(CLI_PROCESS, vec![CLI_EXECUTION])
        .add_process(WORKER_PROCESS, vec![WORKER_EXECUTION])
        .consume_target(CLI_ROOT, CLI_COMMANDS, PUBLIC_QUALIFIER)
        .add_contribution(
            GREETING_COMMAND,
            CLI_COMMANDS,
            PUBLIC_QUALIFIER,
            GREETING_CONTRIBUTION,
        )
        .require_provider(GREETING_COMMAND, ClockCapability::ID, None, CLOCK_PROVIDER)
        .require_provider(CLOCK_PROVIDER, EPOCH_CAPABILITY, None, EPOCH_PROVIDER)
        .require_provider(WORKER_ROOT, ClockCapability::ID, None, CLOCK_PROVIDER)
        .require_provider(WORKER_ROOT, QUEUE_CAPABILITY, None, QUEUE_PROVIDER)
        .require_provider(
            DORMANT_MODULE,
            DATABASE_CAPABILITY,
            None,
            UNAVAILABLE_DATABASE,
        );
    application
}

/// Resolves the requested process and returns its selected runtime description.
pub fn run(process: ProcessId) -> Result<String, StartupError> {
    run_with_worker_setting(process, || None)
}

/// Resolves a projection and reads Worker configuration only when Worker is included.
pub fn run_with_worker_setting(
    process: ProcessId,
    read_setting: impl FnOnce() -> Option<String>,
) -> Result<String, StartupError> {
    let mut probe = StartupProbe::default();
    let instance = start_process(process, read_setting, &mut probe)?;
    let modules = instance
        .modules()
        .iter()
        .map(|module| module.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let config = instance
        .worker_concurrency()
        .map(|value| format!("; worker concurrency: {value}"))
        .unwrap_or_default();
    Ok(format!(
        "{} projection includes: {modules}",
        instance.process().as_str()
    ) + &config)
}

/// Starts one process from the same frozen projection used for runtime assembly.
pub fn start_process(
    process: ProcessId,
    read_setting: impl FnOnce() -> Option<String>,
    probe: &mut StartupProbe,
) -> Result<ProcessInstance, StartupError> {
    let frozen = application()
        .freeze(process)
        .map_err(StartupError::Projection)?;
    initialize(frozen, read_setting, probe)
}

/// Freezes and starts CLI and Worker together from one application blueprint.
pub fn start_host(
    mut read_setting: &mut impl FnMut() -> Option<String>,
    probe: &mut StartupProbe,
) -> Result<Vec<ProcessInstance>, StartupError> {
    let frozen = application()
        .freeze_processes(&[CLI_PROCESS, WORKER_PROCESS])
        .map_err(StartupError::Projection)?;
    frozen
        .into_iter()
        .map(|process| initialize(process, &mut read_setting, probe))
        .collect()
}

fn initialize(
    frozen: FrozenProcess,
    read_setting: impl FnOnce() -> Option<String>,
    probe: &mut StartupProbe,
) -> Result<ProcessInstance, StartupError> {
    let projection = frozen.inspection();
    let process = projection.process();
    let worker_concurrency = if projection.includes(WORKER_ROOT) {
        let value = read_setting().ok_or(StartupError::WorkerSetting {
            process,
            setting: WORKER_CONCURRENCY_SETTING,
            problem: WorkerSettingProblem::Missing,
        })?;
        let parsed = value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(StartupError::WorkerSetting {
                process,
                setting: WORKER_CONCURRENCY_SETTING,
                problem: WorkerSettingProblem::Invalid,
            })?;
        probe.worker_factory_calls += 1;
        probe.worker_target_builds += 1;
        probe.worker_startup_calls += 1;
        Some(parsed)
    } else {
        None
    };
    probe.cli_target_builds += frozen
        .runtime()
        .contributions()
        .iter()
        .filter(|entry| entry.contributor() == GREETING_COMMAND)
        .count();
    Ok(ProcessInstance {
        process,
        modules: frozen.runtime().modules().to_vec(),
        worker_concurrency,
        mutable_state: 0,
    })
}

/// Observable calls made while assembling the in-memory host fixture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StartupProbe {
    worker_factory_calls: usize,
    worker_target_builds: usize,
    cli_target_builds: usize,
    worker_startup_calls: usize,
}

impl StartupProbe {
    /// Returns the number of Worker factory invocations.
    pub const fn worker_factory_calls(&self) -> usize {
        self.worker_factory_calls
    }
    /// Returns the number of Worker-only target builds.
    pub const fn worker_target_builds(&self) -> usize {
        self.worker_target_builds
    }
    /// Returns the number of CLI command-target builds.
    pub const fn cli_target_builds(&self) -> usize {
        self.cli_target_builds
    }
    /// Returns the number of Worker startup callbacks.
    pub const fn worker_startup_calls(&self) -> usize {
        self.worker_startup_calls
    }
}

/// A fake process instance with process-local mutable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessInstance {
    process: ProcessId,
    modules: Vec<ModuleId>,
    worker_concurrency: Option<usize>,
    mutable_state: usize,
}

impl ProcessInstance {
    /// Returns this instance's process identity.
    pub const fn process(&self) -> ProcessId {
        self.process
    }
    /// Returns modules in the frozen runtime projection.
    pub fn modules(&self) -> &[ModuleId] {
        &self.modules
    }
    /// Returns the validated Worker concurrency, if this is a Worker instance.
    pub const fn worker_concurrency(&self) -> Option<usize> {
        self.worker_concurrency
    }
    /// Increments state owned only by this in-memory process instance.
    pub fn increment_state(&mut self) {
        self.mutable_state += 1;
    }
    /// Returns the current process-local state value.
    pub const fn state(&self) -> usize {
        self.mutable_state
    }
}

/// A startup failure while resolving the projection or its selected settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupError {
    /// The requested process projection could not be resolved.
    Projection(ProjectionError),
    /// The selected Worker projection has invalid required configuration.
    WorkerSetting {
        /// Process whose selected projection requires the setting.
        process: ProcessId,
        /// Configuration key that failed validation.
        setting: &'static str,
        /// Whether the setting was missing or invalid.
        problem: WorkerSettingProblem,
    },
}

/// Why Worker configuration validation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerSettingProblem {
    /// No value was supplied.
    Missing,
    /// The value was not a positive integer.
    Invalid,
}

/// Returns the derived projection for inspection by this example's tests.
pub fn inspect(process: ProcessId) -> Result<ProcessProjection, ProjectionError> {
    application().project(process)
}

/// Formats identities, selections, target edges, paths, and omissions for inspection.
pub fn inspection_text(process: ProcessId) -> Result<String, ProjectionError> {
    let projection = inspect(process)?;
    let mut lines = vec![
        format!("application: {}", projection.application().as_str()),
        format!("process: {}", projection.process().as_str()),
        format!(
            "roots: {}",
            projection
                .roots()
                .iter()
                .map(|root| root.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "included modules:".to_owned(),
    ];
    for module in projection.included_modules() {
        lines.push(format!(
            "  {} via {} ({:?})",
            module.module().as_str(),
            module
                .path()
                .iter()
                .map(|part| part.as_str())
                .collect::<Vec<_>>()
                .join(" -> "),
            module.reason()
        ));
    }
    lines.push("resolved requirements:".to_owned());
    for requirement in projection.requirements() {
        lines.push(format!(
            "  {} requires {} qualifier={:?} provider={:?} optional={} selection={:?} replaced={:?}",
            requirement.consumer().as_str(),
            requirement.capability().as_str(),
            requirement.qualifier().map(QualifierId::as_str),
            requirement.provider().map(ModuleId::as_str),
            requirement.optional(),
            requirement.selection(),
            requirement.replaced_provider().map(ModuleId::as_str),
        ));
    }
    lines.push("resolved contributions:".to_owned());
    for contribution in projection.contributions() {
        lines.push(format!(
            "  {} consumes {} qualifier={} from {} contribution={}",
            contribution.consumer().as_str(),
            contribution.target().as_str(),
            contribution.qualifier().as_str(),
            contribution.contributor().as_str(),
            contribution.contribution().as_str(),
        ));
    }
    lines.push("excluded modules:".to_owned());
    for excluded in projection.exclusions() {
        lines.push(format!(
            "  {} ({:?})",
            excluded.module().as_str(),
            excluded.reason()
        ));
    }
    Ok(lines.join("\n"))
}
