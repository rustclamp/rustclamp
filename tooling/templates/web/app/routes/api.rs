use rustclamp::web::Router;

use crate::config::Settings;

/// JSON endpoints, rate limited per client.
pub fn routes(router: Router, settings: &Settings) -> Router {
    let throttle = settings.per_minute(settings.api_per_minute);
    // `/api/health` answers `{"status":"ok"}` while the database answers.
    router.group(|api| api.middleware(throttle.middleware()).up("/api/health"))
}
