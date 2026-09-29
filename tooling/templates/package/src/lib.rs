//! A RustClamp package. Everything the feature needs lives here: routes,
//! middleware, views, static files, config and tests. An app adds it with one
//! line, and nothing is copied into the app:
//!
//! ```ignore
//! let routes = Router::new().package(__STRUCT__::from(&config));
//! ```

// Framework types are written as paths, so a package named `config` or `router`
// can call its struct `Config` or `Router`.
use rustclamp::web::{self, escape, package_view};

/// The package name: its URL prefix and the folder the app overrides views in,
/// `app/resources/views/vendor/__NAME__/`.
pub const NAME: &str = "__NAME__";

/// The package, built from the app's settings.
pub struct __STRUCT__ {
    title: String,
}

impl From<&rustclamp::config::Config> for __STRUCT__ {
    /// Reads `__ENV___TITLE` from the app's `.env`.
    fn from(config: &rustclamp::config::Config) -> Self {
        Self {
            title: config.get("__ENV___TITLE").unwrap_or(NAME).to_owned(),
        }
    }
}

impl web::Package for __STRUCT__ {
    /// Handlers are `'static`: move owned settings into them.
    fn routes(self, router: web::Router) -> web::Router {
        let title = escape(&self.title);
        router
            .get("/__NAME__", move |_| {
                package_view(
                    NAME,
                    "index",
                    include_str!("../resources/views/index.html"),
                    &[("title", &title)],
                )
            })
            .get("/__NAME__/__NAME__.css", |_| {
                web::Response::new(200, "text/css", include_str!("../resources/css/__NAME__.css"))
            })
    }
}
