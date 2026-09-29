//! Serve the Python-built RustClamp website through Clamp's HTTP target.

use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustclamp_core::ModuleId;
use rustclamp_http::ecosystem::body::Body;
use rustclamp_http::ecosystem::extract::{OriginalUri, State};
use rustclamp_http::ecosystem::http::{HeaderValue, StatusCode, header};
use rustclamp_http::ecosystem::response::Response;
use rustclamp_http::ecosystem::routing::get;
use rustclamp_http::{HttpRoute, HttpRoutes, Public};
use rustclamp_kernel::{TargetComposition, TargetCompositionError};

async fn serve_file(State(root): State<Arc<PathBuf>>, uri: OriginalUri) -> Response<Body> {
    let requested = uri.0.path();
    let (path, body, status) = read_page(&root, requested);
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type(&path))
        .body(Body::from(body))
        .expect("static response headers are valid")
}

fn read_page(root: &Path, requested: &str) -> (PathBuf, Vec<u8>, StatusCode) {
    let Some(relative) = requested.strip_prefix('/') else {
        return not_found(root);
    };
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let mut candidate = PathBuf::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => candidate.push(part),
            _ => return not_found(root),
        }
    }
    if candidate.extension().is_none() {
        candidate.set_extension("html");
    }
    let Ok(page) = fs::canonicalize(root.join(candidate)) else {
        return not_found(root);
    };
    if !page.starts_with(root) || !page.is_file() {
        return not_found(root);
    }
    match fs::read(&page) {
        Ok(body) => (page, body, StatusCode::OK),
        Err(_) => not_found(root),
    }
}

fn not_found(root: &Path) -> (PathBuf, Vec<u8>, StatusCode) {
    let page = root.join("404.html");
    let body = fs::read(&page).unwrap_or_default();
    (page, body, StatusCode::NOT_FOUND)
}

fn content_type(path: &Path) -> HeaderValue {
    let value = match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xml") => "application/xml; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(value)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../rustclamp.com/public");
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(default_root);
    let root = Arc::new(fs::canonicalize(root)?);

    let router = TargetComposition::<HttpRoutes<Public>, Public>::new(vec![
        (
            ModuleId::new("site.pages"),
            HttpRoute::<Public>::new("/", get(serve_file).with_state(root.clone())),
        ),
        (
            ModuleId::new("site.pages"),
            HttpRoute::<Public>::new("/{*path}", get(serve_file).with_state(root)),
        ),
    ])
    .build(Some(&HttpRoutes::new()))
    .map_err(|error| match error {
        TargetCompositionError::Target(error) => std::io::Error::other(error),
        TargetCompositionError::UnconsumedRequired { .. } => {
            std::io::Error::other("the selected site route target was not consumed")
        }
    })?
    .expect("the public route target is selected");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8002").await?;
    println!(
        "serving {} at http://127.0.0.1:8002",
        listener.local_addr()?
    );
    rustclamp_http::ecosystem::serve(listener, router).await?;
    Ok(())
}
