//! A small, synchronous HTTP server for Clamp web applications.
//!
//! Enabled by the `web` feature. It uses only the standard library: the app
//! supplies one routing function and [`serve`] handles ports, request parsing,
//! responses and static files. `public/` is the web root; Vite builds views and
//! assets into `public/build/`.
//!
//! ```no_run
//! use rustclamp::web::{self, Request, Response};
//!
//! fn routes(request: &Request) -> Response {
//!     match (request.method.as_str(), request.path.as_str()) {
//!         ("GET", "/") => web::view("welcome"),
//!         ("GET", "/api/health") => web::json(r#"{"status":"ok"}"#),
//!         ("GET", _) => web::asset(&request.path),
//!         _ => Response::text(404, "Not found"),
//!     }
//! }
//!
//! web::serve(routes);
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path};

/// The web root, relative to the working directory. [`asset`] serves files from it.
pub const PUBLIC: &str = "public";

/// An incoming HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Request {
    /// The request method, such as `GET`.
    pub method: String,
    /// The request path without its query string or fragment.
    pub path: String,
}

impl Request {
    /// Creates a request, for example to call routes from a test.
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
        }
    }
}

/// An HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The `Content-Type` header value.
    pub content_type: &'static str,
    /// The response body.
    pub body: Vec<u8>,
}

impl Response {
    /// A plain-text response with the given status.
    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
        }
    }
}

/// A `200` JSON response with a pre-serialized body.
pub fn json(body: &str) -> Response {
    Response {
        status: 200,
        content_type: "application/json",
        body: body.into(),
    }
}

/// Serves a built view, `public/build/views/{name}.html`, or `503` if the
/// frontend is not built yet.
pub fn view(name: &str) -> Response {
    let file = Path::new(PUBLIC)
        .join("build/views")
        .join(format!("{name}.html"));
    match fs::read(file) {
        Ok(body) => Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body,
        },
        Err(_) => Response::text(
            503,
            "Frontend is not built yet. If `clamp dev` is running, refresh in a moment; otherwise run `clamp dev`.",
        ),
    }
}

/// Serves a static file from [`PUBLIC`], refusing any path that could leave it.
pub fn asset(path: &str) -> Response {
    let relative = Path::new(path.trim_start_matches('/'));
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Response::text(404, "Not found");
    }
    let file = Path::new(PUBLIC).join(relative);
    let Ok(body) = fs::read(&file) else {
        return Response::text(404, "Not found");
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
    Response {
        status: 200,
        content_type,
        body,
    }
}

/// Listens on `127.0.0.1` and answers every request with `routes`.
///
/// Uses `PORT` when set; otherwise the first free port from 8080 to 8099, then
/// any free port. Exits the process if `PORT` is set but unavailable.
pub fn serve(routes: impl Fn(&Request) -> Response) {
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

fn respond(mut stream: TcpStream, routes: &impl Fn(&Request) -> Response) {
    let mut request_line = String::new();
    let _ = BufReader::new(&stream).read_line(&mut request_line);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let response = routes(&Request::new(method, path));
    let reason = if response.status == 200 {
        "OK"
    } else {
        "Error"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
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
    fn json_sets_content_type() {
        let response = json("{}");
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
    }
}
