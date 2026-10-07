use std::time::{SystemTime, UNIX_EPOCH};

use rustclamp_core::{Clock, ClockCapability, Module, ModuleId, Provides, Requires};

pub const CLOCK_MODULE: ModuleId = ModuleId::new("example.module.clock");
pub const GREETER_MODULE: ModuleId = ModuleId::new("example.module.greeter");

pub struct FixedClockModule(pub SystemTime);

impl Module for FixedClockModule {
    const ID: ModuleId = CLOCK_MODULE;
}

impl Clock for FixedClockModule {
    fn now(&self) -> SystemTime {
        self.0
    }
}

impl Provides<ClockCapability> for FixedClockModule {
    fn provided_value(&self) -> &(dyn Clock + 'static) {
        self
    }
}

pub struct GreeterModule;

impl Module for GreeterModule {
    const ID: ModuleId = GREETER_MODULE;
}

impl Requires<ClockCapability> for GreeterModule {}

pub struct Transaction {
    pub writes: Vec<String>,
}

pub struct Invocation {
    pub request_id: u64,
    pub principal: String,
    pub transaction: Transaction,
    pub tick: u64,
}

pub struct Greeter<'a> {
    clock: &'a dyn Clock,
}

impl<'a> Greeter<'a> {
    pub fn new(clock: &'a dyn Clock) -> Self {
        Self { clock }
    }

    pub fn greet(&self, name: &str, invocation: &mut Invocation) -> String {
        invocation.transaction.writes.push(name.to_owned());
        let seconds = self
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!(
            "Hello, {name}! (unix second {seconds}, principal {}, request {}, tick {})",
            invocation.principal, invocation.request_id, invocation.tick
        )
    }
}
