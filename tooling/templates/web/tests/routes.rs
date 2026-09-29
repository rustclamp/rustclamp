use __CRATE__::routes;
use rustclamp::web::Request;

#[test]
fn health_route_returns_json() {
    let response = routes::handle(&Request::new("GET", "/api/health"));
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
}

#[test]
fn unknown_method_is_not_found() {
    let response = routes::handle(&Request::new("DELETE", "/"));
    assert_eq!(response.status, 404);
}
