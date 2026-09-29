//! A small, synchronous entrypoint for Clamp applications.
//!
//! ```
//! use rustclamp::prelude::*;
//!
//! let answer = Clamp::run(|| 42);
//! assert_eq!(answer, 42);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "web")]
pub mod web;

/// Entrypoints for running a Clamp application.
///
/// The closure entrypoint has no application state or required runtime.
pub struct Clamp;

impl Clamp {
    /// Calls `run` once on the current thread and returns its result unchanged.
    ///
    /// The closure may borrow local data or consume owned captures; it need not
    /// be `Send` or `'static`. Errors are ordinary return values. Panics propagate
    /// according to the application's panic strategy, just as a direct call does.
    ///
    /// This entrypoint adds no allocation, background work, composition, or
    /// lifecycle handling. Returning a future does not poll or execute it.
    #[inline]
    pub fn run<F, R>(run: F) -> R
    where
        F: FnOnce() -> R,
    {
        run()
    }
}

/// The small set of imports needed by a basic Clamp application.
pub mod prelude {
    pub use crate::Clamp;
}
