//! Compare direct SQLx and the Users adapter over one local PostgreSQL query.

use rustclamp_example_users::{
    OperationContext, PgUserRepository, UserId, Users, pg_users_migrations,
};
use rustclamp_postgres::{Database, Primary};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ITERATIONS: usize = 100;
const SAMPLES: usize = 7;
const TENANT: &str = "phase6-measurement";

fn context() -> OperationContext {
    OperationContext::new(
        Some("benchmark".into()),
        TENANT,
        "measure-postgres",
        Instant::now() + Duration::from_secs(60),
        || false,
    )
}

#[tokio::main]
async fn main() {
    let url = std::env::var("RUSTCLAMP_TEST_DATABASE_URL")
        .expect("set RUSTCLAMP_TEST_DATABASE_URL to a local PostgreSQL database");
    let database = Database::<Primary>::connect_managed(&url, 4).await.unwrap();
    pg_users_migrations()
        .unwrap()
        .run(database.pool())
        .await
        .unwrap();
    let id: (i64,) = rustclamp_postgres::sqlx::query_as(
        "INSERT INTO rustclamp_users (tenant, name) VALUES ($1, 'benchmark') RETURNING id",
    )
    .bind(TENANT)
    .fetch_one(database.pool())
    .await
    .unwrap();

    let mut direct_samples = Vec::with_capacity(SAMPLES);
    let mut adapter_samples = Vec::with_capacity(SAMPLES);
    let mut direct_allocations = Vec::with_capacity(SAMPLES);
    let mut adapter_allocations = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let region = Region::new(GLOBAL);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let mut transaction = database.begin().await.unwrap();
            let row: (i64, String, String) = rustclamp_postgres::sqlx::query_as(
                "SELECT id, tenant, name FROM rustclamp_users WHERE tenant = $1 AND id = $2",
            )
            .bind(TENANT)
            .bind(id.0)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
            transaction.commit().await.unwrap();
            std::hint::black_box(row);
        }
        direct_samples.push(start.elapsed().as_nanos() / ITERATIONS as u128);
        let stats = region.change();
        direct_allocations.push((stats.allocations, stats.bytes_allocated));

        let region = Region::new(GLOBAL);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let repository = PgUserRepository::begin(&database).await.unwrap();
            let mut users = Users::new(repository);
            let user = users.get(&context(), UserId(id.0)).await.unwrap();
            users.into_repository().commit().await.unwrap();
            std::hint::black_box(user);
        }
        adapter_samples.push(start.elapsed().as_nanos() / ITERATIONS as u128);
        let stats = region.change();
        adapter_allocations.push((stats.allocations, stats.bytes_allocated));
    }
    direct_samples.sort_unstable();
    adapter_samples.sort_unstable();
    println!("iterations per sample: {ITERATIONS}; samples: {SAMPLES}");
    println!(
        "direct SQLx transaction median: {:.1} us/op",
        direct_samples[SAMPLES / 2] as f64 / 1_000.0
    );
    println!(
        "Users adapter transaction median: {:.1} us/op",
        adapter_samples[SAMPLES / 2] as f64 / 1_000.0
    );
    println!(
        "ratio: {:.2}x (same database query and transaction boundary)",
        adapter_samples[SAMPLES / 2] as f64 / direct_samples[SAMPLES / 2] as f64
    );
    direct_allocations.sort_unstable();
    adapter_allocations.sort_unstable();
    let direct_alloc = direct_allocations[SAMPLES / 2];
    let adapter_alloc = adapter_allocations[SAMPLES / 2];
    println!(
        "allocations/read: direct SQLx {:.1}, Users adapter {:.1}",
        direct_alloc.0 as f64 / ITERATIONS as f64,
        adapter_alloc.0 as f64 / ITERATIONS as f64
    );
    println!(
        "bytes allocated/read: direct SQLx {:.1}, Users adapter {:.1}",
        direct_alloc.1 as f64 / ITERATIONS as f64,
        adapter_alloc.1 as f64 / ITERATIONS as f64
    );
    database.close().await;
}
