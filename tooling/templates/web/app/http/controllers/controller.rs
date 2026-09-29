//! The base every controller starts from, like Laravel's `Controller`. Rust
//! has no inheritance: controllers are plain functions that begin with
//! `use super::controller::*;`. Put helpers every controller shares here.

//!
//! A controller that can fail returns `web::Result` and uses `?`: the error
//! is logged and the visitor gets a `500`.

pub use rustclamp::web::{self, Request, Response, error, json, redirect, render, view};
