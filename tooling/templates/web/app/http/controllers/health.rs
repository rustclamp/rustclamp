use super::controller::*;
use rustclamp::db::Db;

/// `GET /api/health`: the app is up and its database answers.
pub fn show(_: &Request, db: &Db) -> Response {
    match db.with(|sql| sql.query_row("SELECT 1", [], |_| Ok(()))) {
        Ok(()) => json(r#"{"status":"ok"}"#),
        Err(_) => error(503),
    }
}
