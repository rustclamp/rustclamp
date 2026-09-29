//! The database, read once at startup from `.env` and the environment.
//! `.env.example` lists every key.

use rustclamp::config::Config;
use rustclamp::db::Settings;

/// Which database to open. Change it in `.env`, not here.
pub fn database(config: &Config) -> Settings {
    Settings {
        // `DB_CONNECTION`: the engine; only `sqlite` so far.
        connection: config.get("DB_CONNECTION").unwrap_or("sqlite").into(),
        // `DB_DATABASE`: the SQLite file, or `:memory:`.
        database: config
            .get("DB_DATABASE")
            .unwrap_or("storage/database.sqlite")
            .into(),
    }
}
