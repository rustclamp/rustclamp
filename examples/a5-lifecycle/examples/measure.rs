//! Instrumented runtime microbenchmark. Unsafe allocation tracking is local to this harness.

#![deny(unsafe_op_in_unsafe_fn)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustclamp_core::ProcessId;
use rustclamp_runtime::{
    FailurePolicy, ManualRuntime, Supervisor, TaskContext, TaskDefinition, TaskKind,
};

struct CountingAllocator;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: Each request is forwarded unchanged to System; relaxed counters do not
// modify or retain the pointers and layouts owned by the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: The caller supplies a valid layout, passed unchanged to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: This pointer was allocated by System with the same layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: The caller supplies a valid layout, passed unchanged to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
        // SAFETY: The caller guarantees pointer/layout validity and a valid size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const PROCESS: ProcessId = ProcessId::new("lifecycle.measurement");
const TASKS: usize = 500;
const SAMPLES: usize = 9;
static DIRECT_COMPLETED: AtomicUsize = AtomicUsize::new(0);
static MANUAL_COMPLETED: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "tokio-runtime")]
static TOKIO_COMPLETED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Sample {
    nanos_per_task: u128,
    allocations_per_task: f64,
    bytes_per_task: f64,
}

fn task_definition(completed: &'static AtomicUsize) -> TaskDefinition {
    TaskDefinition::new(
        PROCESS,
        "measurement-task",
        TaskKind::Finite,
        true,
        Some(Duration::from_secs(30)),
        FailurePolicy::Shutdown,
        move |_| {
            completed.fetch_add(1, Ordering::Relaxed);
            black_box(());
            Ok(())
        },
    )
}

fn measure(mut run: impl FnMut()) -> Vec<Sample> {
    (0..SAMPLES)
        .map(|_| {
            let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
            let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
            let start = Instant::now();
            for _ in 0..TASKS {
                run();
            }
            let nanos_per_task = start.elapsed().as_nanos() / TASKS as u128;
            Sample {
                nanos_per_task,
                allocations_per_task: (ALLOCATIONS.load(Ordering::Relaxed) - allocations_before)
                    as f64
                    / TASKS as f64,
                bytes_per_task: (ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before) as f64
                    / TASKS as f64,
            }
        })
        .collect()
}

fn median(values: impl Iterator<Item = u128>) -> u128 {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    values[values.len() / 2]
}

fn lifecycle_wall_samples() -> (Vec<u128>, Vec<u128>, u64, u64) {
    let mut ready = Vec::with_capacity(SAMPLES);
    let mut shutdown = Vec::with_capacity(SAMPLES);
    let mut ready_ms = 0;
    let mut shutdown_ms = 0;
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let outcome = rustclamp_example_lifecycle::start(Default::default());
        ready.push(start.elapsed().as_nanos());
        ready_ms = outcome.elapsed_ms();
        let start = Instant::now();
        let outcome = rustclamp_example_lifecycle::shutdown(
            outcome,
            rustclamp_example_lifecycle::ShutdownReason::Requested,
        );
        shutdown.push(start.elapsed().as_nanos());
        shutdown_ms = outcome.elapsed_ms();
    }
    (ready, shutdown, ready_ms, shutdown_ms)
}

#[cfg(feature = "tokio-runtime")]
fn thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task")
        .ok()
        .map(Iterator::count)
}

fn report(name: &str, samples: &[Sample], task_count: usize) -> String {
    let nanos = median(samples.iter().map(|sample| sample.nanos_per_task));
    let timings = samples
        .iter()
        .map(|sample| sample.nanos_per_task.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let allocations: f64 = samples
        .iter()
        .map(|sample| sample.allocations_per_task)
        .sum::<f64>()
        / samples.len() as f64;
    let bytes: f64 = samples
        .iter()
        .map(|sample| sample.bytes_per_task)
        .sum::<f64>()
        / samples.len() as f64;
    format!(
        "\"{name}\":{{\"tasks\":{task_count},\"sample_ns_per_task\":[{timings}],\"median_ns_per_task\":{nanos},\"mean_allocations_per_task\":{allocations:.3},\"mean_allocated_bytes_per_task\":{bytes:.1}}}"
    )
}

fn main() {
    let direct = measure(|| {
        DIRECT_COMPLETED.fetch_add(1, Ordering::Relaxed);
        black_box(());
    });

    let manual_definition = task_definition(&MANUAL_COMPLETED);
    let manual_supervisor = Supervisor::new(ManualRuntime);
    let manual = measure(|| {
        black_box(manual_supervisor.supervise(&manual_definition, &TaskContext::new()));
    });

    let (ready_wall, shutdown_wall, ready_ms, shutdown_ms) = lifecycle_wall_samples();

    #[cfg(feature = "tokio-runtime")]
    let (tokio_samples, runtime_start_ns, threads_before, threads_during, threads_after) = {
        use rustclamp_runtime::tokio_runtime::TokioRuntime;

        let threads_before = thread_count();
        let start = Instant::now();
        let runtime = TokioRuntime::managed().expect("create managed Tokio runtime");
        let runtime_start_ns = start.elapsed().as_nanos();
        let threads_during = thread_count();
        let definition = task_definition(&TOKIO_COMPLETED);
        let supervisor = Supervisor::new(runtime);
        let samples = measure(|| {
            black_box(supervisor.supervise(&definition, &TaskContext::new()));
        });
        drop(supervisor);
        let threads_after = thread_count();
        (
            samples,
            runtime_start_ns,
            threads_before,
            threads_during,
            threads_after,
        )
    };
    #[cfg(not(feature = "tokio-runtime"))]
    let (runtime_start_ns, threads_before, threads_during, threads_after) =
        { (0u128, None::<usize>, None::<usize>, None::<usize>) };

    let direct_report = report("direct", &direct, DIRECT_COMPLETED.load(Ordering::Relaxed));
    let manual_report = report("manual", &manual, MANUAL_COMPLETED.load(Ordering::Relaxed));
    #[cfg(feature = "tokio-runtime")]
    let tokio_report = report(
        "tokio",
        &tokio_samples,
        TOKIO_COMPLETED.load(Ordering::Relaxed),
    );
    #[cfg(not(feature = "tokio-runtime"))]
    let tokio_report = "\"tokio\":null";
    let ready_wall_samples = ready_wall
        .iter()
        .map(u128::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let shutdown_wall_samples = shutdown_wall
        .iter()
        .map(u128::to_string)
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "{{{direct_report},{manual_report},{tokio_report},\"lifecycle\":{{\"fake_time_to_ready_ms\":{ready_ms},\"fake_shutdown_total_ms\":{shutdown_ms},\"wall_time_to_ready_ns_samples\":[{ready_wall_samples}],\"wall_shutdown_ns_samples\":[{shutdown_wall_samples}]}},\"tokio_runtime_start_ns\":{runtime_start_ns},\"threads\":{{\"before\":{},\"during\":{},\"after\":{}}},\"samples\":{SAMPLES},\"tasks_per_sample\":{TASKS}}}",
        threads_before.map_or("null".to_owned(), |n| n.to_string()),
        threads_during.map_or("null".to_owned(), |n| n.to_string()),
        threads_after.map_or("null".to_owned(), |n| n.to_string()),
    );
}
