use rustclamp::web::{Router, json};

/// The app's routes. A GET that matches none serves the file from `public/`.
pub fn routes() -> Router {
    Router::new()
        .view("/", "welcome")
        .get("/api/health", |_| json(r#"{"status":"ok"}"#))
}
