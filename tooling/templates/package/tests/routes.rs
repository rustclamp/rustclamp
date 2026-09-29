//! The package tests itself on a bare router: no app needed.

use __CRATE__::__STRUCT__;
use rustclamp::config::Config;
use rustclamp::web::{Request, Router};

fn app(env: &str) -> Router {
    Router::new().package(__STRUCT__::from(&Config::parse(env)))
}

#[test]
fn index_renders_the_configured_title_escaped() {
    let response = app("__ENV___TITLE=<Docs>\n").handle(&Request::get("/__NAME__"));
    assert_eq!(response.status, 200);
    let body = String::from_utf8(response.body).unwrap();
    assert!(body.contains("<h1>&lt;Docs&gt;</h1>"), "{body}");
}

#[test]
fn stylesheet_ships_with_the_package() {
    let response = app("").handle(&Request::get("/__NAME__/__NAME__.css"));
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "text/css");
}

#[test]
fn other_paths_are_left_to_the_app() {
    assert_eq!(app("").handle(&Request::get("/elsewhere")).status, 404);
}
