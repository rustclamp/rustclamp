use super::controller::*;

/// `GET /api/health`: the app is up.
pub fn show(_: &Request) -> Response {
    json(r#"{"status":"ok"}"#)
}
