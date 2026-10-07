use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustclamp_core::{
    Capability, CapabilityId, Clock, ClockCapability, Contribution, ContributionId,
    ContributionTarget, ContributionTargetId, Module, ModuleId, Provides, Qualifier, QualifierId,
    Requires,
};

pub const HELLO_MODULE: ModuleId = ModuleId::new("example.contribution.hello");
pub const GOODBYE_MODULE: ModuleId = ModuleId::new("example.contribution.goodbye");
pub const ADMIN_STATUS_MODULE: ModuleId = ModuleId::new("example.contribution.admin-status");
pub const CLOCK_MODULE: ModuleId = ModuleId::new("example.contribution.clock");
pub const EPOCH_MODULE: ModuleId = ModuleId::new("example.contribution.epoch");
pub const COMMAND_TARGET: ContributionTargetId = ContributionTargetId::new("example.cli.commands");
pub const COMMAND_CONTRIBUTION: ContributionId = ContributionId::new("example.cli.command");

pub struct PublicCommands;

impl Qualifier for PublicCommands {
    const ID: QualifierId = QualifierId::new("example.cli.public");
}

pub struct AdminCommands;

impl Qualifier for AdminCommands {
    const ID: QualifierId = QualifierId::new("example.cli.admin");
}

pub struct CommandDeclaration<Q: Qualifier> {
    pub name: &'static str,
    handler: fn(Option<&dyn Clock>) -> Result<String, CommandRunError>,
    qualifier: PhantomData<Q>,
}

impl<Q: Qualifier> Copy for CommandDeclaration<Q> {}

impl<Q: Qualifier> Clone for CommandDeclaration<Q> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Q: Qualifier> CommandDeclaration<Q> {
    pub fn new(
        name: &'static str,
        handler: fn(Option<&dyn Clock>) -> Result<String, CommandRunError>,
    ) -> Self {
        Self {
            name,
            handler,
            qualifier: PhantomData,
        }
    }
}

impl<Q: Qualifier> Contribution for CommandDeclaration<Q> {
    const ID: ContributionId = COMMAND_CONTRIBUTION;
}

pub struct CliCommandTarget<Q: Qualifier>(PhantomData<Q>);

impl<Q: Qualifier> CliCommandTarget<Q> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Q: Qualifier> Default for CliCommandTarget<Q> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Q: Qualifier> ContributionTarget for CliCommandTarget<Q> {
    type Contribution = CommandDeclaration<Q>;
    type Runtime = CommandTree;
    type Error = CliBuildError;

    const ID: ContributionTargetId = COMMAND_TARGET;

    fn build(
        &self,
        contributions: &[(ModuleId, Self::Contribution)],
    ) -> Result<Self::Runtime, Self::Error> {
        let mut commands = contributions
            .iter()
            .map(|(contributor, declaration)| Command {
                name: declaration.name,
                contributor: *contributor,
                handler: declaration.handler,
            })
            .collect::<Vec<_>>();
        commands.sort_unstable_by_key(|command| (command.name, command.contributor));

        for pair in commands.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(CliBuildError {
                    target: Self::ID,
                    command: pair[0].name,
                    first_contributor: pair[0].contributor,
                    second_contributor: pair[1].contributor,
                });
            }
        }

        Ok(CommandTree {
            commands: commands
                .into_iter()
                .map(|command| RuntimeCommand {
                    name: command.name,
                    handler: command.handler,
                })
                .collect(),
        })
    }
}

struct Command {
    name: &'static str,
    contributor: ModuleId,
    handler: fn(Option<&dyn Clock>) -> Result<String, CommandRunError>,
}

#[derive(Debug)]
struct RuntimeCommand {
    name: &'static str,
    handler: fn(Option<&dyn Clock>) -> Result<String, CommandRunError>,
}

#[derive(Debug)]
pub struct CommandTree {
    commands: Vec<RuntimeCommand>,
}

impl CommandTree {
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.commands.iter().map(|command| command.name)
    }

    /// Bytes held by the tree and its command slice, excluding allocator metadata.
    pub fn storage_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.commands.capacity() * std::mem::size_of::<RuntimeCommand>()
    }

    pub fn execute(
        &self,
        name: &str,
        clock: Option<&dyn Clock>,
    ) -> Result<String, CommandRunError> {
        let command = self
            .commands
            .iter()
            .find(|command| command.name == name)
            .ok_or_else(|| CommandRunError::UnknownCommand(name.to_owned()))?;
        (command.handler)(clock)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct CliBuildError {
    pub target: ContributionTargetId,
    pub command: &'static str,
    pub first_contributor: ModuleId,
    pub second_contributor: ModuleId,
}

impl fmt::Display for CliBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "target {} has duplicate command {:?} from {} and {}",
            self.target.as_str(),
            self.command,
            self.first_contributor.as_str(),
            self.second_contributor.as_str()
        )
    }
}

impl Error for CliBuildError {}

#[derive(Debug, Eq, PartialEq)]
pub enum CommandRunError {
    UnknownCommand(String),
    MissingClock,
}

impl fmt::Display for CommandRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(name) => write!(f, "unknown command {name:?}"),
            Self::MissingClock => f.write_str("hello command requires a Clock"),
        }
    }
}

impl Error for CommandRunError {}

/// A public command contributor whose declaration cannot enter an admin target.
///
/// ```compile_fail
/// use rustclamp_example_contribution::{AdminCommands, CommandDeclaration, HelloModule};
/// let _: CommandDeclaration<AdminCommands> = HelloModule::contribution();
/// ```
pub struct HelloModule;

impl Module for HelloModule {
    const ID: ModuleId = HELLO_MODULE;
}

impl Requires<ClockCapability> for HelloModule {}

impl HelloModule {
    pub fn contribution() -> CommandDeclaration<PublicCommands> {
        CommandDeclaration::new("hello", hello)
    }
}

pub struct GoodbyeModule;

impl Module for GoodbyeModule {
    const ID: ModuleId = GOODBYE_MODULE;
}

impl GoodbyeModule {
    pub fn contribution() -> CommandDeclaration<PublicCommands> {
        CommandDeclaration::new("goodbye", goodbye)
    }
}

pub struct AdminStatusModule;

impl Module for AdminStatusModule {
    const ID: ModuleId = ADMIN_STATUS_MODULE;
}

impl AdminStatusModule {
    pub fn contribution() -> CommandDeclaration<AdminCommands> {
        CommandDeclaration::new("status", admin_status)
    }
}

fn hello(clock: Option<&dyn Clock>) -> Result<String, CommandRunError> {
    let seconds = clock
        .ok_or(CommandRunError::MissingClock)?
        .now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(format!("hello at {seconds}"))
}

fn goodbye(_: Option<&dyn Clock>) -> Result<String, CommandRunError> {
    Ok("goodbye".to_owned())
}

fn admin_status(_: Option<&dyn Clock>) -> Result<String, CommandRunError> {
    Ok("admin status".to_owned())
}

pub struct EpochSourceCapability;

pub trait EpochSource {
    fn seconds(&self) -> u64;
}

impl Capability for EpochSourceCapability {
    type Value = dyn EpochSource;

    const ID: CapabilityId = CapabilityId::new("example.epoch-source");
}

pub struct FixedEpoch(pub u64);

impl EpochSource for FixedEpoch {
    fn seconds(&self) -> u64 {
        self.0
    }
}

pub struct EpochModule(pub FixedEpoch);

impl Module for EpochModule {
    const ID: ModuleId = EPOCH_MODULE;
}

impl Provides<EpochSourceCapability> for EpochModule {
    fn provided_value(&self) -> &(dyn EpochSource + 'static) {
        &self.0
    }
}

pub struct ClockModule {
    epoch_seconds: u64,
}

impl ClockModule {
    pub fn new(epoch_source: &dyn EpochSource) -> Self {
        Self {
            epoch_seconds: epoch_source.seconds(),
        }
    }
}

impl Module for ClockModule {
    const ID: ModuleId = CLOCK_MODULE;
}

impl Requires<EpochSourceCapability> for ClockModule {}

impl Clock for ClockModule {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.epoch_seconds)
    }
}

impl Provides<ClockCapability> for ClockModule {
    fn provided_value(&self) -> &(dyn Clock + 'static) {
        self
    }
}
