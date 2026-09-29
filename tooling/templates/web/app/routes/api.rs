use rustclamp::db::Db;
use rustclamp::web::Router;

use crate::config::Settings;
use crate::http::controllers::health;

/// JSON endpoints, rate limited per client.
pub fn routes(router: Router, settings: &Settings, db: &Db) -> Router {
    let throttle = settings.per_minute(settings.api_per_minute);
    let db = db.clone();
    router.group(|api| {
        api.middleware(throttle.middleware())
            .get("/api/health", move |request| health::show(request, &db))
    })
}
