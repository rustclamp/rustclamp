//! Ownership, return-value and failure behavior of the public entrypoint.

use rustclamp::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn calls_once_on_the_calling_thread_and_accepts_owned_non_send_captures() {
    let calls = Rc::new(Cell::new(0));
    let captured = Rc::clone(&calls);
    let text = String::from("owned result");
    let thread = std::thread::current().id();
    let result = Clamp::run(move || {
        assert_eq!(std::thread::current().id(), thread);
        captured.set(captured.get() + 1);
        text
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(result, "owned result");
}

#[test]
fn borrows_local_state_and_returns_a_borrow() {
    let mut value = 40;
    let result = Clamp::run(|| {
        value += 2;
        &value
    });
    assert_eq!(*result, 42);
}

#[test]
fn returns_errors_without_translation() {
    #[derive(Debug, PartialEq)]
    struct Failure(u32);

    assert_eq!(Clamp::run(|| Err::<(), _>(Failure(7))), Err(Failure(7)));
}

#[test]
fn propagates_panics_and_drops_captured_values_once() {
    struct DropCounter<'a>(&'a Cell<usize>);
    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Cell::new(0);
    let guard = DropCounter(&drops);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Clamp::run(move || {
            let _guard = guard;
            std::panic::panic_any(123_u32);
        });
    }));
    assert_eq!(*panic.unwrap_err().downcast::<u32>().unwrap(), 123);
    assert_eq!(drops.get(), 1);
}
