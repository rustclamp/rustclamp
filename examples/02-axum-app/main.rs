//! An existing Axum service joins Clamp composition without rewriting its routes.

use std::error::Error;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::Request;
use axum::routing::get;
use rustclamp_core::ModuleId;
use rustclamp_http::{HttpRoute, HttpRoutes, Public};
use rustclamp_kernel::{TargetComposition, TargetCompositionError};
use tower::ServiceExt;

/// The application as it was before Clamp: plain Axum, untouched.
fn existing_app() -> Router {
    Router::new()
        .route("/", get(|| async { "existing home" }))
        .route(
            "/users/{id}",
            get(
                |Path(id): Path<u32>, State(name): State<&'static str>| async move {
                    format!("{name} user {id}")
                },
            ),
        )
        .with_state("existing")
}

fn compose() -> Result<Router, Box<dyn Error>> {
    let router = TargetComposition::<HttpRoutes<Public>, Public>::new(vec![
        // The whole existing router is one contribution, mounted at the root.
        (
            ModuleId::new("app.existing"),
            HttpRoute::<Public>::mount("/", existing_app()),
        ),
        // A new module adds its route beside it.
        (
            ModuleId::new("app.health"),
            HttpRoute::<Public>::new("/health", get(|| async { "ok" })),
        ),
    ])
    .build(Some(&HttpRoutes::new()))
    .map_err(|error| match error {
        TargetCompositionError::Target(error) => std::io::Error::other(error),
        TargetCompositionError::UnconsumedRequired { .. } => {
            std::io::Error::other("the selected route target was not consumed")
        }
    })?
    .expect("the public route target is selected");
    Ok(router)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let router = compose()?;
    if std::env::args().nth(1).as_deref() == Some("serve") {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8003").await?;
        println!("serving at http://{}", listener.local_addr()?);
        rustclamp_http::ecosystem::serve(listener, router).await?;
        return Ok(());
    }
    for path in ["/", "/users/7", "/health"] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await?;
        println!("GET {path} -> {status} {}", String::from_utf8_lossy(&body));
    }
    Ok(())
}
