use rustclamp::config::Config;
use rustclamp::db::Db;
use rustclamp::prelude::*;

use ::__CRATE__::database;

fn main() {
    let config = Config::load();
    // `cargo run -- migrate`, `migrate:rollback` or `db:seed`.
    if let Some(command) = std::env::args().nth(1) {
        let db = Db::open(&config);
        let (migrations, seeders) = (database::migrations(), database::seeders());
        std::process::exit(rustclamp::db::command(&db, &command, &migrations, &seeders));
    }
    Clamp::run(|| rustclamp::web::serve(::__CRATE__::routes(&config)));
}
