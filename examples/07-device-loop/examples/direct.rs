//! Equivalent fixed-step application written with ordinary Rust only.

fn main() {
    let mut position = 0_i64;
    let mut velocity = 2_i64;
    for (step, delta) in [1_i64, -1, 3].into_iter().enumerate() {
        position += velocity;
        velocity += delta;
        println!(
            "step={} position={position}mm velocity={velocity}mm/step",
            step + 1
        );
    }
}
