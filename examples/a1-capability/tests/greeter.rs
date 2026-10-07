use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustclamp_core::Clock;
use rustclamp_example_capability::Greeter;

struct FixedClock(SystemTime);

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

#[test]
fn greeter_accepts_an_application_owned_clock_without_the_facade() {
    let greeter = Greeter::new(FixedClock(UNIX_EPOCH + Duration::from_secs(42)));

    assert_eq!(greeter.greet("Ada"), "Hello, Ada! (unix second 42)");
}
