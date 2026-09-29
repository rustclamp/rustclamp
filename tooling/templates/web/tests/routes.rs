//! Call the app like a browser, no server needed: `Client` keeps the session
//! cookie and sends the CSRF token of the last page with each form.

use rustclamp::web::testing::Client;

/// The app on a fresh, migrated and seeded in-memory database. `env` adds
/// `.env` lines, such as `"API_PER_MINUTE=1\n"`.
fn app(env: &str) -> rustclamp::web::Router {
    __CRATE__::app().test(env).0
}

#[test]
fn health_returns_json() {
    let routes = app("");
    let response = Client::new(&routes).get("/api/health");
    assert_eq!((response.status, response.content_type), (200, "application/json"));
}

#[test]
fn unknown_path_is_not_found() {
    assert_eq!(Client::new(&app("")).get("/nope").status, 404);
}

#[test]
fn every_response_carries_security_headers() {
    let routes = app("");
    for path in ["/api/health", "/nope"] {
        let response = Client::new(&routes).get(path);
        assert_eq!(response.header("x-frame-options"), Some("DENY"), "{path}");
        assert!(response.header("content-security-policy").is_some(), "{path}");
    }
}

#[test]
fn api_is_throttled_per_client() {
    let routes = app("API_PER_MINUTE=1\n");
    let client = Client::new(&routes).from("10.0.0.1");
    assert_eq!(client.get("/api/health").status, 200);
    assert_eq!(client.get("/api/health").status, 429);
}
