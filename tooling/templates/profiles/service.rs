//! The `service` profile: HTTP on `PORT` (else the first free port from 8080)
//! with a health check at `/up`, Prometheus metrics at `/metrics`, a request
//! log and security headers on every response.

use std::sync::Arc;

use rustclamp::metrics::Registry;
use rustclamp::web::{self, Router};

/// The service's routes. Tests call them without a server.
pub fn routes() -> Router {
    Router::new()
        .middleware(rustclamp::log::request_log)
        .middleware(web::security_headers)
        .metrics("/metrics", Arc::new(Registry::new()))
        .up("/up")
}

/// Serves [`routes`] until the process is stopped.
pub fn run(_args: &[String]) -> u8 {
    web::serve(routes());
    0
}

#[cfg(test)]
mod tests {
    use rustclamp::web::testing::Client;

    #[test]
    fn health_and_metrics_answer() {
        let routes = super::routes();
        let client = Client::new(&routes);
        assert_eq!(client.get("/up").status, 200);
        assert!(client.get("/metrics").see("http_requests_total 1"));
    }
}
