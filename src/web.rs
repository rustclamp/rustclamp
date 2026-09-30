//! A small, synchronous HTTP server for Clamp web applications.
//!
//! Enabled by the `web` feature. It uses only the standard library: the app
//! declares a [`Router`] and [`serve`] handles ports, request parsing, responses
//! and static files. `public/` is the web root; Vite builds views and assets
//! into `public/build/`. A `GET` that matches no route serves the file of that
//! name from `public/`; anything else is `404`.
//!
//! Error responses are pages: [`error`] renders `errors/404` (or `errors/4xx`)
//! when the app has built that view, and a built-in page otherwise.
//!
//! Middleware wraps handlers: [`Router::middleware`] applies to every request,
//! [`Router::group`] to the routes declared inside it.
//!
//! ```no_run
//! use std::time::Duration;
//! use rustclamp::web::{self, Router, json, throttle};
//!
//! let routes = Router::new()
//!     .view("/", "welcome")
//!     .group(|api| {
//!         api.middleware(throttle(60, Duration::from_secs(60)))
//!             .get("/api/health", |_| json(r#"{"status":"ok"}"#))
//!     });
//!
//! web::serve(routes);
//! ```

#[cfg(feature = "auth")]
pub mod auth;
mod form;
#[cfg(feature = "markdown")]
pub mod markdown;
mod request;
mod session;
pub mod testing;
mod throttle;
mod upload;
mod view;

use std::fs;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use crate::log::Log;

pub use form::{Form, Invalid};
pub use request::{MAX_BODY, Request};
pub use session::{COOKIE, CSRF_FIELD, Session, Sessions, csrf};
pub use throttle::{Throttle, throttle};
pub use upload::{MAX_UPLOAD, UploadedFile};
pub use view::{ToValue, Value};

/// The web root, relative to the working directory. [`asset`] serves files from it.
pub const PUBLIC: &str = "public";

/// How long the server waits for a client to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The rest of the chain, passed to middleware: call it to continue, or return
/// a response without calling it to stop the request there.
pub type Next<'a> = &'a dyn Fn(&Request) -> Response;

type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

/// What a handler may return: a [`Response`], or a [`Result`] whose error is
/// logged with the request's method and path and answered with `500`, as an
/// uncaught exception is in Laravel. So a handler can use `?`:
///
/// ```
/// use rustclamp::web::{self, Request, Response, Router};
///
/// fn show(request: &Request) -> web::Result {
///     let id: u32 = request.query("id").unwrap_or_default().parse()?;
///     Ok(Response::text(200, &id.to_string()))
/// }
///
/// let app = Router::new().get("/", show);
/// assert_eq!(app.handle(&Request::get("/?id=7")).body, b"7");
/// assert_eq!(app.handle(&Request::get("/?id=x")).status, 500);
/// ```
pub trait IntoResponse {
    /// The response to send for `request`.
    fn into_response(self, request: &Request) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self, _: &Request) -> Response {
        self
    }
}

impl<T: IntoResponse, E: std::fmt::Display> IntoResponse for std::result::Result<T, E> {
    fn into_response(self, request: &Request) -> Response {
        match self {
            Ok(response) => response.into_response(request),
            Err(problem) => {
                Log::error(format_args!(
                    "{} {}: {problem}",
                    request.method, request.path
                ));
                error(500)
            }
        }
    }
}

/// A handler's result: any error converts with `?` and answers `500`.
pub type Result<T = Response> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Middleware = Arc<dyn Fn(&Request, Next) -> Response + Send + Sync>;

/// The app's routes: exact method and path matches, checked in order.
#[derive(Default)]
pub struct Router {
    routes: Vec<(&'static str, String, Handler)>,
    middleware: Vec<Middleware>,
    state: request::State,
}

impl Router {
    /// An empty router. Unmatched `GET`s still serve files from [`PUBLIC`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `GET path` with `handler`.
    #[must_use]
    pub fn get<R: IntoResponse>(
        self,
        path: &str,
        handler: impl Fn(&Request) -> R + Send + Sync + 'static,
    ) -> Self {
        self.route("GET", path, handler)
    }

    /// Answers `POST path` with `handler`.
    #[must_use]
    pub fn post<R: IntoResponse>(
        self,
        path: &str,
        handler: impl Fn(&Request) -> R + Send + Sync + 'static,
    ) -> Self {
        self.route("POST", path, handler)
    }

    /// A health check at `GET path`, like Laravel's `/up`: `{"status":"ok"}`,
    /// or `503` when the database shared with [`Router::state`] does not answer.
    #[must_use]
    pub fn up(self, path: &str) -> Self {
        self.get(path, |request: &Request| {
            #[cfg(feature = "db")]
            if let Some(db) = request.state::<crate::db::Db>()
                && db
                    .with(|sql| sql.query_row("SELECT 1", [], |_| Ok(())))
                    .is_err()
            {
                return error(503);
            }
            #[cfg(not(feature = "db"))]
            let _ = request;
            json(r#"{"status":"ok"}"#)
        })
    }

    /// Answers `GET path` with the built view `name`, as [`view`] does.
    #[must_use]
    pub fn view(self, path: &str, name: &'static str) -> Self {
        self.get(path, move |_| view(name))
    }

    /// Answers `method path` with `handler`. A `{name}` segment matches any
    /// one segment and is read with [`Request::param`], as in `/blog/{slug}`.
    #[must_use]
    pub fn route<R: IntoResponse>(
        mut self,
        method: &'static str,
        path: &str,
        handler: impl Fn(&Request) -> R + Send + Sync + 'static,
    ) -> Self {
        self.routes.push((
            method,
            path.into(),
            Box::new(move |request: &Request| handler(request).into_response(request)),
        ));
        self
    }

    /// Shares `value` with every handler, read with [`Request::state`], such as
    /// the app's database (`request.db()` with the `db` feature). One value
    /// per type; add it to the router that [`serve`] runs.
    ///
    /// ```
    /// use rustclamp::web::{Request, Response, Router};
    ///
    /// struct Greeting(&'static str);
    ///
    /// let app = Router::new()
    ///     .state(Greeting("hi"))
    ///     .get("/", |request| Response::text(200, request.state::<Greeting>().unwrap().0));
    /// assert_eq!(app.handle(&Request::get("/")).body, b"hi");
    /// ```
    #[must_use]
    pub fn state<T: std::any::Any + Send + Sync>(mut self, value: T) -> Self {
        Arc::make_mut(&mut self.state.0).push(Arc::new(value));
        self
    }

    /// Wraps every request this router handles, including static files and
    /// `404`s, in `middleware`. The first middleware added runs first.
    #[must_use]
    pub fn middleware(
        mut self,
        middleware: impl Fn(&Request, Next) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Adds the routes `build` declares, wrapped in the middleware it declares,
    /// such as a `web` group with sessions and an `api` group with a throttle.
    /// Global middleware still runs first.
    #[must_use]
    pub fn group(mut self, build: impl FnOnce(Router) -> Router) -> Self {
        let group = build(Router::new());
        let chain: Arc<[Middleware]> = group.middleware.into();
        for (method, path, handler) in group.routes {
            let chain = Arc::clone(&chain);
            self.routes.push((
                method,
                path,
                Box::new(move |request: &Request| run(&chain, request, &handler)),
            ));
        }
        self
    }

    /// Adds `package`'s routes as a [`group`](Self::group), so middleware the
    /// package declares wraps only its own routes. Routes match in the order
    /// added: routes declared before `.package(...)` win over the package's.
    #[must_use]
    pub fn package(self, package: impl Package) -> Self {
        self.group(|router| package.routes(router))
    }

    /// Runs the middleware, then the first matching route, else a static file
    /// for `GET`, else `404`.
    pub fn handle(&self, request: &Request) -> Response {
        let response = if self.state.0.is_empty() {
            run(&self.middleware, request, &|request| self.dispatch(request))
        } else {
            let mut request = request.clone();
            request.state = self.state.clone();
            run(&self.middleware, &request, &|request| {
                self.dispatch(request)
            })
        };
        // A 403 would confirm the page exists to someone not allowed to see
        // it, so it is answered as the 404 page and logged with its real status.
        if response.status == 403 {
            Log::notice(format_args!(
                "http_status_code=403 {} {}",
                request.method, request.path
            ));
            // Keep the middleware's headers (security headers), so the
            // disguised 404 matches a real one down to its headers. Cookies
            // go: a URL that matches no route never runs session middleware,
            // so a session cookie would give the page away. A refused request
            // never moved the session, so nothing is lost.
            let mut hidden = error(404);
            hidden.headers = response
                .headers
                .into_iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("set-cookie"))
                .collect();
            return hidden;
        }
        response
    }

    fn dispatch(&self, request: &Request) -> Response {
        let matched = self.routes.iter().find_map(|(method, pattern, handler)| {
            let params = (*method == request.method).then(|| params(pattern, &request.path))??;
            Some((handler, params))
        });
        match matched {
            Some((handler, params)) if params.is_empty() => handler(request),
            Some((handler, params)) => {
                let mut request = request.clone();
                request.params = params;
                handler(&request)
            }
            None if request.method == "GET" => asset(&request.path),
            None => error(404),
        }
    }
}

/// A reusable feature in its own crate, like a Laravel package: its routes,
/// middleware, views and config live in the package, and an app adds it with
/// [`Router::package`].
///
/// Views ship inside the package (`include_str!`) and render with
/// [`package_view`], which prefers the app's override. Settings come from the
/// app's [`Config`](crate::config::Config), passed to the package's constructor.
///
/// ```
/// use rustclamp::web::{Package, Request, Response, Router};
///
/// struct Hello {
///     greeting: String,
/// }
///
/// impl Package for Hello {
///     fn routes(self, router: Router) -> Router {
///         let greeting = self.greeting;
///         router.get("/hello", move |_| Response::text(200, &greeting))
///     }
/// }
///
/// let app = Router::new().package(Hello { greeting: "hi".into() });
/// assert_eq!(app.handle(&Request::get("/hello")).body, b"hi");
/// ```
pub trait Package {
    /// Adds the package's routes and middleware to `router`.
    fn routes(self, router: Router) -> Router;
}

/// The view `name` of `package`, rendered with `data` as in [`render`]. The
/// app's built override, `public/build/views/vendor/{package}/{name}.html`
/// (source `app/resources/views/vendor/{package}/{name}.html`), wins over
/// `embedded`, the package's own copy. Views it extends or includes are the
/// app's, so a package page can `@extends('layouts.app')`.
pub fn package_view(
    package: &str,
    name: &str,
    embedded: &str,
    data: &[(&str, &dyn ToValue)],
) -> Response {
    package_view_in(Path::new(PUBLIC), package, name, embedded, data)
}

fn package_view_in(
    public: &Path,
    package: &str,
    name: &str,
    embedded: &str,
    data: &[(&str, &dyn ToValue)],
) -> Response {
    let full = format!("vendor/{package}/{name}");
    let source = built_view(public, &full).unwrap_or_else(|| embedded.to_owned());
    rendered(public, &full, &source, &Value::map(data))
}

/// The `{name}` values when `path` matches `pattern`, segment by segment.
fn params(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
    let mut params = Vec::new();
    let mut segments = path.split('/');
    for expected in pattern.split('/') {
        let segment = segments.next()?;
        match expected
            .strip_prefix('{')
            .and_then(|name| name.strip_suffix('}'))
        {
            Some(name) if !segment.is_empty() => params.push((name.to_owned(), segment.to_owned())),
            Some(_) => return None,
            None if expected == segment => {}
            None => return None,
        }
    }
    segments.next().is_none().then_some(params)
}

/// Calls `chain` in order, then `last`.
fn run(chain: &[Middleware], request: &Request, last: Next) -> Response {
    match chain.split_first() {
        None => last(request),
        Some((first, rest)) => first(request, &|request| run(rest, request, last)),
    }
}

/// An HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The `Content-Type` header value.
    pub content_type: &'static str,
    /// Extra headers, such as `Set-Cookie` or `Location`.
    pub headers: Vec<(String, String)>,
    /// The response body.
    pub body: Vec<u8>,
}

impl Response {
    /// A response with the given status, content type and body.
    pub fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// A plain-text response with the given status.
    pub fn text(status: u16, body: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body)
    }

    /// Adds a header. Line breaks in `name` or `value` become spaces, so a
    /// value taken from a request cannot inject headers of its own.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        let clean = |text: &str| text.replace(['\r', '\n'], " ");
        self.headers.push((clean(name), clean(value)));
        self
    }

    /// The first value of header `name`, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A `200` JSON response with a pre-serialized body.
pub fn json(body: &str) -> Response {
    Response::new(200, "application/json", body)
}

/// A `302` redirect to `location`.
pub fn redirect(location: &str) -> Response {
    Response::text(302, "").with_header("Location", location)
}

/// Renders the built view `name` without data, as [`render`] does.
pub fn view(name: &str) -> Response {
    render(name, &[])
}

/// Renders the built view `name`, `public/build/views/{name}.html` (source
/// `app/resources/views/`), with `data`: Blade-style `{{ title }}`,
/// `@foreach`, `@if`, `@extends` and `@include`, described in [`Value`]'s
/// module. `{{ }}` escapes; `{!! !!}` does not. `503` if the frontend is not
/// built yet; a view that fails to render is logged and answers `500`.
///
/// ```no_run
/// use rustclamp::web::render;
///
/// let posts = vec!["First", "Second"];
/// render("blog", &[("title", &"Blog"), ("posts", &posts)]);
/// ```
pub fn render(name: &str, data: &[(&str, &dyn ToValue)]) -> Response {
    render_value(name, &Value::map(data))
}

fn render_value(name: &str, data: &Value) -> Response {
    let public = Path::new(PUBLIC);
    match built_view(public, name) {
        Some(source) => rendered(public, name, &source, data),
        // The app's own error views are not built either, so use the built-in page.
        None => page(
            503,
            "The frontend is not built yet. If <code>clamp dev</code> is running, refresh in a moment; otherwise run <code>clamp dev</code>.",
        ),
    }
}

/// The built view `name` (`layouts.app` or `layouts/app`) under `public`.
fn built_view(public: &Path, name: &str) -> Option<String> {
    if !view::is_view_name(name) {
        return None;
    }
    let file = public
        .join("build/views")
        .join(format!("{}.html", name.replace('.', "/")));
    fs::read_to_string(file).ok()
}

/// `200` with `source` rendered, or a logged `500`.
fn rendered(public: &Path, name: &str, source: &str, data: &Value) -> Response {
    match view::render(name, source, data, &|name| built_view(public, name)) {
        Ok(body) => html(200, body.into_bytes()),
        Err(problem) => {
            Log::error(format_args!("{problem}"));
            error(500)
        }
    }
}

/// Escapes text for HTML content and quoted attribute values.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The Content-Security-Policy [`security_headers`] sends. Scripts only from
/// the app's own files, so injected markup cannot run: no inline `<script>`
/// and no `onclick=`. Styles may be inline (the built-in error page is) and
/// fonts may come from Google Fonts, which the starter kits use.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
    font-src 'self' https://fonts.gstatic.com; img-src 'self' data:; \
    object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

/// Middleware adding the browser security headers every page should carry:
/// no MIME sniffing, no framing by other sites, no full URLs leaked as
/// referrer, HTTPS only for a year once a browser has seen the site over
/// HTTPS (browsers ignore HSTS on plain HTTP, so local development is
/// unaffected), no camera, microphone or location access, and
/// [`CONTENT_SECURITY_POLICY`]. A response that already sets
/// `Content-Security-Policy` keeps its own.
pub fn security_headers(request: &Request, next: Next) -> Response {
    let response = next(request);
    let has_policy = response.header("content-security-policy").is_some();
    let response = response
        .with_header("X-Content-Type-Options", "nosniff")
        .with_header("X-Frame-Options", "DENY")
        .with_header("Referrer-Policy", "strict-origin-when-cross-origin")
        .with_header("Strict-Transport-Security", "max-age=31536000")
        .with_header(
            "Permissions-Policy",
            "camera=(), microphone=(), geolocation=()",
        );
    if has_policy {
        response
    } else {
        response.with_header("Content-Security-Policy", CONTENT_SECURITY_POLICY)
    }
}

/// An error page for `status`: the app's built view `errors/{status}`, else
/// `errors/{class}xx` (such as `errors/4xx`), else a built-in page. In the
/// app's view, `{{ status }}` and `{{ reason }}` are the code and its reason
/// phrase, so one `4xx` view serves every client error. An error view that
/// fails to render is logged and the built-in page answers instead.
pub fn error(status: u16) -> Response {
    let public = Path::new(PUBLIC);
    let data = Value::map(&[("status", &status), ("reason", &reason(status))]);
    for name in [
        format!("errors/{status}"),
        format!("errors/{}xx", status / 100),
    ] {
        if let Some(source) = built_view(public, &name) {
            match view::render(&name, &source, &data, &|name| built_view(public, name)) {
                Ok(body) => return html(status, body.into_bytes()),
                Err(problem) => {
                    Log::error(format_args!("{problem}"));
                    break;
                }
            }
        }
    }
    page(status, "")
}

fn html(status: u16, body: Vec<u8>) -> Response {
    Response::new(status, "text/html; charset=utf-8", body)
}

/// The built-in page: status, reason, an optional trusted HTML `detail` and a link home.
fn page(status: u16, detail: &str) -> Response {
    let reason = reason(status);
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("<p>{detail}</p>")
    };
    html(
        status,
        format!(
            r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{status} {reason}</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Instrument+Sans:wght@400;500&family=Geist+Mono:wght@500&display=swap" rel="stylesheet">
<style>
:root {{ color-scheme: light; --bg: #fff; --fg: #0a0a0a; --fg-2: #525252; --line: #00000018; --accent: #ea580c; --accent-strong: #c2410c;
  --grad: linear-gradient(256.93deg, #b45309 -35%, #d97706 -17%, #ea580c 12%, #c2410c 27%, #db2777 68%, #dc2626 129%, #f59e0b 158%); }}
@media (prefers-color-scheme: dark) {{ :root {{ color-scheme: dark; --bg: #0a0a0a; --fg: #eeeeec; --fg-2: #b5b3ad; --line: #ffffff20; --accent: #fb923c; --accent-strong: #fdba74;
  --grad: linear-gradient(256.93deg, #fde68a -35%, #fcd34d -17%, #fb923c 12%, #fdba74 27%, #f9a8d4 68%, #fca5a5 129%, #fed7aa 158%); }} }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: var(--bg); color: var(--fg); font: 400 16px/1.6 "Instrument Sans", ui-sans-serif, system-ui, sans-serif; -webkit-font-smoothing: antialiased;
  background-image: radial-gradient(50% 45% at 50% 0%, color-mix(in srgb, var(--accent) 14%, transparent), transparent 70%); }}
main {{ padding: 1.5rem; max-width: 40rem; text-align: center; }}
.big {{ margin: 1rem 0 0; font-size: clamp(6rem, 22vw, 12rem); line-height: .9; letter-spacing: -0.06em; background: var(--grad); -webkit-background-clip: text; background-clip: text; color: transparent; padding: .12em 0; }}
h1 {{ margin: .5rem 0 0; font-weight: 400; letter-spacing: -0.025em; line-height: 1.1; font-size: clamp(2rem, 5vw, 3rem); text-wrap: balance; }}
p {{ color: var(--fg-2); font-size: 1.1rem; text-wrap: pretty; }}
code {{ font: .9em "Geist Mono", ui-monospace, monospace; color: var(--fg); }}
a {{ display: inline-flex; align-items: center; height: 3rem; margin-top: 1.5rem; padding: 0 1.5rem; border-radius: 999px; background: var(--accent); color: #fff; font-size: 1.05rem; text-decoration: none; box-shadow: 0 1px 2px #0000000d; transition: background-color 60ms ease-out; }}
a:hover {{ background: var(--accent-strong); }}
a:focus-visible {{ outline: 2px solid var(--accent); outline-offset: 2px; }}
</style>
</head>
<body><main><p class="big">{status}</p><h1>{reason}</h1>{detail}<a href="/">Go home</a></main></body>
</html>
"#
        )
        .into_bytes(),
    )
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        419 => "Page Expired",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => match status / 100 {
            2 => "Success",
            3 => "Redirect",
            4 => "Client Error",
            _ => "Server Error",
        },
    }
}

/// Serves a static file from [`PUBLIC`], refusing any path that could leave it.
pub fn asset(path: &str) -> Response {
    let relative = Path::new(path.trim_start_matches('/'));
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return error(404);
    }
    // Views are templates: they are served rendered, by a route, never raw.
    if relative.starts_with("build/views") {
        return error(404);
    }
    let file = Path::new(PUBLIC).join(relative);
    let Ok(body) = fs::read(&file) else {
        return error(404);
    };
    let content_type = match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    Response::new(200, content_type, body)
}

/// A web app: its config functions, its migrations and seeders, and its
/// routes. The app declares one in `app/lib.rs`; `main.rs` runs it and tests
/// build it, so the wiring every app repeats lives here: opening and migrating
/// the database, sharing it and the storage disks with handlers
/// (`request.db()`, `request.storage()`) and adding [`security_headers`].
///
/// ```no_run
/// # mod config {
/// #     use rustclamp::config::Config;
/// #     pub fn logging(c: &Config) -> rustclamp::log::Settings { rustclamp::log::Settings::from_config(c) }
/// #     pub fn database(c: &Config) -> rustclamp::db::Settings { rustclamp::db::Settings::from_config(c) }
/// #     pub fn filesystems(_: &Config) -> rustclamp::storage::Settings { unimplemented!() }
/// # }
/// # mod database {
/// #     pub fn migrations() -> Vec<&'static dyn rustclamp::db::Migration> { Vec::new() }
/// #     pub fn seeders() -> Vec<&'static dyn rustclamp::db::Seeder> { Vec::new() }
/// # }
/// use rustclamp::config::Config;
/// use rustclamp::db::Db;
/// use rustclamp::web::{App, Router};
///
/// fn routes(router: Router, _config: &Config, _db: &Db) -> Router {
///     router.view("/", "welcome")
/// }
///
/// pub fn app() -> App {
///     App {
///         logging: config::logging,
///         database: config::database,
///         filesystems: config::filesystems,
///         migrations: database::migrations,
///         seeders: database::seeders,
///         routes,
///         # #[cfg(feature = "auth")]
///         # roles: &["super-admin", "admin", "user", "blocked"],
///         # #[cfg(feature = "mail")]
///         # mail: rustclamp::mail::Settings::from_config,
///     }
/// }
///
/// fn main() {
///     app().run();
/// }
/// ```
#[cfg(feature = "db")]
#[derive(Debug, Clone, Copy)]
pub struct App {
    /// `app/config/logging.rs`.
    pub logging: fn(&crate::config::Config) -> crate::log::Settings,
    /// `app/config/database.rs`.
    pub database: fn(&crate::config::Config) -> crate::db::Settings,
    /// `app/config/filesystems.rs`.
    pub filesystems: fn(&crate::config::Config) -> crate::storage::Settings,
    /// Every migration, from `build.rs`.
    pub migrations: fn() -> Vec<&'static dyn crate::db::Migration>,
    /// Every seeder, from `build.rs`.
    pub seeders: fn() -> Vec<&'static dyn crate::db::Seeder>,
    /// Adds the app's routes to a router that already shares the database
    /// and disks and sends security headers.
    pub routes: fn(Router, &crate::config::Config, &crate::db::Db) -> Router,
    /// The app's roles, highest first (ADR 0013), such as
    /// `&["super-admin", "admin", "user", "blocked"]`.
    #[cfg(feature = "auth")]
    pub roles: &'static [&'static str],
    /// `app/config/mail.rs` (ADR 0015).
    #[cfg(feature = "mail")]
    pub mail: fn(&crate::config::Config) -> crate::mail::Settings,
}

#[cfg(feature = "db")]
impl App {
    /// Loads `.env`, installs the logger, then either runs a database command
    /// given as the first argument (`migrate`, `migrate:rollback`, `db:seed`)
    /// and exits with its status, or migrates, links `public/storage` to the
    /// public disk (like `php artisan storage:link`; a failure is a warning)
    /// and serves the routes.
    ///
    /// # Panics
    ///
    /// When a migration fails: the app must not serve an old schema.
    pub fn run(self) {
        use crate::db::Db;
        use crate::log::Logger;
        use crate::storage::Storage;

        let config = crate::config::Config::load();
        Log::init(Logger::new(&(self.logging)(&config)));
        let db = Db::connect(&(self.database)(&config));
        #[cfg(feature = "auth")]
        if std::env::args().nth(1).as_deref() == Some("user:create") {
            std::process::exit(
                self.create_user(&db, &std::env::args().skip(2).collect::<Vec<_>>()),
            );
        }
        if let Some(command) = std::env::args().nth(1) {
            let (migrations, seeders) = ((self.migrations)(), (self.seeders)());
            std::process::exit(crate::db::command(&db, &command, &migrations, &seeders));
        }
        db.migrate(&(self.migrations)())
            .unwrap_or_else(|error| panic!("migration failed: {error}"));
        if let Err(error) = Storage::new((self.filesystems)(&config)).link() {
            Log::warning(format_args!("storage link failed: {error}"));
        }
        // Handlers only queue mail; this thread delivers it, within the cap.
        #[cfg(feature = "mail")]
        std::sync::Arc::new(crate::mail::Mailer::new((self.mail)(&config))).start(db.clone());
        serve(self.router(&config, db));
    }

    /// The app's routes on `db`, shared with handlers together with the
    /// storage disks, behind [`security_headers`].
    pub fn router(&self, config: &crate::config::Config, db: crate::db::Db) -> Router {
        let router = Router::new()
            .state(db.clone())
            .state(crate::storage::Storage::new((self.filesystems)(config)))
            .middleware(security_headers);
        #[cfg(feature = "auth")]
        let router = router.state(auth::Auth::from_config(self.roles, config));
        #[cfg(feature = "mail")]
        let router = router.state(std::sync::Arc::new(crate::mail::Mailer::new((self.mail)(
            config,
        ))));
        (self.routes)(router, config, &db)
    }

    /// `user:create <email> <role> [name]`: migrates, creates the user and
    /// prints a generated password once, so no seeder ships a default one.
    #[cfg(feature = "auth")]
    fn create_user(&self, db: &crate::db::Db, args: &[String]) -> i32 {
        let [email, role, rest @ ..] = args else {
            eprintln!("usage: user:create <email> <role> [name]");
            return 2;
        };
        if let Err(error) = db.migrate(&(self.migrations)()) {
            eprintln!("migration failed: {error}");
            return 1;
        }
        let name = rest.first().map_or(email.as_str(), String::as_str);
        let password = session::random_token()[..24].to_owned();
        let auth = auth::Auth::new(self.roles);
        match auth
            .create_user(db, name, email, &password, role)
            .and_then(|id| auth.mark_verified(db, id))
        {
            Ok(()) => {
                println!("Created {email} ({role}). Password, shown once: {password}");
                0
            }
            Err(error) => {
                eprintln!("could not create {email}: {error}");
                1
            }
        }
    }

    /// For tests: the app on a fresh in-memory database, migrated and seeded,
    /// with `env` (`.env` lines such as `"API_PER_MINUTE=2\n"`) as its config.
    /// Returns the database too, so a test can check what was stored.
    /// Drive it with [`testing::Client`].
    ///
    /// # Panics
    ///
    /// When a migration or seeder fails.
    pub fn test(&self, env: &str) -> (Router, crate::db::Db) {
        // A fixed test URL and key, for signed links; `env` can override them.
        let config = crate::config::Config::parse(&format!(
            "DB_DATABASE=:memory:\nAPP_URL=http://localhost\n\
             APP_KEY=base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n{env}"
        ));
        let db = crate::db::Db::connect(&(self.database)(&config));
        db.migrate(&(self.migrations)())
            .unwrap_or_else(|error| panic!("migration failed: {error}"));
        db.seed(&(self.seeders)())
            .unwrap_or_else(|error| panic!("seeding failed: {error}"));
        (self.router(&config, db.clone()), db)
    }
}

/// Threads answering requests unless `WEB_THREADS` says otherwise.
const THREADS: usize = 32;

/// Listens on `127.0.0.1` and answers requests with `routes` on a pool of
/// `WEB_THREADS` threads (default 32), so one slow request does not hold up
/// the rest.
///
/// Uses `PORT` when set; otherwise the first free port from 8080 to 8099, then
/// any free port. Exits the process if `PORT` is set but unavailable.
pub fn serve(routes: Router) {
    let ports: Vec<u16> = match std::env::var("PORT") {
        Ok(port) => vec![port.parse().expect("PORT must be a number")],
        Err(_) => (8080..8100).chain([0]).collect(),
    };
    let Some(listener) = ports
        .iter()
        .find_map(|port| TcpListener::bind(("127.0.0.1", *port)).ok())
    else {
        eprintln!("Port {} is in use; set PORT to another one.", ports[0]);
        std::process::exit(1);
    };
    let address = listener.local_addr().expect("listener has an address");
    let threads = std::env::var("WEB_THREADS").map_or(THREADS, |threads| {
        threads.parse().expect("WEB_THREADS must be a number")
    });
    println!("Serving on http://{address}");
    serve_on(listener, routes, threads);
}

/// Answers connections from `listener` on `threads` worker threads. Accepted
/// connections wait in a queue of four per thread; when it is full, the
/// operating system's backlog holds the rest, so a flood cannot spawn threads
/// or grow memory without bound.
// ponytail: a fixed pool of blocking threads; each slow client holds one for up
// to READ_TIMEOUT, so size WEB_THREADS above the slow clients you expect, or
// put nginx in front (it buffers requests), before reaching for async.
// Memory: each thread holds one parsed request, so the worst case is
// WEB_THREADS x the largest body (10 MiB uploads: 32 x 10 MiB = 320 MiB).
fn serve_on(listener: TcpListener, routes: Router, threads: usize) {
    let threads = threads.max(1);
    let routes = Arc::new(routes);
    let (queue, waiting) = std::sync::mpsc::sync_channel::<TcpStream>(threads * 4);
    let waiting = Arc::new(std::sync::Mutex::new(waiting));
    for _ in 0..threads {
        let (routes, waiting) = (Arc::clone(&routes), Arc::clone(&waiting));
        std::thread::spawn(move || {
            loop {
                // The lock is held only while taking the next connection.
                let next = waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv();
                let Ok(stream) = next else { return };
                // A panic outside the handler (writing the response) must not
                // shrink the pool.
                let _ = catch_unwind(AssertUnwindSafe(|| respond(stream, &routes)));
            }
        });
    }
    for stream in listener.incoming().flatten() {
        if queue.send(stream).is_err() {
            return;
        }
    }
}

fn respond(mut stream: TcpStream, routes: &Router) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let peer = stream.peer_addr().ok().map(|address| address.ip());
    let parsed = request::parse(&mut BufReader::new(&stream), peer);
    let response = match parsed {
        // A panicking handler answers 500 instead of stopping the server.
        Ok(request) => {
            catch_unwind(AssertUnwindSafe(|| routes.handle(&request))).unwrap_or_else(|panic| {
                let reason = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Log::error(format_args!(
                    "{} {} panicked: {reason}",
                    request.method, request.path
                ));
                error(500)
            })
        }
        Err(response) => response,
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_requests_do_not_hold_up_the_rest() {
        use std::io::Read;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new().get("/slow", |_| {
            std::thread::sleep(Duration::from_millis(300));
            Response::text(200, "done")
        });
        std::thread::spawn(move || serve_on(listener, routes, 4));
        let get = move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        let started = Instant::now();
        let clients: Vec<_> = (0..4).map(|_| std::thread::spawn(get)).collect();
        for client in clients {
            assert!(client.join().unwrap().ends_with("done"));
        }
        // One at a time would take 1.2 s.
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "{:?}",
            started.elapsed()
        );

        // A client that connects and sends nothing holds one thread, not the server.
        let _idle = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        assert!(get().ends_with("done"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn asset_refuses_paths_outside_dist() {
        assert_eq!(asset("/../Cargo.toml").status, 404);
        assert_eq!(asset("/assets/../../Cargo.toml").status, 404);
        assert_eq!(asset("//etc/passwd").status, 404);
    }

    #[test]
    fn router_matches_method_and_path_then_falls_back() {
        let routes = Router::new()
            .get("/hello", |_| json("{}"))
            .post("/hello", |_| Response::text(201, "created"));
        assert_eq!(routes.handle(&Request::get("/hello")).status, 200);
        assert_eq!(routes.handle(&Request::post("/hello")).status, 201);
        assert_eq!(routes.handle(&Request::get("/missing.css")).status, 404);
        assert_eq!(routes.handle(&Request::new("DELETE", "/hello")).status, 404);
    }

    #[test]
    fn errors_fall_back_to_the_built_in_page() {
        // No app views are built under the crate root, so the built-in page answers.
        let response = error(418);
        assert_eq!(response.status, 418);
        assert_eq!(response.content_type, "text/html; charset=utf-8");
        assert!(
            String::from_utf8(response.body)
                .unwrap()
                .contains("Client Error")
        );
        assert_eq!(Router::new().handle(&Request::get("/missing")).status, 404);
    }

    #[test]
    fn middleware_runs_in_order_and_groups_stay_scoped() {
        let tag = |name: &'static str| {
            move |request: &Request, next: Next| {
                let response = next(request);
                let trail = response.header("x-trail").unwrap_or_default().to_owned();
                let mut response = response;
                response.headers.retain(|(key, _)| key != "x-trail");
                response.with_header("x-trail", &format!("{name}{trail}"))
            }
        };
        let routes = Router::new()
            .middleware(tag("a"))
            .middleware(tag("b"))
            .group(|group| group.middleware(tag("g")).get("/in", |_| json("{}")))
            .get("/out", |_| json("{}"));
        let trail = |path| {
            routes
                .handle(&Request::get(path))
                .header("x-trail")
                .map(str::to_owned)
        };
        assert_eq!(trail("/in").as_deref(), Some("abg"));
        assert_eq!(trail("/out").as_deref(), Some("ab"));
        assert_eq!(trail("/missing").as_deref(), Some("ab"));

        let stop = Router::new()
            .middleware(|_: &Request, _: Next| Response::text(401, "no"))
            .get("/", |_| json("{}"));
        assert_eq!(stop.handle(&Request::get("/")).status, 401);
    }

    #[test]
    fn params_match_one_segment() {
        let routes = Router::new().get("/blog/{slug}", |request| {
            Response::text(200, request.param("slug").unwrap_or_default())
        });
        let body = |path| {
            let response = routes.handle(&Request::get(path));
            (
                response.status,
                String::from_utf8(response.body).unwrap_or_default(),
            )
        };
        assert_eq!(
            body("/blog/hello-clamp?x=1"),
            (200, "hello-clamp".to_owned())
        );
        assert_eq!(body("/blog/").0, 404);
        assert_eq!(body("/blog/a/b").0, 404);
        assert_eq!(body("/blog").0, 404);
        assert_eq!(params("/a/{x}/c/{y}", "/a/1/c/2").unwrap().len(), 2);
    }

    #[test]
    fn headers_cannot_inject_lines() {
        let response = redirect("/next\r\nSet-Cookie: evil=1");
        assert_eq!(response.status, 302);
        assert_eq!(
            response.header("location"),
            Some("/next  Set-Cookie: evil=1")
        );
    }

    #[test]
    fn escape_and_security_headers() {
        assert_eq!(
            escape(r#"<a href="x" title='y'>&</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;&lt;/a&gt;"
        );
        let routes = Router::new()
            .middleware(security_headers)
            .get("/", |_| json("{}"));
        let response = routes.handle(&Request::get("/"));
        assert_eq!(response.header("x-frame-options"), Some("DENY"));
        assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(
            response.header("strict-transport-security"),
            Some("max-age=31536000")
        );
        let policy = response.header("content-security-policy").unwrap();
        assert!(
            policy.contains("script-src 'self';") && !policy.contains("script-src 'self' 'unsafe")
        );
        assert!(policy.contains("frame-ancestors 'none'"));

        let own = Router::new().middleware(security_headers).get("/", |_| {
            json("{}").with_header("Content-Security-Policy", "default-src 'none'")
        });
        let response = own.handle(&Request::get("/"));
        let policies: Vec<_> = response
            .headers
            .iter()
            .filter(|(name, _)| name == "Content-Security-Policy")
            .collect();
        assert_eq!(
            policies.len(),
            1,
            "a route's own policy is kept, not doubled"
        );
        assert_eq!(policies[0].1, "default-src 'none'");
    }

    #[test]
    fn package_middleware_stays_on_package_routes() {
        struct Locked;
        impl Package for Locked {
            fn routes(self, router: Router) -> Router {
                router
                    .middleware(|_: &Request, _: Next| Response::text(401, "no"))
                    .get("/locked", |_| json("{}"))
            }
        }
        let routes = Router::new().package(Locked).get("/open", |_| json("{}"));
        assert_eq!(routes.handle(&Request::get("/locked")).status, 401);
        assert_eq!(routes.handle(&Request::get("/open")).status, 200);
    }

    #[test]
    fn package_view_prefers_the_app_override_and_extends_app_layouts() {
        let public = std::env::temp_dir().join(format!("clamp-pkg-{}", std::process::id()));
        let body = |response: Response| String::from_utf8(response.body).unwrap();
        let data: [(&str, &dyn ToValue); 1] = [("title", &"<Hi>")];
        assert_eq!(
            body(package_view_in(
                &public,
                "blog",
                "index",
                "<h1>{{ title }}</h1>",
                &data
            )),
            "<h1>&lt;Hi&gt;</h1>"
        );
        let dir = public.join("build/views/vendor/blog");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("index.html"),
            "@extends('layouts.app')@section('main')<h2>{{ title }}</h2>@endsection",
        )
        .unwrap();
        fs::create_dir_all(public.join("build/views/layouts")).unwrap();
        fs::write(
            public.join("build/views/layouts/app.html"),
            "<main>@yield('main')</main>",
        )
        .unwrap();
        let response = package_view_in(&public, "blog", "index", "<h1>{{ title }}</h1>", &data);
        fs::remove_dir_all(&public).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "<main><h2>&lt;Hi&gt;</h2></main>");
    }

    #[test]
    fn forbidden_is_answered_as_not_found() {
        let routes = Router::new()
            .middleware(security_headers)
            .get("/secret", |_| Response::text(403, "no"));
        let hidden = routes.handle(&Request::get("/secret"));
        let missing = routes.handle(&Request::get("/missing"));
        assert_eq!(hidden.status, 404);
        assert_eq!(
            hidden, missing,
            "indistinguishable from a real 404, headers included"
        );
    }

    #[test]
    fn up_answers_ok() {
        let response = Router::new().up("/up").handle(&Request::get("/up"));
        assert_eq!(
            (response.status, response.body),
            (200, br#"{"status":"ok"}"#.to_vec())
        );
    }

    #[test]
    fn json_sets_content_type() {
        let response = json("{}");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
    }
}
