use rustclamp::config::Config;
use rustclamp::db::Db;
use rustclamp::log::{Log, Logger};
use rustclamp::prelude::*;
use rustclamp::storage::Storage;

use ::__CRATE__::{config, database};

fn main() {
    let config = Config::load();
    Log::init(Logger::new(&config::logging(&config)));
    // `cargo run -- migrate`, `migrate:rollback` or `db:seed`.
    if let Some(command) = std::env::args().nth(1) {
        let db = Db::connect(&config::database(&config));
        let (migrations, seeders) = (database::migrations(), database::seeders());
        std::process::exit(rustclamp::db::command(&db, &command, &migrations, &seeders));
    }
    // `public/storage` serves the public disk, like `php artisan storage:link`.
    if let Err(error) = Storage::new(config::filesystems(&config)).link() {
        Log::warning(format_args!("storage link failed: {error}"));
    }
    Clamp::run(|| rustclamp::web::serve(::__CRATE__::routes(&config)));
}
