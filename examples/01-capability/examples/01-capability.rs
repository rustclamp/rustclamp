use std::time::SystemTime;

use rustclamp_core::Clock;
use rustclamp_example_capability::Greeter;

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

fn main() {
    println!("{}", Greeter::new(SystemClock).greet("world"));
}
