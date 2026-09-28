//! Isolated consumer, dependency, output and allocation regression checks.

#[test]
fn isolated_pico_has_only_the_facade_and_runs_without_entrypoint_allocations() {
    let status = std::process::Command::new("python3")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/pico.py"))
        .arg("--check")
        .status()
        .expect("Pico checks require Python 3.11 or newer");
    assert!(status.success(), "Pico regression check failed");
}
