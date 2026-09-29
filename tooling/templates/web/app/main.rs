/// Loads `.env`, starts logging, runs `cargo run -- migrate` (or
/// `migrate:rollback`, `db:seed`) when asked, migrates, links
/// `public/storage`, then serves the routes.
fn main() {
    __CRATE__::app().run();
}
