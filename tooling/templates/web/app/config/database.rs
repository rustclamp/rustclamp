//! The database, read once at startup from `.env` and the environment.
//! `.env.example` lists every key.

use rustclamp::config::Config;
use rustclamp::db::Settings;

/// Which database to open. Change it in `.env`, not here.
pub fn database(config: &Config) -> Settings {
    // `DB_CONNECTION`: `sqlite` (`DB_DATABASE`: the file, or `:memory:`), or
    // `pgsql` with rustclamp's `postgres` feature (`DATABASE_URL`, or
    // `DB_HOST`, `DB_PORT`, `DB_DATABASE`, `DB_USERNAME`, `DB_PASSWORD`,
    // `DB_SSLMODE`, `DB_SSLROOTCERT`; `DB_POOL` caps the connections).
    Settings::from_config(config)
}
