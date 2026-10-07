//! The HTTP kernel, like Laravel's `app/Http/Kernel.php`: the one place for
//! middleware. A request goes through `global`, then its route's group (`web`
//! or `api`, chosen in `routes/`), then the controller, and the response comes
//! back out the same way. The framework adds security headers around it all.
//!
//! Middleware is `Fn(&Request, Next) -> Response`; the first one added runs
//! first. `clamp make:middleware NAME` writes one into `http/middleware/`.

use std::time::Duration;

use rustclamp::web::{Router, Sessions, csrf};

use crate::config::Settings;

/// Middleware every request passes through, static files and `404`s included.
pub fn global(router: Router) -> Router {
    router.middleware(rustclamp::log::request_log)
}

/// Pages with forms: a session (`request.session()`, flash messages, old
/// input) and CSRF checks on every `POST`. The welcome page needs neither, so
/// `routes/web.rs` keeps it outside this group and sets no cookie.
pub fn web(router: Router) -> Router {
    router
        .middleware(Sessions::new(Duration::from_secs(2 * 60 * 60)).middleware())
        .middleware(csrf())
}

/// JSON endpoints, rate limited per client to `API_PER_MINUTE`.
pub fn api(router: Router, settings: &Settings) -> Router {
    router.middleware(settings.per_minute(settings.api_per_minute).middleware())
}
