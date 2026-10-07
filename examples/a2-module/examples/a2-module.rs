use std::time::UNIX_EPOCH;

use rustclamp_core::ClockCapability;
use rustclamp_example_module::{FixedClockModule, Greeter, GreeterModule, Invocation, Transaction};
use rustclamp_kernel::{CapabilityRequirement, Provision, Resolver};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let clock = FixedClockModule(UNIX_EPOCH + std::time::Duration::from_secs(42));
    let provisions = [Provision::<ClockCapability>::from_module(&clock)];
    let requirement = CapabilityRequirement::<ClockCapability>::from_module::<GreeterModule>();
    let clock = Resolver::resolve::<ClockCapability>(
        requirement.required_by(),
        &provisions,
        requirement.selected_provider(),
    )?;
    let greeter = Greeter::new(clock);
    let mut invocation = Invocation {
        request_id: 7,
        principal: "Ada".to_owned(),
        transaction: Transaction { writes: Vec::new() },
        tick: 12,
    };

    println!("{}", greeter.greet("world", &mut invocation));
    Ok(())
}
