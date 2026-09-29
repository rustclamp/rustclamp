//! Deterministic simulation and fake device composition without async or I/O.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use rustclamp_core::{
    Clock, Contribution, ContributionId, ContributionTarget, ContributionTargetId, ModuleId,
};

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

/// Testable stream of validated sensor samples.
#[derive(Clone, Debug, Default)]
pub struct FakeSensor(VecDeque<Sample>);

impl FakeSensor {
    /// Queues one raw reading at the supplied simulation time.
    pub fn push(&mut self, at: Duration, raw: i32) -> Result<(), SensorError> {
        if self.0.len() == 8 {
            return Err(SensorError::Overloaded);
        }
        self.0.push_back(Sample {
            value: MilliCelsius::from_raw(raw)?,
            at,
        });
        Ok(())
    }

    /// Reads the latest sample, detecting missing and stale data.
    pub fn read(&mut self, now: Duration, max_age: Duration) -> Result<MilliCelsius, SensorError> {
        let sample = self.0.pop_front().ok_or(SensorError::Missing)?;
        if now.saturating_sub(sample.at) > max_age {
            return Err(SensorError::Stale);
        }
        Ok(sample.value)
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

/// Device telemetry is optional and externally observable.
pub trait Telemetry {
    /// Emits one temperature sample.
    fn emit(&mut self, value: MilliCelsius);
}

/// CAN frame whose identifier and payload are supplied by the bus adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanFrame {
    /// Arbitration identifier.
    pub id: u16,
    /// Little-endian raw temperature.
    pub payload: [u8; 2],
}

/// Typed contribution decoded from a CAN frame.
pub trait CanDecoder {
    /// Frame identifier owned by this decoder.
    const FRAME_ID: u16;
    /// Parses a matching frame.
    fn decode(frame: CanFrame) -> Result<MilliCelsius, SensorError>;
}

/// Target that validates decoder identity and builds a runtime lookup table.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanTarget;

impl ContributionTarget for CanTarget {
    type Contribution = CanDecoderDecl;
    type Runtime = Vec<(u16, fn(CanFrame) -> Result<MilliCelsius, SensorError>)>;
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

/// Runtime decoder contribution.
#[derive(Clone, Copy)]
pub struct CanDecoderDecl {
    id: u16,
    decode: fn(CanFrame) -> Result<MilliCelsius, SensorError>,
}

impl Contribution for CanDecoderDecl {
    const ID: ContributionId = ContributionId::new("example.device.can-decoder");
}

impl CanDecoderDecl {
    /// Declares a typed decoder function for a frame identifier.
    pub const fn new(id: u16, decode: fn(CanFrame) -> Result<MilliCelsius, SensorError>) -> Self {
        Self { id, decode }
    }
}

/// Builds one table entry from a decoder implementation.
pub fn decoder<D: CanDecoder>() -> CanDecoderDecl {
    CanDecoderDecl::new(D::FRAME_ID, D::decode)
}

/// Runs only bounded work from a device tick to model overload/backpressure.
#[derive(Clone, Debug)]
pub struct DeviceProcess {
    sensor: FakeSensor,
    display: FakeDisplay,
    disconnected: bool,
}

impl DeviceProcess {
    /// Creates a fake device process with caller-owned sensor input.
    pub const fn new(sensor: FakeSensor) -> Self {
        Self {
            sensor,
            display: FakeDisplay(None),
            disconnected: false,
        }
    }
    /// Marks the sensor link state; recovery resumes reads on the next tick.
    pub fn set_disconnected(&mut self, disconnected: bool) {
        self.disconnected = disconnected;
    }
    /// Reads, validates, and renders at most one sample per tick.
    pub fn tick(
        &mut self,
        now: Duration,
        max_age: Duration,
        mut telemetry: Option<&mut dyn Telemetry>,
    ) -> Result<MilliCelsius, SensorError> {
        if self.disconnected {
            return Err(SensorError::Disconnected);
        }
        let value = self.sensor.read(now, max_age)?;
        self.display.render(value);
        if let Some(sink) = telemetry.as_mut() {
            sink.emit(value);
        }
        Ok(value)
    }
    /// Returns the fake display output.
    pub const fn display(&self) -> FakeDisplay {
        self.display
    }
}
