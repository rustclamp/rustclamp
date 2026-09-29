use ::__CRATE__::{config, database, routes};

/// Loads `.env`, starts logging, runs `cargo run -- migrate` (or
/// `migrate:rollback`, `db:seed`) when asked, links `public/storage`, then
/// serves the routes.
fn main() {
    rustclamp::web::App {
        logging: config::logging,
        database: config::database,
        filesystems: config::filesystems,
        migrations: database::migrations,
        seeders: database::seeders,
    }
    .run(routes);
}
