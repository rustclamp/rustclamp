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

mod request;
mod session;
mod throttle;

use std::fs;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use crate::log::Log;

pub use request::{MAX_BODY, Request};
pub use session::{COOKIE, CSRF_FIELD, Session, Sessions, csrf};
pub use throttle::{Throttle, throttle};

/// The web root, relative to the working directory. [`asset`] serves files from it.
pub const PUBLIC: &str = "public";

/// How long the server waits for a client to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The rest of the chain, passed to middleware: call it to continue, or return
/// a response without calling it to stop the request there.
pub type Next<'a> = &'a dyn Fn(&Request) -> Response;

type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;
type Middleware = Arc<dyn Fn(&Request, Next) -> Response + Send + Sync>;

/// The app's routes: exact method and path matches, checked in order.
#[derive(Default)]
pub struct Router {
    routes: Vec<(&'static str, String, Handler)>,
    middleware: Vec<Middleware>,
}

impl Router {
    /// An empty router. Unmatched `GET`s still serve files from [`PUBLIC`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `GET path` with `handler`.
    #[must_use]
    pub fn get(
        self,
        path: &str,
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.route("GET", path, handler)
    }

    /// Answers `POST path` with `handler`.
    #[must_use]
    pub fn post(
        self,
        path: &str,
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.route("POST", path, handler)
    }

    /// Answers `GET path` with the built view `name`, as [`view`] does.
    #[must_use]
    pub fn view(self, path: &str, name: &'static str) -> Self {
        self.get(path, move |_| view(name))
    }

    /// Answers `method path` with `handler`. A `{name}` segment matches any
    /// one segment and is read with [`Request::param`], as in `/blog/{slug}`.
    #[must_use]
    pub fn route(
        mut self,
        method: &'static str,
        path: &str,
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.routes.push((method, path.into(), Box::new(handler)));
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
        run(&self.middleware, request, &|request| self.dispatch(request))
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

/// The view `name` of `package`, with `<!--key-->` markers filled as in
/// [`render`]. The app's built override, `public/build/views/vendor/{package}/{name}.html`
/// (source `app/resources/views/vendor/{package}/{name}.html`), wins over
/// `embedded`, the package's own copy.
pub fn package_view(package: &str, name: &str, embedded: &str, slots: &[(&str, &str)]) -> Response {
    package_view_in(Path::new(PUBLIC), package, name, embedded, slots)
}

fn package_view_in(
    public: &Path,
    package: &str,
    name: &str,
    embedded: &str,
    slots: &[(&str, &str)],
) -> Response {
    let file = public
        .join("build/views/vendor")
        .join(package)
        .join(format!("{name}.html"));
    let mut page = fs::read_to_string(file).unwrap_or_else(|_| embedded.to_owned());
    for (key, value) in slots {
        page = page.replace(&format!("<!--{key}-->"), value);
    }
    html(200, page.into_bytes())
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

/// Serves a built view, `public/build/views/{name}.html`, or `503` if the
/// frontend is not built yet.
pub fn view(name: &str) -> Response {
    let file = Path::new(PUBLIC)
        .join("build/views")
        .join(format!("{name}.html"));
    match fs::read(file) {
        Ok(body) => html(200, body),
        // The app's own error views are not built either, so use the built-in page.
        Err(_) => page(
            503,
            "The frontend is not built yet. If <code>clamp dev</code> is running, refresh in a moment; otherwise run <code>clamp dev</code>.",
        ),
    }
}

/// The built view `name` with each `<!--key-->` marker replaced by its value.
/// Values are inserted as-is: pass text through [`escape`] first. A view that
/// is not built yet (`503`) passes through untouched.
pub fn render(name: &str, slots: &[(&str, &str)]) -> Response {
    let mut response = view(name);
    if response.status != 200 {
        return response;
    }
    let mut page = String::from_utf8_lossy(&response.body).into_owned();
    for (key, value) in slots {
        page = page.replace(&format!("<!--{key}-->"), value);
    }
    response.body = page.into_bytes();
    response
}

/// Escapes text for HTML content and quoted attribute values.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Middleware adding the browser security headers every page should carry:
/// no MIME sniffing, no framing by other sites, no full URLs leaked as referrer.
pub fn security_headers(request: &Request, next: Next) -> Response {
    next(request)
        .with_header("X-Content-Type-Options", "nosniff")
        .with_header("X-Frame-Options", "DENY")
        .with_header("Referrer-Policy", "strict-origin-when-cross-origin")
}

/// An error page for `status`: the app's built view `errors/{status}`, else
/// `errors/{class}xx` (such as `errors/4xx`), else a built-in page. In the
/// app's view, `<!--status-->` and `<!--reason-->` become the code and its
/// reason phrase, so one `4xx` view serves every client error.
pub fn error(status: u16) -> Response {
    for name in [
        format!("errors/{status}"),
        format!("errors/{}xx", status / 100),
    ] {
        let file = Path::new(PUBLIC)
            .join("build/views")
            .join(format!("{name}.html"));
        if let Ok(body) = fs::read_to_string(file) {
            let body = body
                .replace("<!--status-->", &status.to_string())
                .replace("<!--reason-->", reason(status));
            return html(status, body.into_bytes());
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

/// Listens on `127.0.0.1` and answers every request with `routes`.
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
    println!("Serving on http://{address}");
    // ponytail: one request at a time; spawn a thread per stream when apps need concurrency
    for stream in listener.incoming().flatten() {
        respond(stream, &routes);
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
    fn package_view_prefers_the_app_override() {
        let public = std::env::temp_dir().join(format!("clamp-pkg-{}", std::process::id()));
        let body = |response: Response| String::from_utf8(response.body).unwrap();
        let slots = [("title", "Hi")];
        assert_eq!(
            body(package_view_in(
                &public,
                "blog",
                "index",
                "<h1><!--title--></h1>",
                &slots
            )),
            "<h1>Hi</h1>"
        );
        let dir = public.join("build/views/vendor/blog");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.html"), "<h2><!--title--></h2>").unwrap();
        let response = package_view_in(&public, "blog", "index", "<h1><!--title--></h1>", &slots);
        fs::remove_dir_all(&public).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(body(response), "<h2>Hi</h2>");
    }

    #[test]
    fn json_sets_content_type() {
        let response = json("{}");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
    }
}
