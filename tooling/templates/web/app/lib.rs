//! Everything the app is made of lives in `app/`: code, routes, config,
//! views and data. The project root keeps only what must survive replacing
//! `app/`: `.env`, `storage/` (uploads, logs) and `public/`.
//!
//! This file is the map: every folder and file of Rust code is declared here,
//! so there are no `mod.rs` files.

use rustclamp::web::App;

/// Everything that handles HTTP: a request goes through the kernel's
/// middleware to a route, and the route's controller makes the response.
pub mod http {
    /// Middleware: global, and the `web` and `api` groups.
    pub mod kernel;
    /// Turn requests into responses (`clamp make:controller NAME`).
    pub mod controllers {
        pub mod controller;
        pub mod home;
    }
    /// `Fn(&Request, Next) -> Response`, added in `kernel` (`clamp make:middleware NAME`).
    pub mod middleware {}
    /// Form input and its validation (`clamp make:request NAME`).
    pub mod requests {}
}

/// Data the app works with (`clamp make:model NAME`).
pub mod models {}

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
/// handlers, behind security headers and the kernel's global middleware.
pub fn app() -> App {
    App {
        logging: config::logging,
        database: config::database,
        filesystems: config::filesystems,
        migrations: database::migrations,
        seeders: database::seeders,
        routes: |router, config, _db| {
            let settings = config::Settings::from(config);
            let router = http::kernel::global(router);
            // Packages (`clamp init --package`) add their routes here, e.g.
            // `let router = router.package(blog::Blog::from(config));`
            routes::api::routes(routes::web::routes(router), &settings)
        },
    }
}
