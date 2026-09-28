use std::time::UNIX_EPOCH;

use rustclamp_core::Clock;

pub struct Greeter<C> {
    clock: C,
}

impl<C: Clock> Greeter<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn greet(&self, name: &str) -> String {
        let seconds = self
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("Hello, {name}! (unix second {seconds})")
    }
}
