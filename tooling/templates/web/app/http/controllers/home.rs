//! The welcome page. A route names a controller function, which turns the
//! request into a response: `routes/web.rs` sends `GET /` to `home::index`.

use super::controller::*;

/// `GET /`
pub fn index(_request: &Request) -> Response {
    view("welcome")
}
