use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustclamp_core::{ClockCapability, Module, ModuleId};
use rustclamp_example_module::{FixedClockModule, Greeter, GreeterModule, Invocation, Transaction};
use rustclamp_kernel::{CapabilityRequirement, Provision, Resolver};

#[test]
fn application_module_uses_core_and_kernel_without_the_facade() {
    let provider = FixedClockModule(UNIX_EPOCH + Duration::from_secs(42));
    let provisions = [Provision::<ClockCapability>::from_module(&provider)];
    let requirement = CapabilityRequirement::<ClockCapability>::from_module::<GreeterModule>();
    let clock = Resolver::resolve::<ClockCapability>(
        requirement.required_by(),
        &provisions,
        requirement.selected_provider(),
    )
    .unwrap();
    let greeter = Greeter::new(clock);
    let mut invocation = Invocation {
        request_id: 5,
        principal: "Ada".to_owned(),
        transaction: Transaction { writes: Vec::new() },
        tick: 9,
    };

    assert_eq!(
        greeter.greet("world", &mut invocation),
        "Hello, world! (unix second 42, principal Ada, request 5, tick 9)"
    );
    assert_eq!(invocation.transaction.writes, ["world"]);
}

#[test]
fn request_state_is_per_invocation_and_not_a_capability() {
    let provider = FixedClockModule(UNIX_EPOCH + Duration::from_secs(42));
    let provisions = [Provision::<ClockCapability>::from_module(&provider)];
    let requirement = CapabilityRequirement::<ClockCapability>::from_module::<GreeterModule>();
    let clock = Resolver::resolve::<ClockCapability>(
        requirement.required_by(),
        &provisions,
        requirement.selected_provider(),
    )
    .unwrap();
    let greeter = Greeter::new(clock);
    let mut first = Invocation {
        request_id: 1,
        principal: "first".to_owned(),
        transaction: Transaction { writes: Vec::new() },
        tick: 10,
    };
    let mut second = Invocation {
        request_id: 2,
        principal: "second".to_owned(),
        transaction: Transaction { writes: Vec::new() },
        tick: 20,
    };

    greeter.greet("one", &mut first);
    greeter.greet("two", &mut second);

    assert_eq!(first.principal, "first");
    assert_eq!(first.request_id, 1);
    assert_eq!(first.tick, 10);
    assert_eq!(first.transaction.writes, ["one"]);
    assert_eq!(second.principal, "second");
    assert_eq!(second.request_id, 2);
    assert_eq!(second.tick, 20);
    assert_eq!(second.transaction.writes, ["two"]);
}

struct LocalClockModule(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl rustclamp_core::Module for LocalClockModule {
    const ID: ModuleId = ModuleId::new("example.module.local-clock");
}

impl rustclamp_core::Clock for LocalClockModule {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }
}

impl rustclamp_core::Provides<ClockCapability> for LocalClockModule {
    fn provided_value(&self) -> &(dyn rustclamp_core::Clock + 'static) {
        self
    }
}

#[test]
fn local_non_send_clock_can_be_borrowed_without_shared_locking() {
    let clock = LocalClockModule(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(42)));
    let provision = Provision::<ClockCapability>::from_module(&clock);
    let clock =
        Resolver::resolve::<ClockCapability>(GreeterModule::ID, &[provision], None).unwrap();

    assert_eq!(clock.now(), UNIX_EPOCH + Duration::from_secs(42));
}
