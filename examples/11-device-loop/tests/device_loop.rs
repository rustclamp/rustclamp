//! Deterministic loop, fake-device, decoder, and process projection checks.

use std::time::Duration;

use rustclamp_core::{
    ApplicationId, Contribution, ContributionId, ContributionTarget, ExecutionId, ModuleId,
    ProcessId,
};
use rustclamp_example_device_loop::{
    CanDecoder, CanFrame, CanTarget, DeviceProcess, FakeSensor, Input, MilliCelsius, Seeded,
    SensorError, SimClock, Simulation, State, decoder,
};
use rustclamp_kernel::ApplicationBlueprint;
use rustclamp_runtime::{
    FailurePolicy, ManualRuntime, TaskContext, TaskDefinition, TaskExit, TaskKind, TaskRuntime,
};

fn replay() -> Vec<State> {
    let mut state = State {
        position_mm: 0,
        velocity_mm_per_step: 2,
        steps: 0,
        noise: 0,
    };
    let mut random = Seeded::new(42);
    let inputs = [Input { delta: 1 }, Input { delta: -1 }, Input { delta: 3 }];
    let mut output = Vec::new();
    for input in inputs {
        Simulation::step(&mut state, input, &mut random);
        output.push(state);
    }
    output
}

#[test]
fn replay_is_repeatable_and_seed_controls_randomness() {
    assert_eq!(replay(), replay());
    let mut left = Seeded::new(1);
    let mut right = Seeded::new(2);
    assert_ne!(left.next_u64(), right.next_u64());
}

#[test]
fn time_and_raw_sensor_values_are_controlled_and_validated() {
    let mut clock = SimClock::new();
    clock.advance(Duration::from_secs(2));
    assert_eq!(clock.elapsed(), Duration::from_secs(2));
    let mut sensor = FakeSensor::default();
    assert_eq!(
        sensor.read(clock.elapsed(), Duration::from_secs(1)),
        Err(SensorError::Missing)
    );
    assert_eq!(
        sensor.push(clock.elapsed(), 130_000),
        Err(SensorError::OutOfRange(130_000))
    );
    sensor.push(clock.elapsed(), 20_000).unwrap();
    assert_eq!(
        sensor.read(
            clock.elapsed() + Duration::from_secs(2),
            Duration::from_secs(1)
        ),
        Err(SensorError::Stale)
    );
    sensor.push(clock.elapsed(), 20_000).unwrap();
    assert_eq!(
        sensor
            .read(clock.elapsed(), Duration::from_secs(1))
            .unwrap()
            .value(),
        20_000
    );
}

#[test]
fn fake_device_handles_disconnect_recovery_and_optional_telemetry() {
    #[derive(Default)]
    struct Sink(Vec<i32>);
    impl rustclamp_example_device_loop::Telemetry for Sink {
        fn emit(&mut self, value: MilliCelsius) {
            self.0.push(value.value());
        }
    }
    let mut sensor = FakeSensor::default();
    sensor.push(Duration::ZERO, 18_500).unwrap();
    let mut device = DeviceProcess::new(sensor);
    device.set_disconnected(true);
    assert_eq!(
        device.tick(Duration::ZERO, Duration::from_secs(1), None),
        Err(SensorError::Disconnected)
    );
    device.set_disconnected(false);
    let mut sink = Sink::default();
    assert_eq!(
        device
            .tick(Duration::ZERO, Duration::from_secs(1), Some(&mut sink))
            .unwrap()
            .value(),
        18_500
    );
    assert_eq!(device.display().value().unwrap().value(), 18_500);
    assert_eq!(sink.0, [18_500]);
}

#[test]
fn sensor_queue_applies_backpressure_and_consumes_one_sample_per_tick() {
    let mut sensor = FakeSensor::default();
    for _ in 0..8 {
        sensor.push(Duration::ZERO, 20_000).unwrap();
    }
    assert_eq!(
        sensor.push(Duration::ZERO, 20_000),
        Err(SensorError::Overloaded)
    );
    assert_eq!(
        sensor
            .read(Duration::ZERO, Duration::from_secs(1))
            .unwrap()
            .value(),
        20_000
    );
    assert_eq!(sensor.push(Duration::ZERO, 20_000), Ok(()));
}

struct Temperature;
impl CanDecoder for Temperature {
    const FRAME_ID: u16 = 0x123;
    fn decode(frame: CanFrame) -> Result<MilliCelsius, SensorError> {
        MilliCelsius::from_raw(i32::from(u16::from_le_bytes(frame.payload)))
    }
}
impl Contribution for Temperature {
    const ID: ContributionId = ContributionId::new("test.temperature-decoder");
}

#[test]
fn can_target_sorts_decoders_and_rejects_duplicate_frames() {
    let one = ModuleId::new("one");
    let two = ModuleId::new("two");
    let table = CanTarget.build(&[(one, decoder::<Temperature>())]).unwrap();
    assert_eq!(table[0].0, 0x123);
    assert_eq!(
        (table[0].1)(CanFrame {
            id: 0x123,
            payload: 200u16.to_le_bytes()
        })
        .unwrap()
        .value(),
        200
    );
    assert_eq!(
        CanTarget
            .build(&[
                (one, decoder::<Temperature>()),
                (two, decoder::<Temperature>())
            ])
            .unwrap_err(),
        0x123
    );
}

#[test]
fn deterministic_and_service_processes_have_separate_runtime_choices() {
    const APP: ApplicationId = ApplicationId::new("device-and-service");
    const LOOP: ProcessId = ProcessId::new("simulation-loop");
    const SERVICE: ProcessId = ProcessId::new("async-service");
    let mut blueprint = ApplicationBlueprint::new(APP);
    blueprint.add_module(ModuleId::new("loop-root"));
    blueprint.add_module(ModuleId::new("service-root"));
    for module in [ModuleId::new("loop-root"), ModuleId::new("service-root")] {
        blueprint.add_module(module);
    }
    blueprint.add_execution(ExecutionId::new("loop-exec"), ModuleId::new("loop-root"));
    blueprint.add_execution(
        ExecutionId::new("service-exec"),
        ModuleId::new("service-root"),
    );
    blueprint.add_process(LOOP, vec![ExecutionId::new("loop-exec")]);
    blueprint.add_process(SERVICE, vec![ExecutionId::new("service-exec")]);
    let frozen = blueprint.freeze_processes(&[LOOP, SERVICE]).unwrap();
    assert_eq!(frozen[0].runtime().modules(), &[ModuleId::new("loop-root")]);
    assert_eq!(
        frozen[1].runtime().modules(),
        &[ModuleId::new("service-root")]
    );

    let definition = TaskDefinition::new(
        SERVICE,
        "service",
        TaskKind::Service,
        true,
        None,
        FailurePolicy::Shutdown,
        |_context: TaskContext| Ok(()),
    );
    let runtime = ManualRuntime;
    let context = TaskContext::new();
    let handle = runtime.spawn(&definition, context);
    runtime.cancel(&handle);
    assert_eq!(
        runtime.join(handle, Duration::from_secs(1)),
        TaskExit::Cancelled
    );
}

#[cfg(feature = "tokio-service")]
#[test]
fn optional_async_service_runtime_cancels_without_changing_the_loop_path() {
    use rustclamp_runtime::tokio_runtime::TokioRuntime;

    let runtime = TokioRuntime::managed().unwrap();
    let task = runtime.spawn_async(TaskContext::new(), |_| async {
        std::future::pending::<Result<(), String>>().await
    });
    task.cancel();
    assert_eq!(
        runtime.block_on(task.wait(Some(Duration::from_secs(1)))),
        TaskExit::Cancelled
    );
}
