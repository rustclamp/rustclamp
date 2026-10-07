//! Compare one in-memory Users read directly with the same read over Axum.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt;
use rustclamp_example_users::{
    MemoryUserRepository, OperationContext, User, UserId, Users, UsersErrorKind,
};
use rustclamp_http::ecosystem as axum;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower::ServiceExt;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[allow(dead_code)]
#[path = "http.rs"]
mod http;

#[derive(serde::Serialize)]
struct DirectUserResponse {
    id: i64,
    tenant: String,
    name: String,
}

async fn direct_axum_get(
    State(state): State<Arc<Mutex<Users<MemoryUserRepository>>>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Json<DirectUserResponse>, StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer demo")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let tenant = headers
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("default")
        .to_owned();
    let context = OperationContext::new(
        Some("demo".into()),
        tenant,
        "measure-http",
        Instant::now() + Duration::from_secs(1),
        || false,
    );
    let user = state
        .lock()
        .await
        .get(&context, UserId(id))
        .await
        .map_err(|error| match error.kind() {
            UsersErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            UsersErrorKind::NotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    Ok(Json(to_direct_response(user)))
}

fn to_direct_response(user: User) -> DirectUserResponse {
    DirectUserResponse {
        id: user.id.0,
        tenant: user.tenant,
        name: user.name,
    }
}

const ITERATIONS: usize = 1_000;
const SAMPLES: usize = 7;

fn context() -> OperationContext {
    OperationContext::new(
        Some("benchmark".into()),
        "benchmark-tenant",
        "measure-http",
        Instant::now() + Duration::from_secs(60),
        || false,
    )
}

#[tokio::main]
async fn main() {
    let repository = MemoryUserRepository::default();
    let mut seed = Users::new(repository.clone());
    seed.create(&context(), "Ada").await.unwrap();
    let direct_router = Router::new()
        .route("/users/{id}", get(direct_axum_get))
        .with_state(Arc::new(Mutex::new(Users::new(repository.clone()))));
    let router = http::application_router(Arc::new(Mutex::new(Users::new(repository)))).unwrap();
    let mut direct_samples = Vec::with_capacity(SAMPLES);
    let mut http_samples = Vec::with_capacity(SAMPLES);
    let mut direct_allocations = Vec::with_capacity(SAMPLES);
    let mut http_allocations = Vec::with_capacity(SAMPLES);

    for _ in 0..SAMPLES {
        let region = Region::new(GLOBAL);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let request = Request::builder()
                .uri("/users/1")
                .header(header::AUTHORIZATION, "Bearer demo")
                .header("x-tenant-id", "benchmark-tenant")
                .header(header::ACCEPT, "application/json")
                .body(Body::empty())
                .unwrap();
            let response = direct_router.clone().oneshot(request).await.unwrap();
            assert!(response.status().is_success());
            std::hint::black_box(response.into_body().collect().await.unwrap());
        }
        direct_samples.push(start.elapsed().as_nanos() / ITERATIONS as u128);
        let stats = region.change();
        direct_allocations.push((stats.allocations, stats.bytes_allocated));

        let region = Region::new(GLOBAL);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let request = Request::builder()
                .uri("/users/1")
                .header(header::AUTHORIZATION, "Bearer demo")
                .header("x-tenant-id", "benchmark-tenant")
                .header(header::ACCEPT, "application/json")
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert!(response.status().is_success());
            std::hint::black_box(response.into_body().collect().await.unwrap());
        }
        http_samples.push(start.elapsed().as_nanos() / ITERATIONS as u128);
        let stats = region.change();
        http_allocations.push((stats.allocations, stats.bytes_allocated));
    }
    direct_samples.sort_unstable();
    http_samples.sort_unstable();
    println!("iterations per sample: {ITERATIONS}; samples: {SAMPLES}");
    println!(
        "direct Axum route median: {} ns/op",
        direct_samples[SAMPLES / 2]
    );
    println!(
        "RustClamp HTTP route median: {} ns/op",
        http_samples[SAMPLES / 2]
    );
    direct_allocations.sort_unstable();
    http_allocations.sort_unstable();
    let direct_alloc = direct_allocations[SAMPLES / 2];
    let http_alloc = http_allocations[SAMPLES / 2];
    println!(
        "allocations/request: direct Axum {:.1}, RustClamp {:.1}",
        direct_alloc.0 as f64 / ITERATIONS as f64,
        http_alloc.0 as f64 / ITERATIONS as f64
    );
    println!(
        "bytes allocated/request: direct Axum {:.1}, RustClamp {:.1}",
        direct_alloc.1 as f64 / ITERATIONS as f64,
        http_alloc.1 as f64 / ITERATIONS as f64
    );
    println!(
        "ratio: {:.2}x (includes request construction, middleware, routing, and JSON response)",
        http_samples[SAMPLES / 2] as f64 / direct_samples[SAMPLES / 2] as f64
    );
}
