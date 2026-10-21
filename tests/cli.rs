//! The CLI target built through a frozen process projection (ADR 0024).
#![cfg(feature = "cli")]

use std::sync::{Arc, Mutex};

use clap::error::ErrorKind;
use rustclamp::cli::{Arg, CliBuildError, CliCommandTarget, CommandDeclaration, CommandTree};
use rustclamp_core::{
    ApplicationId, Contribution, ContributionTarget, ExecutionId, ModuleId, ProcessId, Qualifier,
    QualifierId,
};
use rustclamp_kernel::{ApplicationBlueprint, ComposeError, TargetComposition};

const APP: ApplicationId = ApplicationId::new("test.todo");
const CLI: ProcessId = ProcessId::new("test.todo.cli");
const WORKER: ProcessId = ProcessId::new("test.todo.worker");
const CLI_ROOT: ModuleId = ModuleId::new("test.todo.cli-root");
const WORKER_ROOT: ModuleId = ModuleId::new("test.todo.worker-root");
const TODO: ModuleId = ModuleId::new("test.todo.items");
const PURGE: ModuleId = ModuleId::new("test.todo.purge");

struct Commands;

impl Qualifier for Commands {
    const ID: QualifierId = QualifierId::new("test.todo.commands");
}

type Store = Arc<Mutex<Vec<String>>>;
type Target = CliCommandTarget<Commands>;

/// TODO contributes `add` and `list`; PURGE contributes `purge`.
fn contributions(store: &Store) -> Vec<(ModuleId, CommandDeclaration<Commands>)> {
    let (add, list, purge) = (store.clone(), store.clone(), store.clone());
    vec![
        (
            TODO,
            CommandDeclaration::new("add", "Add a todo", move |args| {
                let title = args.get_one::<String>("title").unwrap();
                add.lock().unwrap().push(title.clone());
                0
            })
            .arg(Arg::new("title").required(true)),
        ),
        (
            TODO,
            CommandDeclaration::new("list", "List the todos", move |_| {
                list.lock().unwrap().len() as i32
            }),
        ),
        (
            PURGE,
            CommandDeclaration::new("purge", "Delete every todo", move |_| {
                purge.lock().unwrap().clear();
                0
            }),
        ),
    ]
}

/// The CLI root consumes the command target; the worker root does not.
fn tree(process: ProcessId, store: &Store) -> CommandTree {
    let command = <CommandDeclaration<Commands> as Contribution>::ID;
    let mut app = ApplicationBlueprint::new(APP);
    app.add_root(CLI, ExecutionId::new("test.todo.cli"), CLI_ROOT)
        .add_root(WORKER, ExecutionId::new("test.todo.worker"), WORKER_ROOT)
        .add_module(TODO)
        .add_module(PURGE)
        .consume_target(CLI_ROOT, Target::ID, Commands::ID)
        .add_contribution(TODO, Target::ID, Commands::ID, command)
        .add_contribution(PURGE, Target::ID, Commands::ID, command);
    app.freeze(process)
        .unwrap()
        .compose::<Target, Commands>(&Target::new("todo"), contributions(store))
        .unwrap()
}

#[test]
fn the_process_projection_selects_the_commands() {
    let store = Store::default();
    let cli = tree(CLI, &store);
    assert_eq!(cli.names().collect::<Vec<_>>(), ["add", "list", "purge"]);
    assert_eq!(tree(WORKER, &store).names().count(), 0);

    assert_eq!(cli.run(["todo", "add", "milk"]).unwrap(), 0);
    assert_eq!(cli.run(["todo", "add", "eggs"]).unwrap(), 0);
    assert_eq!(cli.run(["todo", "list"]).unwrap(), 2);
    assert_eq!(cli.run(["todo", "purge"]).unwrap(), 0);
    assert_eq!(cli.run(["todo", "list"]).unwrap(), 0);
    let error = cli.run(["todo", "remove"]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
}

#[test]
fn usage_errors_are_returned_not_printed() {
    let cli = tree(CLI, &Store::default());
    let missing = cli.run(["todo"]).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::MissingSubcommand);
    let no_title = cli.run(["todo", "add"]).unwrap_err();
    assert_eq!(no_title.kind(), ErrorKind::MissingRequiredArgument);
    let help = cli.run(["todo", "--help"]).unwrap_err();
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let help = cli.run(["todo", "help"]).unwrap_err();
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
}

#[test]
fn help_lists_names_and_descriptions_in_order() {
    let help = tree(CLI, &Store::default())
        .command()
        .clone()
        .render_help()
        .to_string();
    let add = help.find("add").unwrap();
    let list = help.find("list").unwrap();
    assert!(add < list, "{help}");
    assert!(help.contains("Add a todo") && help.contains("List the todos"));
}

#[test]
fn duplicate_names_name_both_contributors() {
    let declarations = vec![
        (
            PURGE,
            CommandDeclaration::<Commands>::new("list", "", |_| 0),
        ),
        (TODO, CommandDeclaration::new("list", "", |_| 0)),
    ];
    let error = TargetComposition::<Target, Commands>::new(declarations)
        .build(Some(&Target::new("todo")))
        .unwrap_err();
    let expected = CliBuildError {
        target: Target::ID,
        command: "list".to_owned(),
        first_contributor: TODO.min(PURGE),
        second_contributor: TODO.max(PURGE),
    };
    assert!(error.to_string().contains("duplicate command \"list\""));
    assert_eq!(
        error,
        rustclamp_kernel::TargetCompositionError::Target(expected)
    );
}

#[test]
fn a_module_outside_the_blueprint_edges_is_refused() {
    let mut app = ApplicationBlueprint::new(APP);
    app.add_root(CLI, ExecutionId::new("test.todo.cli"), CLI_ROOT)
        .consume_target(CLI_ROOT, Target::ID, Commands::ID);
    let stray = vec![(CLI_ROOT, CommandDeclaration::new("stray", "", |_| 0))];
    let error = app
        .freeze(CLI)
        .unwrap()
        .compose::<Target, Commands>(&Target::new("todo"), stray)
        .unwrap_err();
    assert!(matches!(error, ComposeError::UndeclaredContribution { .. }));
}

#[test]
fn an_empty_target_builds_an_empty_tree() {
    let tree = Target::new("todo").build(&[]).unwrap();
    assert_eq!(tree.names().count(), 0);
    assert!(tree.run(["todo", "anything"]).is_err());
}
