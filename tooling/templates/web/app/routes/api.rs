use rustclamp::web::Router;

use crate::config::Settings;
use crate::http::kernel;

/// JSON endpoints, rate limited per client by the kernel's `api` group.
pub fn routes(router: Router, settings: &Settings) -> Router {
    // `/api/health` answers `{"status":"ok"}` while the database answers.
    router.group(|api| kernel::api(api, settings).up("/api/health"))
}
