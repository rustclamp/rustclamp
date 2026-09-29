use rustclamp::log::Log;
use rustclamp::web::{Next, Request, Response};

/// Logs every request and its status at `debug` level.
pub fn request_log(request: &Request, next: Next) -> Response {
    let response = next(request);
    Log::debug(format_args!("{} {} {}", request.method, request.path, response.status));
    response
}
