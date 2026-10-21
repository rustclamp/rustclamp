//! Console commands for non-web processes (ADR 0024).
//!
//! Modules declare commands; [`CliCommandTarget`] is the
//! [`ContributionTarget`] that checks the names and compiles them into a
//! [`CommandTree`], which parses arguments with `clap` and runs the matching
//! handler. A command has the same shape as `rustclamp::web::Command`:
//! a name, a one-line description and a handler that returns the exit status.
//!
//! Build the tree through the kernel, from the contributions the process
//! selected (`FrozenProcess::compose`), or from a `TargetComposition`:
//!
//! ```
//! use rustclamp::cli::{CliCommandTarget, CommandDeclaration};
//! use rustclamp_core::{ContributionTarget, ModuleId, Qualifier, QualifierId};
//!
//! struct Commands;
//! impl Qualifier for Commands {
//!     const ID: QualifierId = QualifierId::new("app.cli.commands");
//! }
//!
//! let greet = CommandDeclaration::<Commands>::new("greet", "Say hello", |args| {
//!     println!("hello {}", args.get_one::<String>("name").unwrap());
//!     0
//! })
//! .arg(rustclamp::cli::Arg::new("name").required(true));
//!
//! let tree = CliCommandTarget::new("app")
//!     .build(&[(ModuleId::new("app.greet"), greet)])
//!     .unwrap();
//! assert_eq!(tree.run(["app", "greet", "Ada"]).unwrap(), 0);
//! assert!(tree.run(["app", "nope"]).is_err());
//! ```
//!
//! Parse errors (including `--help`) are returned, never printed:
//! `error.exit()` prints them and exits with clap's code.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

pub use clap::{Arg, ArgMatches};
use rustclamp_core::{
    Contribution, ContributionId, ContributionTarget, ContributionTargetId, ModuleId, Qualifier,
};

type Handler = Arc<dyn Fn(&ArgMatches) -> i32 + Send + Sync>;

/// One command a module contributes under qualifier `Q`.
///
/// The qualifier keeps separate trees (public and admin commands, say) apart:
/// a declaration for one cannot enter the other's target.
pub struct CommandDeclaration<Q> {
    command: clap::Command,
    run: Handler,
    qualifier: PhantomData<fn() -> Q>,
}

impl<Q> CommandDeclaration<Q> {
    /// A command `name` with a one-line `description` for help; `run` gets the
    /// parsed arguments of this subcommand and returns the exit status.
    /// The closure can capture what the handler needs (a store, a clock).
    pub fn new(
        name: &'static str,
        description: &'static str,
        run: impl Fn(&ArgMatches) -> i32 + Send + Sync + 'static,
    ) -> Self {
        Self {
            command: clap::Command::new(name).about(description),
            run: Arc::new(run),
            qualifier: PhantomData,
        }
    }

    /// Adds an argument to this command.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<Arg>) -> Self {
        self.command = self.command.arg(arg);
        self
    }

    /// The command name.
    pub fn name(&self) -> &str {
        self.command.get_name()
    }
}

impl<Q: Qualifier> Contribution for CommandDeclaration<Q> {
    const ID: ContributionId = ContributionId::new("rustclamp.cli.command");
}

/// Compiles the command declarations for qualifier `Q` into a [`CommandTree`].
pub struct CliCommandTarget<Q> {
    name: &'static str,
    qualifier: PhantomData<fn() -> Q>,
}

impl<Q> CliCommandTarget<Q> {
    /// A target whose tree is the program `name`, as usage lines show it.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            qualifier: PhantomData,
        }
    }
}

impl<Q: Qualifier> ContributionTarget for CliCommandTarget<Q> {
    type Contribution = CommandDeclaration<Q>;
    type Runtime = CommandTree;
    type Error = CliBuildError;

    const ID: ContributionTargetId = ContributionTargetId::new("rustclamp.cli.commands");

    /// Sorts the commands by name and fails on a name two modules declare.
    /// An empty set builds an empty tree.
    fn build(
        &self,
        contributions: &[(ModuleId, Self::Contribution)],
    ) -> Result<CommandTree, CliBuildError> {
        let mut sorted = contributions.iter().collect::<Vec<_>>();
        sorted.sort_by(|(a_module, a), (b_module, b)| {
            (a.name(), *a_module).cmp(&(b.name(), *b_module))
        });
        for pair in sorted.windows(2) {
            if pair[0].1.name() == pair[1].1.name() {
                return Err(CliBuildError {
                    target: Self::ID,
                    command: pair[0].1.name().to_owned(),
                    first_contributor: pair[0].0,
                    second_contributor: pair[1].0,
                });
            }
        }
        let command = clap::Command::new(self.name)
            .subcommand_required(true)
            .subcommands(sorted.iter().map(|(_, d)| d.command.clone()));
        let handlers = sorted
            .iter()
            .map(|(_, d)| (d.command.get_name().to_owned(), Arc::clone(&d.run)))
            .collect();
        Ok(CommandTree { command, handlers })
    }
}

/// The built commands: a root `clap::Command` and a handler per subcommand.
pub struct CommandTree {
    command: clap::Command,
    handlers: Vec<(String, Handler)>,
}

impl CommandTree {
    /// The root command with every subcommand attached, in name order.
    pub fn command(&self) -> &clap::Command {
        &self.command
    }

    /// The command names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.handlers.iter().map(|(name, _)| name.as_str())
    }

    /// Parses `args` (the program name first, as from `std::env::args_os()`)
    /// and runs the chosen command, returning its exit status.
    ///
    /// A missing or unknown command, bad arguments, `--help` and `help` come
    /// back as the `clap::Error`; nothing is printed here.
    pub fn run<I, T>(&self, args: I) -> Result<i32, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = self.command.clone().try_get_matches_from(args)?;
        let (name, sub) = matches
            .subcommand()
            .expect("subcommand_required makes clap reject a missing command");
        let (_, run) = self
            .handlers
            .iter()
            .find(|(n, _)| n == name)
            .expect("every subcommand in the tree has a handler");
        Ok(run(sub))
    }
}

impl fmt::Debug for CommandTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

/// Two modules declared the same command name for one target.
#[derive(Debug, Eq, PartialEq)]
pub struct CliBuildError {
    /// The target that rejected the declarations.
    pub target: ContributionTargetId,
    /// The duplicated command name.
    pub command: String,
    /// The first module declaring it, in module order.
    pub first_contributor: ModuleId,
    /// The second module declaring it.
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

impl std::error::Error for CliBuildError {}
