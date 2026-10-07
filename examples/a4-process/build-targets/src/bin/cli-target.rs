use rustclamp_core::ProcessId;

const CLI: ProcessId = ProcessId::new("build-comparison.cli");

fn main() {
    println!("{} includes cli-root", CLI.as_str());
}
