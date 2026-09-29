//! Everything the app is made of lives in `app/`: code, routes, config,
//! views and data. The project root keeps only what must survive replacing
//! `app/`: `.env`, `storage/` (uploads, logs) and `public/`.
//!
//! This file is the map: every folder and file of Rust code is declared here,
//! so there are no `mod.rs` files. Add `http/controllers`, `models`, `config`
//! and the like here as the app needs them.

use rustclamp::web::{Router, security_headers};

/// Route definitions.
pub mod routes {
    pub mod api;
    pub mod web;
}

/// Every route. A GET that matches none serves the file from `public/`.
pub fn routes() -> Router {
    let router = Router::new().middleware(security_headers);
    routes::api::routes(routes::web::routes(router))
}
