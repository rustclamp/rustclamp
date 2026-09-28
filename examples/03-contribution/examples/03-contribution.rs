use rustclamp_core::{ClockCapability, Module, Provides};
use rustclamp_example_contribution::{
    AdminCommands, AdminStatusModule, CliCommandTarget, ClockModule, CommandDeclaration,
    EpochModule, FixedEpoch, GoodbyeModule, HelloModule, PublicCommands,
};
use rustclamp_kernel::{CapabilityRequirement, Provision, TargetComposition};

fn main() {
    let epoch = EpochModule(FixedEpoch(42));
    let clock = ClockModule::new(epoch.provided_value());
    let target = CliCommandTarget::<PublicCommands>::new();
    let commands = TargetComposition::<_, PublicCommands>::new(vec![
        (HelloModule::ID, HelloModule::contribution()),
        (GoodbyeModule::ID, GoodbyeModule::contribution()),
    ])
    .build(Some(&target))
    .expect("the CLI target accepts these declarations")
    .expect("a target produces a command tree");

    println!(
        "hello: {}",
        commands.execute("hello", Some(&clock)).unwrap()
    );
    println!("goodbye: {}", commands.execute("goodbye", None).unwrap());

    let admin_target = CliCommandTarget::<AdminCommands>::new();
    let admin = TargetComposition::<_, AdminCommands>::new(vec![(
        AdminStatusModule::ID,
        AdminStatusModule::contribution(),
    )])
    .build(Some(&admin_target))
    .expect("the admin target accepts its declaration")
    .expect("a target produces a command tree");
    println!("admin command count: {}", admin.names().count());

    let empty = TargetComposition::<_, AdminCommands>::new(Vec::<(
        rustclamp_core::ModuleId,
        CommandDeclaration<AdminCommands>,
    )>::new())
    .build(Some(&admin_target))
    .expect("an empty command target is valid")
    .expect("a selected target builds an empty tree");
    println!("empty admin command count: {}", empty.names().count());

    let _clock_requirement = CapabilityRequirement::<ClockCapability>::from_module::<HelloModule>();
    let _clock_provision = Provision::<ClockCapability>::from_module(&clock);
}
