use __CRATE__::routes;
use rustclamp::web::Request;

#[test]
fn health_returns_json() {
    let response = routes().handle(&Request::get("/api/health"));
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
}

#[test]
fn unknown_path_is_not_found() {
    assert_eq!(routes().handle(&Request::get("/nope")).status, 404);
}
