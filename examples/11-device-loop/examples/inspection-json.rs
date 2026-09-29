//! Exports the device capability projection for `clamp` inspection.

use rustclamp_core::{
    ApplicationId, Capability, ClockCapability, ExecutionId, ModuleId, ProcessId,
};
use rustclamp_example_device_loop::{DisplayCapability, HardwareIoCapability, SensorCapability};
use rustclamp_kernel::ApplicationBlueprint;
use rustclamp_tooling::inspection_document;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let application = ApplicationId::new("example.device-loop");
    let process = ProcessId::new("example.device-loop.simulation");
    let execution = ExecutionId::new("example.device-loop.root");
    let root = ModuleId::new("example.device-loop.root");
    let providers = [
        ModuleId::new("example.device-loop.clock"),
        ModuleId::new("example.device-loop.sensor"),
        ModuleId::new("example.device-loop.display"),
        ModuleId::new("example.device-loop.hardware-io"),
    ];
    let mut blueprint = ApplicationBlueprint::new(application);
    blueprint.add_module(root);
    for provider in providers {
        blueprint.add_module(provider);
    }
    blueprint
        .add_execution(execution, root)
        .add_process(process, vec![execution]);
    for (capability, provider) in [
        (ClockCapability::ID, providers[0]),
        (SensorCapability::ID, providers[1]),
        (DisplayCapability::ID, providers[2]),
        (HardwareIoCapability::ID, providers[3]),
    ] {
        blueprint.require_provider(root, capability, None, provider);
    }
    let projection = blueprint
        .project(process)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let document = inspection_document(&[&projection]).map_err(std::io::Error::other)?;
    println!("{document}");
    Ok(())
}
