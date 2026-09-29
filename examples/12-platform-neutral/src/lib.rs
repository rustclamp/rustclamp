//! Core contracts used alongside ordinary Rust-owned resources, without Kernel.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use rustclamp_core::ModuleId;

/// Existing application state retained in ordinary Rust ownership.
#[derive(Debug, Default)]
pub struct TemperatureHistory(Vec<i32>);

impl TemperatureHistory {
    /// Stores one validated milli-Celsius sample.
    pub fn record(&mut self, value: i32) {
        self.0.push(value);
    }
    /// Returns samples in insertion order.
    pub fn readings(&self) -> &[i32] {
        &self.0
    }
}

/// A framework identity can be added while keeping constructors and state local.
pub struct Thermometer;

impl rustclamp_core::Module for Thermometer {
    const ID: ModuleId = ModuleId::new("example.platform-neutral.thermometer");
}

/// Returns the module's stable identity for optional composition diagnostics.
pub const fn module_id() -> ModuleId {
    <Thermometer as rustclamp_core::Module>::ID
}
