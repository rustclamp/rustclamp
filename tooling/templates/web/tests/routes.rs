use __CRATE__::routes;
use rustclamp::config::Config;
use rustclamp::web::Request;

fn app() -> rustclamp::web::Router {
    routes(&Config::parse("DB_DATABASE=:memory:"))
}

#[test]
fn health_returns_json() {
    let response = app().handle(&Request::get("/api/health"));
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
}

#[test]
fn unknown_path_is_not_found() {
    assert_eq!(app().handle(&Request::get("/nope")).status, 404);
}

#[test]
fn every_response_carries_security_headers() {
    for path in ["/api/health", "/nope"] {
        let response = app().handle(&Request::get(path));
        assert_eq!(response.header("x-frame-options"), Some("DENY"), "{path}");
        assert!(response.header("content-security-policy").is_some(), "{path}");
    }
}

#[test]
fn api_is_throttled_per_client() {
    let routes = routes(&Config::parse("API_PER_MINUTE=1\nDB_DATABASE=:memory:\n"));
    let request = || Request::get("/api/health").with_peer("10.0.0.1".parse().unwrap());
    assert_eq!(routes.handle(&request()).status, 200);
    assert_eq!(routes.handle(&request()).status, 429);
}
