use rustclamp_core::{
    Clock, ClockCapability, Contribution, ContributionId, ContributionTarget, ContributionTargetId,
    Module, ModuleId, Provides, Qualifier, QualifierId,
};
use rustclamp_example_contribution::{
    AdminCommands, AdminStatusModule, CliCommandTarget, CommandDeclaration, CommandRunError,
    EpochModule, EpochSourceCapability, FixedEpoch, GOODBYE_MODULE, GoodbyeModule, HELLO_MODULE,
    HelloModule, PublicCommands,
};
use rustclamp_kernel::{
    CapabilityComposition, CapabilityRequirement, ConstructionDependency, ConstructionGraph,
    Provision, TargetComposition, TargetCompositionError,
};
use std::cell::Cell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn assembled_tree_executes_both_commands_with_declared_clock_dependency() {
    let factory_calls = Cell::new(0);
    let make_clock = || {
        factory_calls.set(factory_calls.get() + 1);
        TestClock(Cell::new(0))
    };
    let target = CliCommandTarget::<PublicCommands>::new();
    let tree = TargetComposition::<_, PublicCommands>::new(vec![
        (HELLO_MODULE, HelloModule::contribution()),
        (GOODBYE_MODULE, GoodbyeModule::contribution()),
    ])
    .build(Some(&target))
    .unwrap()
    .unwrap();

    assert_eq!(
        factory_calls.get(),
        0,
        "assembly must not initialize resources"
    );
    assert_eq!(tree.names().collect::<Vec<_>>(), ["goodbye", "hello"]);
    let clock = make_clock();
    assert_eq!(factory_calls.get(), 1);
    assert_eq!(
        tree.execute("hello", Some(&clock)),
        Ok("hello at 42".to_owned())
    );
    assert_eq!(tree.execute("goodbye", None), Ok("goodbye".to_owned()));
    assert_eq!(clock.0.get(), 1);
    assert_eq!(
        tree.execute("missing", None),
        Err(CommandRunError::UnknownCommand("missing".to_owned()))
    );
    assert_eq!(
        tree.execute("hello", None),
        Err(CommandRunError::MissingClock)
    );
}

#[test]
fn duplicate_command_is_a_target_owned_structured_error() {
    let target = CliCommandTarget::<PublicCommands>::new();
    let error = TargetComposition::<_, PublicCommands>::new(vec![
        (HELLO_MODULE, HelloModule::contribution()),
        (
            GOODBYE_MODULE,
            CommandDeclaration::<PublicCommands>::new("hello", |_| Ok("other".to_owned())),
        ),
    ])
    .build(Some(&target))
    .unwrap_err();

    let TargetCompositionError::Target(error) = error else {
        panic!("duplicate command must be rejected by the target");
    };
    assert_eq!(error.target, CliCommandTarget::<PublicCommands>::ID);
    assert_eq!(error.command, "hello");
    assert_eq!(error.first_contributor, GOODBYE_MODULE);
    assert_eq!(error.second_contributor, HELLO_MODULE);
}

#[test]
fn unconsumed_required_contributions_identify_the_typed_target() {
    let error = TargetComposition::<CliCommandTarget<PublicCommands>, PublicCommands>::new(vec![
        (HELLO_MODULE, HelloModule::contribution()),
        (GOODBYE_MODULE, GoodbyeModule::contribution()),
    ])
    .build(None)
    .unwrap_err();

    assert_eq!(
        error,
        TargetCompositionError::UnconsumedRequired {
            target: CliCommandTarget::<PublicCommands>::ID,
            contribution: CommandDeclaration::<PublicCommands>::ID,
            qualifier: PublicCommands::ID,
            contributors: vec![GOODBYE_MODULE, HELLO_MODULE],
        }
    );
}

#[test]
fn empty_and_qualified_targets_are_isolated() {
    let target = CliCommandTarget::<PublicCommands>::new();
    let public = TargetComposition::<_, PublicCommands>::new(vec![(
        HELLO_MODULE,
        HelloModule::contribution(),
    )])
    .build(Some(&target))
    .unwrap()
    .unwrap();
    let admin_target = CliCommandTarget::<AdminCommands>::new();
    let admin = TargetComposition::<_, AdminCommands>::new(vec![(
        AdminStatusModule::ID,
        AdminStatusModule::contribution(),
    )])
    .build(Some(&admin_target))
    .unwrap()
    .unwrap();

    assert_eq!(public.names().collect::<Vec<_>>(), ["hello"]);
    assert_eq!(admin.names().collect::<Vec<_>>(), ["status"]);
    let empty =
        TargetComposition::<CliCommandTarget<AdminCommands>, AdminCommands>::new(Vec::new())
            .build(Some(&admin_target))
            .unwrap()
            .unwrap();
    assert_eq!(empty.names().count(), 0);
    assert!(
        TargetComposition::<CliCommandTarget<AdminCommands>, AdminCommands>::new(Vec::new())
            .build(None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn contribution_requirement_reaches_the_clock_dependency_chain() {
    let epoch = EpochModule(FixedEpoch(42));
    let clock = rustclamp_example_contribution::ClockModule::new(epoch.provided_value());
    let clock_graph = CapabilityComposition::new(
        vec![Provision::<ClockCapability>::from_module(&clock)],
        vec![CapabilityRequirement::<ClockCapability>::from_module::<
            HelloModule,
        >()],
    );
    let epoch_graph = CapabilityComposition::new(
        vec![Provision::<EpochSourceCapability>::from_module(&epoch)],
        vec![
            CapabilityRequirement::<EpochSourceCapability>::from_module::<
                rustclamp_example_contribution::ClockModule,
            >(),
        ],
    );

    clock_graph.validate().unwrap();
    epoch_graph.validate().unwrap();
    assert_eq!(
        ConstructionGraph::new(vec![
            ConstructionDependency::new(HELLO_MODULE, rustclamp_example_contribution::CLOCK_MODULE),
            ConstructionDependency::new(
                rustclamp_example_contribution::CLOCK_MODULE,
                rustclamp_example_contribution::EPOCH_MODULE,
            ),
        ])
        .validate(),
        Ok(())
    );
}

#[test]
fn third_party_target_uses_the_public_extension_contract() {
    struct AuditTarget;
    struct AuditContribution(u32);
    struct AuditQualifier;
    struct ThirdPartyModule;

    impl Qualifier for AuditQualifier {
        const ID: QualifierId = QualifierId::new("test.audit");
    }

    impl Contribution for AuditContribution {
        const ID: ContributionId = ContributionId::new("test.audit.item");
    }

    impl ContributionTarget for AuditTarget {
        type Contribution = AuditContribution;
        type Runtime = u32;
        type Error = ();
        const ID: ContributionTargetId = ContributionTargetId::new("test.audit.target");

        fn build(&self, contributions: &[(ModuleId, Self::Contribution)]) -> Result<u32, ()> {
            Ok(contributions.iter().map(|(_, item)| item.0).sum())
        }
    }

    impl Module for ThirdPartyModule {
        const ID: ModuleId = ModuleId::new("test.third-party");
    }

    let runtime = TargetComposition::<AuditTarget, AuditQualifier>::new(vec![(
        ThirdPartyModule::ID,
        AuditContribution(5),
    )])
    .build(Some(&AuditTarget))
    .unwrap()
    .unwrap();

    assert_eq!(runtime, 5);
}

struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        let reads = self.0.get() + 1;
        self.0.set(reads);
        UNIX_EPOCH + Duration::from_secs(42)
    }
}
