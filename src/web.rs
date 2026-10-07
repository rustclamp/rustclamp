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
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::log::Log;

#[cfg(feature = "regex")]
pub use form::Patterns;
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

/// One route: method, path pattern, handler and its own body limit, if any.
struct Route {
    method: &'static str,
    pattern: String,
    handler: Handler,
    body_limit: Option<usize>,
}

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
    routes: Vec<Route>,
    middleware: Vec<Middleware>,
    state: request::State,
    reveal_forbidden: bool,
    json_errors: bool,
    body_limit: Option<usize>,
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

    /// Answers `GET path` with `metrics` in the Prometheus text format, and
    /// counts every other request into it as `http_requests_total`,
    /// `http_server_errors_total` and `http_request_duration_microseconds_total`.
    ///
    /// ```
    /// use rustclamp::metrics::Registry;
    /// use rustclamp::web::{Request, Response, Router};
    /// use std::sync::Arc;
    ///
    /// let metrics = Arc::new(Registry::new());
    /// let app = Router::new()
    ///     .metrics("/metrics", Arc::clone(&metrics))
    ///     .get("/", |_| Response::text(200, "hi"));
    /// app.handle(&Request::get("/"));
    /// metrics.inc("signups_total");
    /// let text = String::from_utf8(app.handle(&Request::get("/metrics")).body).unwrap();
    /// assert!(text.contains("http_requests_total 1\n"));
    /// assert!(text.contains("signups_total 1\n"));
    /// ```
    ///
    /// ponytail: totals only, no per-route labels or buckets; time stops when
    /// the handler returns. Add labels to [`Registry`](crate::metrics::Registry)
    /// when a dashboard needs them.
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn metrics(self, path: &str, metrics: Arc<crate::metrics::Registry>) -> Self {
        let scrape = Arc::clone(&metrics);
        let own = path.to_owned();
        self.middleware(move |request, next| {
            if request.path == own {
                return next(request);
            }
            let started = Instant::now();
            let response = next(request);
            let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            metrics.inc("http_requests_total");
            metrics.add("http_request_duration_microseconds_total", micros);
            if response.status >= 500 {
                metrics.inc("http_server_errors_total");
            }
            response
        })
        .get(path, move |_| {
            Response::new(
                200,
                "text/plain; version=0.0.4; charset=utf-8",
                scrape.render(),
            )
        })
    }

    /// Answers `GET path` with the built view `name`, as [`view`] does.
    #[must_use]
    pub fn view(self, path: &str, name: &'static str) -> Self {
        self.get(path, move |_| view(name))
    }

    /// Answers `method path` with `handler`. A `{name}` segment matches any
    /// one segment and is read with [`Request::param`], as in `/blog/{slug}`.
    /// `{name:u64}` matches only a segment that parses as that type (`u32`,
    /// `u64`, `i32` or `i64`), so `/users/{id:u64}` leaves `/users/me` to the
    /// next route or a `404`; read it with [`Request::param_as`].
    ///
    /// # Panics
    ///
    /// When a `{name:type}` names another type.
    #[must_use]
    pub fn route<R: IntoResponse>(
        mut self,
        method: &'static str,
        path: &str,
        handler: impl Fn(&Request) -> R + Send + Sync + 'static,
    ) -> Self {
        for segment in path.split('/') {
            if let Some((_, kind)) = segment
                .strip_prefix('{')
                .and_then(|name| name.strip_suffix('}'))
                .and_then(|name| name.split_once(':'))
            {
                assert!(
                    matches!(kind, "u32" | "u64" | "i32" | "i64"),
                    "unknown route parameter type `{kind}` in {path}"
                );
            }
        }
        self.routes.push(Route {
            method,
            pattern: path.into(),
            handler: Box::new(move |request: &Request| handler(request).into_response(request)),
            body_limit: None,
        });
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
        let json = group.json_errors;
        for route in group.routes {
            let chain = Arc::clone(&chain);
            let handler = route.handler;
            self.routes.push(Route {
                handler: Box::new(move |request: &Request| {
                    let response = run(&chain, request, &handler);
                    if json {
                        problem_page(response)
                    } else {
                        response
                    }
                }),
                body_limit: route.body_limit.or(group.body_limit),
                ..route
            });
        }
        self
    }

    /// Answers errors as JSON, like [`problem`], instead of error pages: the
    /// router's own `404`, `405`, `413` and `500`, and any error page a
    /// handler returns. Unmatched `GET`s no longer fall back to files in
    /// [`PUBLIC`]. Inside a [`group`](Self::group) it covers that group's
    /// routes only; the router's own answers follow the outer router.
    #[must_use]
    pub fn json_errors(mut self) -> Self {
        self.json_errors = true;
        self
    }

    /// Sets the largest request body, in bytes, for this router, or for the
    /// routes of the [`group`](Self::group) it is set in (a group wins over
    /// the router). Without it the limit is [`MAX_BODY`], or [`MAX_UPLOAD`]
    /// for multipart forms. A larger body is not read: middleware runs, then
    /// the matched route answers `413`, so auth comes first. Middleware and
    /// handlers can ask with [`Request::body_too_large`].
    #[must_use]
    pub fn body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = Some(bytes);
        self
    }

    /// The most bytes of body `request` may send, decided from its route
    /// before the body is read.
    fn limit_for(&self, request: &Request) -> usize {
        let route = self.routes.iter().find(|route| {
            route.method == request.method && params(&route.pattern, &request.path).is_some()
        });
        route
            .and_then(|route| route.body_limit)
            .or(self.body_limit)
            .unwrap_or_else(|| request::default_limit(request))
    }

    /// `response`, as JSON if this router answers errors that way.
    fn finish(&self, response: Response) -> Response {
        if self.json_errors {
            problem_page(response)
        } else {
            response
        }
    }

    /// Answers `403` as `403`. By default a `403` is answered as the `404`
    /// page, so a page's existence is not confirmed to someone not allowed to
    /// see it; an API whose clients already know the resource (bidding on
    /// your own auction) needs the real status. Set it on the outer router:
    /// a [`group`](Self::group)'s routes answer as the router they join.
    #[must_use]
    pub fn reveal_forbidden(mut self) -> Self {
        self.reveal_forbidden = true;
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
    /// for `GET`, else `404`. Log lines written meanwhile carry the request's
    /// [`reference`](Request::reference), and the response carries it in
    /// `X-Request-Id`.
    pub fn handle(&self, request: &Request) -> Response {
        let reference = request.reference();
        crate::log::with_reference(reference, || self.handle_in_scope(request))
            .with_header("x-request-id", &reference.to_string())
    }

    fn handle_in_scope(&self, request: &Request) -> Response {
        let response = if self.state.0.is_empty() {
            run(&self.middleware, request, &|request| self.dispatch(request))
        } else {
            let mut request = request.clone();
            request.state = self.state.clone();
            run(&self.middleware, &request, &|request| {
                self.dispatch(request)
            })
        };
        let response = self.finish(response);
        // A 403 would confirm the page exists to someone not allowed to see
        // it, so it is answered as the 404 page and logged with its real status.
        if response.status == 403 && !self.reveal_forbidden {
            Log::notice(format_args!(
                "http_status_code=403 {} {}",
                request.method, request.path
            ));
            // Keep the middleware's headers (security headers), so the
            // disguised 404 matches a real one down to its headers. Cookies
            // go: a URL that matches no route never runs session middleware,
            // so a session cookie would give the page away. A refused request
            // never moved the session, so nothing is lost.
            let mut hidden = self.finish(error(404));
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
        let matched = self.routes.iter().find_map(|route| {
            let params = (route.method == request.method)
                .then(|| params(&route.pattern, &request.path))??;
            Some((&route.handler, params))
        });
        match matched {
            // The body was not read: the middleware has run, now refuse it.
            Some(_) if request.body_too_large() => error(413),
            Some((handler, params)) if params.is_empty() => handler(request),
            Some((handler, params)) => {
                let mut request = request.clone();
                request.params = params;
                handler(&request)
            }
            None => self.unmatched(request),
        }
    }

    /// No route has this method and path: a static file for `GET`, `405` with
    /// `Allow` when the path exists under other methods, else `404`.
    fn unmatched(&self, request: &Request) -> Response {
        let mut allow: Vec<&str> = self
            .routes
            .iter()
            .filter(|route| params(&route.pattern, &request.path).is_some())
            .map(|route| route.method)
            .collect();
        allow.sort_unstable();
        allow.dedup();
        if request.method == "GET" && !self.json_errors {
            let file = asset(&request.path);
            if file.status != 404 || allow.is_empty() {
                return file;
            }
        }
        if allow.is_empty() {
            error(404)
        } else {
            error(405).with_header("Allow", &allow.join(", "))
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
    rendered(public, &full, &source, Value::map(data))
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
            Some(name) if !segment.is_empty() => {
                let (name, kind) = name.split_once(':').unwrap_or((name, ""));
                let fits = match kind {
                    "u32" => segment.parse::<u32>().is_ok(),
                    "u64" => segment.parse::<u64>().is_ok(),
                    "i32" => segment.parse::<i32>().is_ok(),
                    "i64" => segment.parse::<i64>().is_ok(),
                    _ => true,
                };
                if !fits {
                    return None;
                }
                params.push((name.to_owned(), segment.to_owned()));
            }
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
    json_status(200, body)
}

/// A JSON response with `status` and a pre-serialized body, such as `201`
/// for a created resource.
pub fn json_status(status: u16, body: &str) -> Response {
    Response::new(status, "application/json", body)
}

/// An RFC 9457 `application/problem+json` error for `status`, in the shape
/// of `rustclamp-http`'s `HttpError`: `type`, `title` (the [`reason`]),
/// `status` and `detail`.
pub fn problem(status: u16, detail: &str) -> Response {
    let body = crate::error::Problem::new(status, reason(status), detail).json();
    Response::new(status, "application/problem+json", body)
}

/// `response` as [`problem`] JSON when it is a built-in error page, keeping
/// its headers; anything else is left alone.
fn problem_page(response: Response) -> Response {
    if response.status < 400 || !response.content_type.starts_with("text/html") {
        return response;
    }
    let mut json = problem(response.status, reason(response.status));
    json.headers = response.headers;
    json
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
    render_value(name, Value::map(data))
}

fn render_value(name: &str, data: Value) -> Response {
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
fn rendered(public: &Path, name: &str, source: &str, data: Value) -> Response {
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
    let mut out = String::with_capacity(text.len());
    escape_into(text, &mut out);
    out
}

/// [`escape`], appending to `out`.
fn escape_into(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(at) = rest.find(['&', '<', '>', '"', '\'']) {
        out.push_str(&rest[..at]);
        out.push_str(match rest.as_bytes()[at] {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => "&#39;",
        });
        rest = &rest[at + 1..];
    }
    out.push_str(rest);
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
///
/// While a request is handled, `{{ reference }}` is its
/// [`reference`](Request::reference), and the built-in page shows it for
/// `5xx`, so a visitor can quote it and the log lines can be found.
pub fn error(status: u16) -> Response {
    let public = Path::new(PUBLIC);
    let reference = crate::log::reference().map(|reference| reference.to_string());
    let data = Value::map(&[
        ("status", &status),
        ("reason", &reason(status)),
        ("reference", &reference.clone().unwrap_or_default()),
    ]);
    for name in [
        format!("errors/{status}"),
        format!("errors/{}xx", status / 100),
    ] {
        if let Some(source) = built_view(public, &name) {
            match view::render(&name, &source, data.clone(), &|name| {
                built_view(public, name)
            }) {
                Ok(body) => return html(status, body.into_bytes()),
                Err(problem) => {
                    Log::error(format_args!("{problem}"));
                    break;
                }
            }
        }
    }
    match reference {
        Some(reference) if status >= 500 => {
            page(status, &format!("Reference: <code>{reference}</code>"))
        }
        _ => page(status, ""),
    }
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

/// The reason phrase of `status`, such as `Not Found` for `404`.
pub fn reason(status: u16) -> &'static str {
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
    Response::new(200, content_type(&file), body)
}

/// The `Content-Type` for a file served by [`asset`], by extension. Covers
/// every image the `image` upload rule accepts: with `nosniff` on every
/// response, a browser will not guess a type that is missing here.
fn content_type(file: &Path) -> &'static str {
    let extension = file
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
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
///         # #[cfg(feature = "queue")]
///         # jobs: Vec::new,
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
    /// The job handlers by name (ADR 0019), such as
    /// `|| vec![("thumbnail", jobs::thumbnail)]`.
    #[cfg(feature = "queue")]
    pub jobs: fn() -> Vec<(&'static str, crate::queue::Handler)>,
}

#[cfg(feature = "db")]
impl App {
    /// Loads `.env`, installs the logger, then either runs a database command
    /// given as the first argument (`migrate`, `migrate:rollback`, `db:seed`)
    /// and exits with its status, or migrates, links `public/storage` to the
    /// public disk (like `php artisan storage:link`; a failure is a warning),
    /// starts `QUEUE_WORKERS` queue workers with the `queue` feature and
    /// serves the routes. With the first argument `queue:work`, it runs only
    /// the queue workers (at least one).
    ///
    /// # Panics
    ///
    /// When a migration fails: the app must not serve an old schema. When
    /// the queue settings cannot work ([`Queue::new`](crate::queue::Queue::new)).
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
        #[cfg(feature = "queue")]
        if std::env::args().nth(1).as_deref() == Some("queue:work") {
            let workers = crate::queue::workers(&config).max(1);
            println!("Working the queue on {workers} threads");
            for worker in self.queue(&config, db).start(workers, &Shutdown::new()) {
                let _ = worker.join();
            }
            std::process::exit(0);
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
        // ponytail: nothing stops the workers; a killed job runs again after
        // QUEUE_RETRY_AFTER. Stop this Shutdown from a signal hook if one lands.
        #[cfg(feature = "queue")]
        self.queue(&config, db.clone())
            .start(crate::queue::workers(&config), &Shutdown::new());
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
        #[cfg(feature = "queue")]
        let router = router.state(self.queue(config, db.clone()));
        (self.routes)(router, config, &db)
    }

    /// The app's queue on `db`, with its job handlers. Tests run jobs with
    /// [`Queue::work_once`](crate::queue::Queue::work_once) on a queue built
    /// over the database [`App::test`] returns.
    ///
    /// # Panics
    ///
    /// When the queue settings cannot work, such as Redis without
    /// `REDIS_PREFIX` in production: the app refuses to start.
    #[cfg(feature = "queue")]
    pub fn queue(&self, config: &crate::config::Config, db: crate::db::Db) -> crate::queue::Queue {
        crate::queue::Queue::new(config, db, (self.jobs)())
            .unwrap_or_else(|error| panic!("{error}"))
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
/// any free port. See [`try_serve`] to handle a bad setting or a busy port
/// yourself.
///
/// # Panics
///
/// When `PORT` or `WEB_THREADS` is not a number. Exits the process with code 1
/// when `PORT` is set but unavailable.
pub fn serve(routes: Router) {
    match try_serve(routes) {
        Ok(()) => {}
        Err(ServeError::Config(error)) => panic!("{error}"),
        Err(error @ ServeError::Bind { .. }) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

/// [`serve`], but a bad setting or a busy port comes back as an error, so a
/// service can exit with its own code. Returns only on an error.
///
/// # Errors
///
/// [`ServeError::Config`] when `PORT` or `WEB_THREADS` is set but is not a
/// number, [`ServeError::Bind`] when no port could be bound.
pub fn try_serve(routes: Router) -> std::result::Result<(), ServeError> {
    // The process environment only, as before: `.env` never picked the port.
    let (listener, threads) = bind(&crate::config::Config::from_env())?;
    let address = listener.local_addr().expect("listener has an address");
    println!("Serving on http://{address}");
    serve_on(listener, routes, threads);
    Ok(())
}

/// The listener and thread count `PORT` and `WEB_THREADS` in `config` ask for.
fn bind(config: &crate::config::Config) -> std::result::Result<(TcpListener, usize), ServeError> {
    let ports: Vec<u16> = match config.get_parsed("PORT").map_err(ServeError::Config)? {
        Some(port) => vec![port],
        None => (8080..8100).chain([0]).collect(),
    };
    let threads = config
        .get_parsed("WEB_THREADS")
        .map_err(ServeError::Config)?
        .unwrap_or(THREADS);
    let mut last = None;
    let listener = ports
        .iter()
        .find_map(|port| {
            TcpListener::bind(("127.0.0.1", *port))
                .map_err(|error| last = Some(error))
                .ok()
        })
        .ok_or_else(|| ServeError::Bind {
            port: ports[0],
            source: last.unwrap_or_else(|| std::io::ErrorKind::AddrInUse.into()),
        })?;
    Ok((listener, threads))
}

/// Why [`try_serve`] could not start.
#[derive(Debug)]
pub enum ServeError {
    /// `PORT` or `WEB_THREADS` is set but is not a number.
    Config(crate::config::ConfigError),
    /// No port could be bound: `port` is `PORT`, or 8080 when unset.
    Bind {
        /// The port asked for first.
        port: u16,
        /// Why the last bind failed.
        source: std::io::Error,
    },
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Bind { port, source } => {
                write!(
                    f,
                    "Port {port} is in use; set PORT to another one ({source})."
                )
            }
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Bind { source, .. } => Some(source),
        }
    }
}

/// Answers connections from `listener`, bound by the app itself, on
/// `threads` worker threads, until the listener fails ([`serve_until`] stops
/// and drains on request). Accepted
/// connections wait in a queue of four per thread; when it is full, the
/// operating system's backlog holds the rest, so a flood cannot spawn threads
/// or grow memory without bound.
///
/// Connections stay open between requests (HTTP/1.1 keep-alive) for up to
/// 5 idle seconds and 1,000 requests. A thread never sits on an idle
/// connection while others wait: it puts it back in the queue, and after each
/// response a connection goes to the back when others are waiting, so every
/// connection takes turns.
// ponytail: a fixed pool of blocking threads; each slow client holds one for up
// to READ_TIMEOUT, so size WEB_THREADS above the slow clients you expect, or
// put nginx in front (it buffers requests), before reaching for async.
// Memory: each thread holds one parsed request, so the worst case is
// WEB_THREADS x the largest body (10 MiB uploads: 32 x 10 MiB = 320 MiB).
pub fn serve_on(listener: TcpListener, routes: Router, threads: usize) {
    serve_until(listener, routes, threads, &Shutdown::new(), Duration::ZERO);
}

/// Stops [`serve_until`]: clone it into a signal handler or another thread
/// and call [`stop`](Shutdown::stop).
#[derive(Clone, Debug, Default)]
pub struct Shutdown(Arc<ShutdownState>);

#[derive(Debug, Default)]
struct ShutdownState {
    stopped: AtomicBool,
    /// Where `serve_until` listens, so `stop` can wake its blocked accept.
    address: OnceLock<SocketAddr>,
}

impl Shutdown {
    /// A handle that has not been stopped.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops accepting connections and starts the drain. Calling it again,
    /// or before `serve_until` starts, is fine.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        if let Some(address) = self.0.address.get() {
            let _ = TcpStream::connect_timeout(address, Duration::from_secs(1));
        }
    }

    /// Whether [`stop`](Shutdown::stop) was called.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }
}

/// [`serve_on`] until `shutdown` is stopped, then drains: no new connections,
/// requests already sent are answered with `Connection: close`, idle
/// kept-alive connections are closed. Waits up to `grace` for the last open
/// connection to close and returns whether every one did.
// ponytail: after the drain the worker threads stay parked on the empty
// queue; the process exiting reclaims them. Join them if a server must
// restart in-process many times.
pub fn serve_until(
    listener: TcpListener,
    routes: Router,
    threads: usize,
    shutdown: &Shutdown,
    grace: Duration,
) -> bool {
    if let Ok(mut address) = listener.local_addr() {
        // A listener on 0.0.0.0 or [::] is reached through loopback.
        if address.ip().is_unspecified() {
            address.set_ip(match address.ip() {
                IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
                IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
            });
        }
        let _ = shutdown.0.address.set(address);
    }
    let threads = threads.max(1);
    let routes = Arc::new(routes);
    let (send, receive) = std::sync::mpsc::sync_channel::<Connection>(threads * 4);
    let queue = Arc::new(Queue {
        send,
        waiting: AtomicUsize::new(0),
        open: Arc::new(AtomicUsize::new(0)),
        shutdown: shutdown.clone(),
    });
    let receive = Arc::new(std::sync::Mutex::new(receive));
    for _ in 0..threads {
        let (routes, queue, receive) = (
            Arc::clone(&routes),
            Arc::clone(&queue),
            Arc::clone(&receive),
        );
        std::thread::spawn(move || {
            loop {
                // The lock is held only while taking the next connection.
                let next = receive
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv();
                let Ok(connection) = next else { return };
                queue.waiting.fetch_sub(1, Ordering::Relaxed);
                // A panic outside the handler (writing the response) must not
                // shrink the pool.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    serve_connection(connection, &routes, &queue);
                }));
            }
        });
    }
    for stream in listener.incoming() {
        // Checked after accept too: `stop` wakes it with a connection of its own.
        if shutdown.is_stopped() {
            break;
        }
        let Ok(stream) = stream else { continue };
        // Head and body go out as two writes; without this the body waits
        // for the client's delayed ACK on a kept-alive connection.
        let _ = stream.set_nodelay(true);
        queue.waiting.fetch_add(1, Ordering::Relaxed);
        queue.open.fetch_add(1, Ordering::SeqCst);
        let connection = Connection {
            reader: BufReader::new(stream),
            idle_since: Instant::now(),
            served: 0,
            _open: Open(Arc::clone(&queue.open)),
        };
        if queue.send.send(connection).is_err() {
            return false;
        }
    }
    drop(listener);
    let deadline = Instant::now() + grace;
    while queue.open.load(Ordering::SeqCst) > 0 {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// How long a connection may wait for its next request before it is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests answered on one connection before it is closed.
const MAX_REQUESTS: usize = 1000;
/// How often a thread waiting on an idle connection checks for queued ones.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// An accepted connection, with anything the client sent ahead.
struct Connection {
    reader: BufReader<TcpStream>,
    idle_since: Instant,
    served: usize,
    _open: Open,
}

/// Counts a connection as open until it is dropped, however it closes.
struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Connections waiting for a thread, and how many.
struct Queue {
    send: SyncSender<Connection>,
    waiting: AtomicUsize,
    /// Accepted connections not yet closed, for the drain.
    open: Arc<AtomicUsize>,
    shutdown: Shutdown,
}

impl Queue {
    /// Puts `connection` at the back of the queue, or closes it when the
    /// queue is full.
    fn requeue(&self, connection: Connection) {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        if self.send.try_send(connection).is_err() {
            self.waiting.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn others_waiting(&self) -> bool {
        self.waiting.load(Ordering::Relaxed) > 0
    }
}

/// What a connection did while its thread waited for the next request.
enum Wait {
    Ready,
    Closed,
    Yield,
}

/// Answers requests on `connection` until it closes, idles out, or gives its
/// thread to a queued connection.
fn serve_connection(mut connection: Connection, routes: &Router, queue: &Queue) {
    loop {
        match next_request(&mut connection, queue) {
            Wait::Ready => {}
            Wait::Closed => return,
            Wait::Yield => return queue.requeue(connection),
        }
        connection.served += 1;
        if !respond(&mut connection, routes, &queue.shutdown) {
            return;
        }
        connection.idle_since = Instant::now();
        if queue.others_waiting() {
            return queue.requeue(connection);
        }
    }
}

/// Waits until the client starts its next request, it hangs up or idles out,
/// or another connection is waiting for a thread.
fn next_request(connection: &mut Connection, queue: &Queue) -> Wait {
    let _ = connection
        .reader
        .get_ref()
        .set_read_timeout(Some(IDLE_POLL));
    loop {
        match connection.reader.fill_buf() {
            Ok([]) => return Wait::Closed,
            Ok(_) => return Wait::Ready,
            Err(problem)
                if matches!(
                    problem.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Draining: a request that already arrived is answered above.
                if connection.idle_since.elapsed() >= IDLE_TIMEOUT || queue.shutdown.is_stopped() {
                    return Wait::Closed;
                }
                if queue.others_waiting() {
                    return Wait::Yield;
                }
            }
            Err(problem) if problem.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Wait::Closed,
        }
    }
}

/// Reads one request from `connection` and writes its response. Returns
/// whether the connection stays open for another request; never once
/// `shutdown` is stopped.
fn respond(connection: &mut Connection, routes: &Router, shutdown: &Shutdown) -> bool {
    let stream = connection.reader.get_ref();
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let peer = stream.peer_addr().ok().map(|address| address.ip());
    let parsed = request::parse(&mut connection.reader, peer, &|request| {
        routes.limit_for(request)
    });
    let last = connection.served >= MAX_REQUESTS;
    let (response, keep, head) = match parsed {
        // A panicking handler answers 500 instead of stopping the server.
        Ok(request) => {
            let response = catch_unwind(AssertUnwindSafe(|| routes.handle(&request)))
                .unwrap_or_else(|panic| {
                    let reason = panic
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    let reference = request.reference();
                    crate::log::with_reference(reference, || {
                        Log::error(format_args!(
                            "{} {} panicked: {reason}",
                            request.method, request.path
                        ));
                        routes
                            .finish(error(500))
                            .with_header("x-request-id", &reference.to_string())
                    })
                });
            (
                response,
                // Checked after the handler: a stop during a slow request still says close.
                request.keep_alive && !last && !shutdown.is_stopped(),
                request.method == "HEAD",
            )
        }
        // The rest of a request that failed to parse cannot be found.
        Err(response) => (routes.finish(response), false, false),
    };
    let mut stream = connection.reader.get_ref();
    let mut text = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len(),
        if keep { "" } else { "Connection: close\r\n" }
    );
    for (name, value) in &response.headers {
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    // A HEAD response says how long the body is but leaves it out, or the
    // client would read it as the start of the next response.
    let written = stream.write_all(text.as_bytes()).and_then(|()| {
        if head {
            Ok(())
        } else {
            stream.write_all(&response.body)
        }
    });
    keep && written.is_ok()
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
                .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
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
    fn connections_stay_open_and_idle_ones_make_way() {
        use std::io::Read;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new().get("/hi", |_| Response::text(200, "hi"));
        // One thread: an idle kept-alive connection would block everyone else.
        std::thread::spawn(move || serve_on(listener, routes, 1));

        // Two requests, then a HEAD without a body, then close, on one connection.
        let mut open = TcpStream::connect(address).unwrap();
        open.write_all(b"GET /hi HTTP/1.1\r\n\r\nGET /hi HTTP/1.1\r\n\r\n")
            .unwrap();
        open.write_all(b"HEAD /hi HTTP/1.1\r\n\r\nGET /hi HTTP/1.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut answers = String::new();
        open.read_to_string(&mut answers).unwrap();
        assert_eq!(answers.matches("HTTP/1.1 200 OK").count(), 3, "{answers}");
        assert_eq!(answers.matches("Connection: close").count(), 1, "{answers}");
        let last = &answers[answers.rfind("HTTP/1.1").unwrap()..];
        assert!(last.contains("Connection: close\r\n"), "{answers}");
        assert!(last.ends_with("\r\n\r\nhi"), "{answers}");
        // HEAD was answered with 405 and no body, so the next response parsed.
        assert!(answers.contains("405 Method Not Allowed"), "{answers}");
        assert!(!answers.contains('<'), "HEAD sent a body: {answers}");

        // An idle kept-alive connection gives the only thread to a new one.
        let mut idle = TcpStream::connect(address).unwrap();
        idle.write_all(b"GET /hi HTTP/1.1\r\n\r\n").unwrap();
        let mut first = [0; 16];
        idle.read_exact(&mut first).unwrap();
        let started = Instant::now();
        let mut other = TcpStream::connect(address).unwrap();
        other
            .write_all(b"GET /hi HTTP/1.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        other.read_to_string(&mut answer).unwrap();
        assert!(answer.ends_with("hi"), "{answer}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn stop_finishes_requests_in_flight_closes_idle_ones_and_returns() {
        use std::io::Read;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new()
            .get("/hi", |_| Response::text(200, "hi"))
            .get("/slow", |_| {
                std::thread::sleep(Duration::from_millis(300));
                Response::text(200, "done")
            });
        let shutdown = Shutdown::new();
        let handle = shutdown.clone();
        let server = std::thread::spawn(move || {
            serve_until(listener, routes, 4, &handle, Duration::from_secs(5))
        });

        // A kept-alive connection, idle after its first answer.
        let mut idle = TcpStream::connect(address).unwrap();
        idle.write_all(b"GET /hi HTTP/1.1\r\n\r\n").unwrap();
        let mut first = [0; 16];
        idle.read_exact(&mut first).unwrap();
        // A request still running when the stop comes.
        let mut slow = TcpStream::connect(address).unwrap();
        slow.write_all(b"GET /slow HTTP/1.1\r\n\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let stopped = Instant::now();
        shutdown.stop();
        let mut answer = String::new();
        slow.read_to_string(&mut answer).unwrap();
        assert!(answer.contains("Connection: close\r\n"), "{answer}");
        assert!(answer.ends_with("done"), "{answer}");
        let mut rest = Vec::new();
        idle.read_to_end(&mut rest).unwrap();
        assert!(
            server.join().unwrap(),
            "connections left open after the drain"
        );
        // Idle connections close on stop, not after the 5 s idle timeout.
        assert!(
            stopped.elapsed() < Duration::from_secs(2),
            "{:?}",
            stopped.elapsed()
        );
        assert!(TcpStream::connect(address).is_err(), "still accepting");
    }

    #[test]
    fn requests_carry_a_reference_into_logs_headers_and_error_pages() {
        let routes = Router::new()
            .get("/seen", |request| {
                // Everything logged while handling carries this reference.
                assert_eq!(crate::log::reference(), Some(request.reference()));
                Response::text(200, "ok")
            })
            .get("/broken", |_| error(500));
        let request = Request::get("/seen");
        let response = routes.handle(&request);
        assert_eq!(
            response.header("x-request-id"),
            Some(request.reference().to_string().as_str())
        );
        assert_eq!(
            crate::log::reference(),
            None,
            "the scope ends with the request"
        );
        assert_ne!(Request::get("/seen").reference(), request.reference());
        assert_eq!(
            Request::get("/seen"),
            request,
            "requests compare by what was sent"
        );

        let request = Request::get("/broken");
        let page = String::from_utf8(routes.handle(&request).body).unwrap();
        assert!(page.contains(&format!("Reference: <code>{}</code>", request.reference())));
        let outside = String::from_utf8(error(500).body).unwrap();
        assert!(!outside.contains("Reference:"));
    }

    #[test]
    fn asset_refuses_paths_outside_dist() {
        assert_eq!(asset("/../Cargo.toml").status, 404);
        assert_eq!(asset("/assets/../../Cargo.toml").status, 404);
        assert_eq!(asset("//etc/passwd").status, 404);
    }

    #[test]
    fn asset_types_cover_every_image_upload() {
        for (file, expected) in [
            ("a.jpg", "image/jpeg"),
            ("a.JPEG", "image/jpeg"),
            ("a.png", "image/png"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.pdf", "application/pdf"),
            ("a", "application/octet-stream"),
        ] {
            assert_eq!(content_type(Path::new(file)), expected, "{file}");
        }
    }

    #[test]
    fn router_matches_method_and_path_then_falls_back() {
        let routes = Router::new()
            .get("/hello", |_| json("{}"))
            .post("/hello", |_| Response::text(201, "created"));
        assert_eq!(routes.handle(&Request::get("/hello")).status, 200);
        assert_eq!(routes.handle(&Request::post("/hello")).status, 201);
        assert_eq!(routes.handle(&Request::get("/missing.css")).status, 404);
        assert_eq!(routes.handle(&Request::new("DELETE", "/hello")).status, 405);
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
        let without_reference = |mut response: Response| {
            assert!(response.header("x-request-id").is_some());
            response.headers.retain(|(name, _)| name != "x-request-id");
            response
        };
        let hidden = without_reference(routes.handle(&Request::get("/secret")));
        let missing = without_reference(routes.handle(&Request::get("/missing")));
        assert_eq!(hidden.status, 404);
        assert_eq!(
            hidden, missing,
            "indistinguishable from a real 404, headers included"
        );
    }

    #[test]
    fn forbidden_can_be_revealed() {
        let routes = Router::new()
            .reveal_forbidden()
            .get("/own", |_| Response::text(403, "own_listing"));
        let response = routes.handle(&Request::get("/own"));
        assert_eq!(response.status, 403);
        assert_eq!(response.body, b"own_listing");
    }

    #[test]
    fn bad_settings_and_busy_ports_are_errors() {
        use crate::config::Config;
        assert!(matches!(
            bind(&Config::parse("PORT=eighty")),
            Err(ServeError::Config(_))
        ));
        assert!(matches!(
            bind(&Config::parse("PORT=0\nWEB_THREADS=many")),
            Err(ServeError::Config(_))
        ));
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let busy = bind(&Config::parse(&format!("PORT={port}"))).unwrap_err();
        assert!(matches!(busy, ServeError::Bind { port: p, .. } if p == port));
        let (_listener, threads) = bind(&Config::parse("PORT=0\nWEB_THREADS=3")).unwrap();
        assert_eq!(threads, 3);
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
    fn json_status_and_public_reason() {
        let created = json_status(201, "{}");
        assert_eq!(
            (created.status, created.content_type),
            (201, "application/json")
        );
        assert_eq!(reason(404), "Not Found");
    }

    #[test]
    fn a_path_under_other_methods_is_405_with_allow() {
        let routes = Router::new()
            .get("/items", |_| Response::text(200, "list"))
            .post("/items", |_| Response::text(201, "made"))
            .get("/items/{id}", |_| Response::text(200, "one"));
        let response = routes.handle(&Request::new("DELETE", "/items"));
        assert_eq!(response.status, 405);
        assert_eq!(response.header("allow"), Some("GET, POST"));
        let response = routes.handle(&Request::post("/items/3"));
        assert_eq!(
            (response.status, response.header("allow")),
            (405, Some("GET"))
        );
        assert_eq!(routes.handle(&Request::post("/nope")).status, 404);
        assert_eq!(routes.handle(&Request::get("/nope")).status, 404);
    }

    #[test]
    fn json_errors_render_problem_json() {
        let routes = Router::new()
            .json_errors()
            .get("/boom", |_| -> Result { Err("secret detail".into()) })
            .get("/plain", |_| Response::text(404, "mine"))
            .post("/only", |_| Response::text(200, "ok"));
        let check = |response: Response, status: u16, title: &str| {
            assert_eq!(response.status, status);
            assert_eq!(response.content_type, "application/problem+json");
            assert_eq!(
                response.body_text(),
                format!(
                    r#"{{"type":"about:blank","title":"{title}","status":{status},"detail":"{title}"}}"#
                )
            );
        };
        check(routes.handle(&Request::get("/missing")), 404, "Not Found");
        let response = routes.handle(&Request::get("/only"));
        assert_eq!(response.header("allow"), Some("POST"));
        check(response, 405, "Method Not Allowed");
        check(
            routes.handle(&Request::get("/boom")),
            500,
            "Internal Server Error",
        );
        // A handler's own non-page answer is left alone.
        assert_eq!(routes.handle(&Request::get("/plain")).body, b"mine");
        // A 403 is still disguised as a 404, in JSON.
        let hidden = Router::new()
            .json_errors()
            .get("/x", |_| Response::text(403, ""))
            .handle(&Request::get("/x"));
        assert_eq!(hidden.status, 404);
        assert_eq!(hidden.content_type, "application/problem+json");
    }

    #[test]
    fn json_errors_can_cover_one_group() {
        let routes = Router::new()
            .group(|api| {
                api.json_errors()
                    .get("/api/boom", |_| -> Result { Err("x".into()) })
            })
            .get("/boom", |_| -> Result { Err("x".into()) });
        assert_eq!(
            routes.handle(&Request::get("/api/boom")).content_type,
            "application/problem+json"
        );
        assert_eq!(
            routes.handle(&Request::get("/boom")).content_type,
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn problem_escapes_its_detail() {
        assert_eq!(
            problem(400, "a \"b\"\n").body_text(),
            r#"{"type":"about:blank","title":"Bad Request","status":400,"detail":"a \"b\"\u000a"}"#
        );
    }

    #[test]
    fn body_limits_come_from_the_group_or_router() {
        let ok = |_: &Request| Response::text(200, "ok");
        let routes = Router::new()
            .body_limit(100)
            .post("/router", ok)
            .group(|small| small.body_limit(10).post("/group", ok))
            .group(|inherits| inherits.post("/inherits", ok));
        let limit = |path: &str| routes.limit_for(&Request::post(path));
        assert_eq!(
            (limit("/router"), limit("/group"), limit("/inherits")),
            (100, 10, 100)
        );
        assert_eq!(limit("/unknown"), 100);
        assert_eq!(Router::new().limit_for(&Request::post("/")), MAX_BODY);
    }

    #[test]
    fn an_oversized_body_is_refused_after_auth_and_never_read() {
        use std::io::Read;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new()
            .json_errors()
            .body_limit(16)
            .middleware(|request, next| {
                if request.header("authorization").is_some() {
                    next(request)
                } else {
                    problem(401, "sign in")
                }
            })
            .post("/upload", |_| Response::text(200, "stored"));
        std::thread::spawn(move || serve_on(listener, routes, 2));
        // No body is ever sent: a server that tried to read it would wait
        // for the read timeout instead of answering.
        let send = |head: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            let started = Instant::now();
            let request = format!("POST /upload HTTP/1.1\r\n{head}Content-Length: 9999\r\n\r\n");
            stream.write_all(request.as_bytes()).unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            assert!(started.elapsed() < Duration::from_secs(5), "{answer}");
            answer
        };
        assert!(send("").starts_with("HTTP/1.1 401"));
        let refused = send("Authorization: Bearer x\r\n");
        assert!(
            refused.starts_with("HTTP/1.1 413 Content Too Large"),
            "{refused}"
        );
        assert!(refused.contains("application/problem+json"), "{refused}");
    }

    #[test]
    fn typed_params_only_match_what_parses() {
        let routes = Router::new()
            .get("/users/me", |_| Response::text(200, "me"))
            .get("/users/{id:u64}", |request| {
                let next = request.param_as::<u64>("id").unwrap() + 1;
                Response::text(200, &next.to_string())
            })
            .get("/tags/{name}", |request| {
                Response::text(200, &format!("{:?}", request.param_as::<u8>("name")))
            });
        let body = |path: &str| routes.handle(&Request::get(path)).body_text().into_owned();
        assert_eq!(body("/users/41"), "42");
        assert_eq!(body("/users/me"), "me");
        assert_eq!(routes.handle(&Request::get("/users/-1")).status, 404);
        assert_eq!(routes.handle(&Request::get("/users/x")).status, 404);
        assert_eq!(body("/tags/7"), "Some(7)");
        assert_eq!(body("/tags/seven"), "None");
    }

    #[test]
    #[should_panic(expected = "unknown route parameter type")]
    fn an_unknown_param_type_is_refused_when_declared() {
        let _ = Router::new().get("/a/{id:uuid}", |_| Response::text(200, ""));
    }

    #[test]
    fn extensions_carry_typed_values_from_middleware_to_handlers() {
        struct Principal(&'static str);
        let routes = Router::new()
            .middleware(|request, next| {
                let request = request.clone().with_extension(Principal("ann"));
                next(
                    &request
                        .with_extension(Principal("bob"))
                        .with_extension(7_u8),
                )
            })
            .get("/me", |request| {
                let who = request.extension::<Principal>().map_or("nobody", |p| p.0);
                Response::text(200, &format!("{who} {:?}", request.extension::<u8>()))
            });
        assert_eq!(routes.handle(&Request::get("/me")).body, b"bob Some(7)");
        assert_eq!(Request::get("/").extension::<u8>(), None);
    }

    #[test]
    fn json_sets_content_type() {
        let response = json("{}");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
    }
}
