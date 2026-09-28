//! Runs dependency checks as a Cargo integration test, including invalid fixtures.

use std::path::Path;
use std::process::Command;

#[test]
fn dependency_boundaries_and_adversarial_fixtures() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("python3")
        .arg(package.join("tools/boundaries.py"))
        .arg("--manifest")
        .arg(package.join("Cargo.toml"))
        .arg("--self-test")
        .status()
        .expect("architecture checks require Python 3.11 or newer");
    assert!(status.success(), "dependency boundary checks failed");
}
