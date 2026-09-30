//! Everything the app is made of lives in `app/`: code, routes, config,
//! views and data. The project root keeps only what must survive replacing
//! `app/`: `.env`, `storage/` (uploads, logs) and `public/`.
//!
//! This file is the map: every folder and file of Rust code is declared here,
//! so there are no `mod.rs` files.

use rustclamp::web::App;

/// Everything that handles HTTP.
pub mod http {
    /// Turn requests into responses.
    pub mod controllers {
        pub mod controller;
    }
    // Middleware goes in `middleware/`: `Fn(&Request, Next) -> Response`, e.g.
    // `pub mod middleware { mod admin; pub use admin::admin; }`
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
    include!(concat!(env!("OUT_DIR"), "/database.rs"));
}

/// Typed settings from `.env`, one file per area, like Laravel's `config/`.
pub mod config {
    mod app;
    mod database;
    mod filesystems;
    mod logging;
    pub use app::Settings;
    pub use database::database;
    pub use filesystems::filesystems;
    pub use logging::logging;
}

/// Route definitions.
pub mod routes {
    pub mod api;
    pub mod web;
}

/// The app: its config, database and routes. `main.rs` runs it; tests build
/// it with `app().test("")`. The framework opens and migrates the database
/// and shares it (`request.db()`) and the disks (`request.storage()`) with
/// handlers, behind security headers.
pub fn app() -> App {
    App {
        logging: config::logging,
        database: config::database,
        filesystems: config::filesystems,
        migrations: database::migrations,
        seeders: database::seeders,
        routes: |router, config, _db| {
            let settings = config::Settings::from(config);
            let router = router.middleware(rustclamp::log::request_log);
            // Packages (`clamp init --package`) add their routes here, e.g.
            // `let router = router.package(blog::Blog::from(config));`
            routes::api::routes(routes::web::routes(router), &settings)
        },
    }
}
