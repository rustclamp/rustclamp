use std::hint::black_box;
use std::time::Instant;

use rustclamp_core::ModuleId;
use rustclamp_example_contribution::{CliCommandTarget, CommandDeclaration, PublicCommands};
use rustclamp_kernel::TargetComposition;

const REPETITIONS: usize = 100_000;
const SAMPLES: usize = 9;

fn main() {
    let target = CliCommandTarget::<PublicCommands>::new();
    let declarations = [
        (
            ModuleId::new("bench.hello"),
            CommandDeclaration::<PublicCommands>::new("hello", |_| Ok("hi".into())),
        ),
        (
            ModuleId::new("bench.goodbye"),
            CommandDeclaration::<PublicCommands>::new("goodbye", |_| Ok("bye".into())),
        ),
    ];
    let warmup = TargetComposition::<_, PublicCommands>::new(declarations.to_vec())
        .build(Some(&target))
        .unwrap()
        .unwrap();
    let runtime_bytes = warmup.storage_bytes();
    let mut samples = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..REPETITIONS {
                let tree = TargetComposition::<_, PublicCommands>::new(declarations.to_vec())
                    .build(Some(black_box(&target)))
                    .unwrap()
                    .unwrap();
                black_box(tree);
            }
            start.elapsed().as_nanos() / REPETITIONS as u128
        })
        .collect::<Vec<_>>();
    samples.sort_unstable();
    println!(
        "✓ BENCH cli_target_assembly_2: median_ns={} samples_ns={samples:?}",
        samples[SAMPLES / 2]
    );
    println!("  Intent: validate and order two CLI declarations into the runtime command tree.");
    println!("  Runtime representation: {runtime_bytes} bytes for two entries.");
    println!(
        "  Allocations: not instrumented; input, target scratch, and runtime Vec allocate per build."
    );
}
