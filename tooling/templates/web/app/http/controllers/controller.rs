//! The base every controller starts from, like Laravel's `Controller`. Rust
//! has no inheritance: controllers are plain functions that begin with
//! `use super::controller::*;`. Put helpers every controller shares here.

pub use rustclamp::web::{Request, Response, error, escape, json, redirect, render, view};
