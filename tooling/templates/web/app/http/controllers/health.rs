use super::controller::*;

/// `GET /api/health`: the app is up and its database answers.
pub fn show(request: &Request) -> Response {
    match request.db().with(|sql| sql.query_row("SELECT 1", [], |_| Ok(()))) {
        Ok(()) => json(r#"{"status":"ok"}"#),
        Err(_) => error(503),
    }
}
