//! Deterministic simulation and fake device composition without async or I/O.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustclamp_core::{
    Capability, CapabilityId, Clock, Contribution, ContributionId, ContributionTarget,
    ContributionTargetId, Module, ModuleId, ProcessId, Provides,
};
use rustclamp_runtime::TaskContext;

/// Domain unit for a validated sensor temperature in milli-degrees Celsius.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MilliCelsius(i32);

impl MilliCelsius {
    /// Validates a raw sensor reading at the hydration boundary.
    pub fn from_raw(value: i32) -> Result<Self, SensorError> {
        (-40_000..=125_000)
            .contains(&value)
            .then_some(Self(value))
            .ok_or(SensorError::OutOfRange(value))
    }

    /// Returns the validated milli-degree value.
    pub const fn value(self) -> i32 {
        self.0
    }
}

/// Error reported while reading or validating a sensor value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SensorError {
    /// Raw reading exceeded the supported operating range.
    OutOfRange(i32),
    /// No reading was ever received.
    Missing,
    /// Last reading is older than the selected freshness bound.
    Stale,
    /// Input queue is full and applies backpressure to the producer.
    Overloaded,
    /// The device link is disconnected.
    Disconnected,
    /// The managed device process has shut down.
    Shutdown,
}

/// Errors returned while selecting or applying a CAN frame decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanError {
    /// Frame payload does not match the decoder's expected shape.
    MalformedFrame,
    /// No decoder is registered for the frame identifier.
    UnknownFrame(u16),
    /// The raw CAN value failed sensor-domain validation.
    Sensor(SensorError),
}

/// Sensor capability consumed by a device process.
pub struct SensorCapability;
impl Capability for SensorCapability {
    type Value = dyn Sensor;
    const ID: CapabilityId = CapabilityId::new("example.device.sensor");
}

/// Display capability consumed by a device process.
pub struct DisplayCapability;
impl Capability for DisplayCapability {
    type Value = dyn Display;
    const ID: CapabilityId = CapabilityId::new("example.device.display");
}

/// Hardware link capability consumed by a device process.
pub struct HardwareIoCapability;
impl Capability for HardwareIoCapability {
    type Value = dyn HardwareIo;
    const ID: CapabilityId = CapabilityId::new("example.device.hardware-io");
}

/// Fake or hardware-backed sensor interface.
pub trait Sensor {
    /// Reads one fresh semantic sample.
    fn read(&self, now: Duration, max_age: Duration) -> Result<MilliCelsius, SensorError>;
    /// Releases process-managed input buffers during shutdown.
    fn shutdown(&self) {}
}

/// Fake or hardware-backed display interface.
pub trait Display {
    /// Renders a validated sample.
    fn render(&mut self, value: MilliCelsius);
    /// Returns the displayed value when available.
    fn value(&self) -> Option<MilliCelsius>;
}

/// Replaceable board I/O connectivity interface.
pub trait HardwareIo {
    /// Reports whether the hardware link is connected.
    fn connected(&self) -> bool;
}

/// Last validated reading and its sample time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sample {
    value: MilliCelsius,
    at: Duration,
}

/// Fake monotonic clock controlled by each explicit simulation step.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimClock(Duration);

impl SimClock {
    /// Creates a clock at the simulation origin.
    pub const fn new() -> Self {
        Self(Duration::ZERO)
    }

    /// Advances simulation time by an exact duration.
    pub fn advance(&mut self, by: Duration) {
        self.0 += by;
    }

    /// Returns elapsed simulation time.
    pub const fn elapsed(self) -> Duration {
        self.0
    }
}

impl Clock for SimClock {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + self.0
    }
}

impl Module for SimClock {
    const ID: ModuleId = ModuleId::new("example.device.sim-clock");
}
impl Provides<rustclamp_core::ClockCapability> for SimClock {
    fn provided_value(&self) -> &(dyn Clock + 'static) {
        self
    }
}

/// Small deterministic integer generator with an explicit seed.
#[derive(Clone, Copy, Debug)]
pub struct Seeded(u64);

impl Seeded {
    /// Creates a repeatable random stream.
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns the next pseudorandom word.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

/// Fixed input for one update, applied in target-owned update order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Input {
    /// Requested temperature delta in milli-degrees Celsius.
    pub delta: i32,
}

/// Observable simulation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct State {
    /// Position in integer millimeters.
    pub position_mm: i64,
    /// Velocity in integer millimeters per step.
    pub velocity_mm_per_step: i64,
    /// Number of completed steps.
    pub steps: u64,
    /// Pseudorandom word used by the final step.
    pub noise: u64,
}

/// Explicit-step target; phases and update ordering belong to this loop.
#[derive(Clone, Copy, Debug, Default)]
pub struct Simulation;

impl Simulation {
    /// Advances physics, then controlled input, then the random stream.
    pub fn step(state: &mut State, input: Input, random: &mut Seeded) {
        state.position_mm += state.velocity_mm_per_step;
        state.velocity_mm_per_step += i64::from(input.delta);
        state.noise = random.next_u64();
        state.steps += 1;
    }
}

/// Runtime report for an explicitly driven simulation loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoopReport {
    /// Number of inputs applied before completion or cancellation.
    pub ticks: usize,
    /// Whether the supplied runtime context was cancelled.
    pub cancelled: bool,
}

/// Small synchronous loop driver that borrows caller state and runtime context.
#[derive(Clone, Debug)]
pub struct LoopRuntime {
    clock: SimClock,
    random: Seeded,
    stopped: bool,
}

/// Runtime selected by this application's process configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessRuntime {
    /// Direct synchronous fixed-step driver.
    DeterministicLoop,
    /// Optional asynchronous service adapter.
    AsyncService,
}

/// Selects runtime policy for this example's declared process identities.
pub fn process_runtime(process: ProcessId) -> Option<ProcessRuntime> {
    match process.as_str() {
        "example.device-loop.simulation" => Some(ProcessRuntime::DeterministicLoop),
        "example.device-loop.service" => Some(ProcessRuntime::AsyncService),
        _ => None,
    }
}

impl LoopRuntime {
    /// Creates a loop with controlled time and random seed.
    pub const fn new(seed: u64) -> Self {
        Self {
            clock: SimClock::new(),
            random: Seeded::new(seed),
            stopped: false,
        }
    }

    /// Applies input ticks until exhausted or the runtime context is cancelled.
    pub fn run(
        &mut self,
        state: &mut State,
        inputs: &mut impl Iterator<Item = Input>,
        context: &TaskContext,
    ) -> LoopReport {
        let mut ticks = 0;
        while !self.stopped && !context.is_cancelled() {
            let Some(input) = inputs.next() else { break };
            Simulation::step(state, input, &mut self.random);
            self.clock.advance(Duration::from_millis(16));
            ticks += 1;
        }
        LoopReport {
            ticks,
            cancelled: context.is_cancelled(),
        }
    }

    /// Requests orderly loop shutdown; subsequent runs perform no work.
    pub fn shutdown(&mut self) {
        self.stopped = true;
    }

    /// Returns controlled elapsed time.
    pub const fn clock(&self) -> SimClock {
        self.clock
    }
}

/// Testable stream of validated sensor samples.
#[derive(Debug, Default)]
pub struct FakeSensor(RefCell<VecDeque<Sample>>);

impl FakeSensor {
    /// Queues one raw reading at the supplied simulation time.
    pub fn push(&self, at: Duration, raw: i32) -> Result<(), SensorError> {
        let mut queue = self.0.borrow_mut();
        if queue.len() == 8 {
            return Err(SensorError::Overloaded);
        }
        queue.push_back(Sample {
            value: MilliCelsius::from_raw(raw)?,
            at,
        });
        Ok(())
    }

    /// Reads the oldest queued sample, detecting missing and stale data.
    pub fn read(&self, now: Duration, max_age: Duration) -> Result<MilliCelsius, SensorError> {
        let sample = self
            .0
            .borrow_mut()
            .pop_front()
            .ok_or(SensorError::Missing)?;
        if now.saturating_sub(sample.at) > max_age {
            return Err(SensorError::Stale);
        }
        Ok(sample.value)
    }
}

impl Sensor for FakeSensor {
    fn read(&self, now: Duration, max_age: Duration) -> Result<MilliCelsius, SensorError> {
        FakeSensor::read(self, now, max_age)
    }
    fn shutdown(&self) {
        self.0.borrow_mut().clear();
    }
}
impl Module for FakeSensor {
    const ID: ModuleId = ModuleId::new("example.device.fake-sensor");
}
impl Provides<SensorCapability> for FakeSensor {
    fn provided_value(&self) -> &(dyn Sensor + 'static) {
        self
    }
}

/// Fake display storing its latest rendered value.
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeDisplay(Option<MilliCelsius>);

impl FakeDisplay {
    /// Renders a validated sample.
    pub fn render(&mut self, value: MilliCelsius) {
        self.0 = Some(value);
    }
    /// Returns the displayed value.
    pub const fn value(self) -> Option<MilliCelsius> {
        self.0
    }
}

impl Display for FakeDisplay {
    fn render(&mut self, value: MilliCelsius) {
        FakeDisplay::render(self, value);
    }
    fn value(&self) -> Option<MilliCelsius> {
        self.0
    }
}
impl Module for FakeDisplay {
    const ID: ModuleId = ModuleId::new("example.device.fake-display");
}
impl Provides<DisplayCapability> for FakeDisplay {
    fn provided_value(&self) -> &(dyn Display + 'static) {
        self
    }
}

/// Fake replaceable hardware-I/O link state.
#[derive(Debug)]
pub struct FakeHardwareIo(Cell<bool>);

impl Default for FakeHardwareIo {
    fn default() -> Self {
        Self(Cell::new(true))
    }
}

impl FakeHardwareIo {
    /// Sets link state for disconnect and recovery simulations.
    pub fn set_connected(&self, connected: bool) {
        self.0.set(connected);
    }
}
impl HardwareIo for FakeHardwareIo {
    fn connected(&self) -> bool {
        self.0.get()
    }
}
impl Module for FakeHardwareIo {
    const ID: ModuleId = ModuleId::new("example.device.fake-hardware-io");
}
impl Provides<HardwareIoCapability> for FakeHardwareIo {
    fn provided_value(&self) -> &(dyn HardwareIo + 'static) {
        self
    }
}

/// Device telemetry is optional and externally observable.
pub trait Telemetry {
    /// Emits one temperature sample.
    fn emit(&mut self, value: MilliCelsius);
}

/// CAN frame whose identifier and payload are supplied by the bus adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanFrame {
    /// Arbitration identifier.
    pub id: u16,
    /// Little-endian raw temperature.
    pub payload: Vec<u8>,
}

/// Typed contribution decoded from a CAN frame.
pub trait CanDecoder {
    /// Frame identifier owned by this decoder.
    const FRAME_ID: u16;
    /// Parses a matching frame.
    fn decode(frame: CanFrame) -> Result<MilliCelsius, CanError>;
}

/// Target that validates decoder identity and builds a runtime lookup table.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanTarget;

/// Function pointer retained by an assembled CAN decoder table.
pub type DecoderFn = fn(CanFrame) -> Result<MilliCelsius, CanError>;

/// Sorted runtime table from CAN ID to its typed decoder function.
pub type DecoderTable = Vec<(u16, DecoderFn)>;

impl ContributionTarget for CanTarget {
    type Contribution = CanDecoderDecl;
    type Runtime = DecoderTable;
    type Error = u16;
    const ID: ContributionTargetId = ContributionTargetId::new("example.device.can-decoders");

    fn build(
        &self,
        entries: &[(ModuleId, Self::Contribution)],
    ) -> Result<Self::Runtime, Self::Error> {
        let mut table = Vec::with_capacity(entries.len());
        for (_, declaration) in entries {
            if table.iter().any(|(id, _)| *id == declaration.id) {
                return Err(declaration.id);
            }
            table.push((declaration.id, declaration.decode));
        }
        table.sort_by_key(|(id, _)| *id);
        Ok(table)
    }
}

/// Dispatches one CAN frame through the composed runtime decoder table.
pub fn decode_frame(table: &DecoderTable, frame: CanFrame) -> Result<MilliCelsius, CanError> {
    let decode = table
        .iter()
        .find(|(id, _)| *id == frame.id)
        .map(|(_, decode)| decode)
        .ok_or(CanError::UnknownFrame(frame.id))?;
    if frame.payload.len() != 2 {
        return Err(CanError::MalformedFrame);
    }
    decode(frame)
}

/// Runtime decoder contribution.
#[derive(Clone, Copy)]
pub struct CanDecoderDecl {
    id: u16,
    decode: DecoderFn,
}

impl Contribution for CanDecoderDecl {
    const ID: ContributionId = ContributionId::new("example.device.can-decoder");
}

impl CanDecoderDecl {
    /// Declares a typed decoder function for a frame identifier.
    pub const fn new(id: u16, decode: DecoderFn) -> Self {
        Self { id, decode }
    }
}

/// Builds one table entry from a decoder implementation.
pub fn decoder<D: CanDecoder>() -> CanDecoderDecl {
    CanDecoderDecl::new(D::FRAME_ID, D::decode)
}

/// Runs only bounded work from a device tick to model overload/backpressure.
#[derive(Debug)]
pub struct DeviceProcess<S = FakeSensor, D = FakeDisplay, H = FakeHardwareIo> {
    sensor: S,
    display: D,
    hardware: H,
    shutdown: bool,
}

impl DeviceProcess<FakeSensor, FakeDisplay, FakeHardwareIo> {
    /// Creates a fake device process with caller-owned sensor input.
    pub fn new(sensor: FakeSensor) -> Self {
        Self {
            sensor,
            display: FakeDisplay(None),
            hardware: FakeHardwareIo::default(),
            shutdown: false,
        }
    }
    /// Marks the sensor link state; recovery resumes reads on the next tick.
    pub fn set_disconnected(&mut self, disconnected: bool) {
        self.hardware.set_connected(!disconnected);
    }
}

impl<S: Sensor, D: Display, H: HardwareIo> DeviceProcess<S, D, H> {
    /// Creates a device process from replaceable platform capabilities.
    pub const fn with_capabilities(sensor: S, display: D, hardware: H) -> Self {
        Self {
            sensor,
            display,
            hardware,
            shutdown: false,
        }
    }

    /// Closes the managed process and releases queued sensor samples.
    pub fn shutdown(&mut self) {
        self.shutdown = true;
        self.sensor.shutdown();
    }
    /// Reads, validates, and renders at most one sample per tick.
    pub fn tick(
        &mut self,
        clock: &dyn Clock,
        max_age: Duration,
        mut telemetry: Option<&mut dyn Telemetry>,
    ) -> Result<MilliCelsius, SensorError> {
        if self.shutdown {
            return Err(SensorError::Shutdown);
        }
        if !self.hardware.connected() {
            return Err(SensorError::Disconnected);
        }
        let now = clock.now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let value = self.sensor.read(now, max_age)?;
        self.display.render(value);
        if let Some(sink) = telemetry.as_mut() {
            sink.emit(value);
        }
        Ok(value)
    }
    /// Returns the fake display output.
    pub fn display(&self) -> &D {
        &self.display
    }
}
