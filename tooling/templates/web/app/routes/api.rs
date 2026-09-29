use rustclamp::web::{Router, json};

/// JSON endpoints. Add `Throttle::per_minute(60).middleware()` to the group to rate limit them.
pub fn routes(router: Router) -> Router {
    router.group(|api| api.get("/api/health", |_| json(r#"{"status":"ok"}"#)))
}
