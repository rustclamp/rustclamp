use rustclamp::web::{self, Request, Response};

/// Maps a request to a response. Add routes here.
pub fn handle(request: &Request) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => web::view("welcome"),
        ("GET", "/api/health") => web::json(r#"{"status":"ok"}"#),
        ("GET", _) => web::asset(&request.path),
        _ => Response::text(404, "Not found"),
    }
}
