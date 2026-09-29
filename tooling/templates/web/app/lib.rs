//! Everything the app is made of lives in `app/`: code, routes, config,
//! views and data. The project root keeps only what must survive replacing
//! `app/`: `.env`, `storage/` (uploads, logs) and `public/`.
//!
//! This file is the map: every folder and file of Rust code is declared here,
//! so there are no `mod.rs` files.

use rustclamp::config::Config;
use rustclamp::db::Db;
use rustclamp::web::{Router, security_headers};

/// Everything that handles HTTP.
pub mod http {
    /// Turn requests into responses.
    pub mod controllers {
        pub mod controller;
        pub mod health;
    }
    /// Wrap requests: `Fn(&Request, Next) -> Response`.
    pub mod middleware {
        mod request_log;
        pub use request_log::request_log;
    }
    // Form input and its validation goes in `requests/`, e.g.
    // `pub mod requests { mod contact; pub use contact::ContactRequest; }`
}

// Data the app works with goes in `models/`, e.g.
// `pub mod models { mod post; pub use post::Post; }`

/// The database: every file in `database/migrations/` (the schema, oldest
/// first), `database/seeders/` (data for `cargo run -- db:seed`) and
/// `database/states/` (allowed status transitions, as `database::states::*`).
/// Adding a file is enough; `build.rs` lists them.
pub mod database {
    include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
    include!(concat!(env!("OUT_DIR"), "/seeders.rs"));
    include!(concat!(env!("OUT_DIR"), "/states.rs"));
}

/// Typed settings from `.env`.
pub mod config {
    mod app;
    pub use app::Settings;
}

/// Route definitions.
pub mod routes {
    pub mod api;
    pub mod web;
}

/// Every route. A GET that matches none serves the file from `public/`.
/// Opens the database from `.env` and runs pending migrations first.
pub fn routes(config: &Config) -> Router {
    let settings = config::Settings::from(config);
    let db = Db::open(config);
    db.migrate(&database::migrations())
        .unwrap_or_else(|error| panic!("migration failed: {error}"));
    // Handlers reach the database with `request.db()`.
    let router = Router::new()
        .state(db)
        .middleware(security_headers)
        .middleware(http::middleware::request_log);
    // Packages (`clamp init --package`) add their routes here, e.g.
    // `let router = router.package(blog::Blog::from(config));`
    routes::api::routes(routes::web::routes(router), &settings)
}
