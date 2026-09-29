use rustclamp::config::Config;
use rustclamp::db::Db;
use rustclamp::prelude::*;

fn main() {
    let config = Config::load();
    // `cargo run -- migrate:rollback` undoes the last batch of migrations.
    if std::env::args().nth(1).as_deref() == Some("migrate:rollback") {
        match Db::open(&config).rollback(&::__CRATE__::database::migrations::all()) {
            Ok(names) => names.iter().for_each(|name| println!("Rolled back {name}")),
            Err(error) => {
                eprintln!("rollback failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    Clamp::run(|| rustclamp::web::serve(::__CRATE__::routes(&config)));
}
