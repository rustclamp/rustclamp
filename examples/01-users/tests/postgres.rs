//! Explicit real PostgreSQL transaction and repository integration tests.

#![cfg(feature = "postgres")]

use rustclamp_example_users::{
    OperationContext, PgUserRepository, Users, UsersErrorKind, pg_users_migrations,
};
use rustclamp_postgres::{Database, Primary};
use std::time::{Duration, Instant};

fn context(principal: &str, tenant: &str) -> OperationContext {
    OperationContext::new(
        Some(principal.to_owned()),
        tenant,
        "postgres-test",
        Instant::now() + Duration::from_secs(5),
        || false,
    )
}

#[tokio::test]
#[ignore = "requires the local Phase 6 PostgreSQL service fixture"]
async fn postgres_users_transactions_commit_rollback_and_isolate_concurrent_tenants() {
    let url =
        std::env::var("RUSTCLAMP_TEST_DATABASE_URL").expect("service fixture sets database URL");
    let database = Database::<Primary>::connect_managed(&url, 8).await.unwrap();
    pg_users_migrations()
        .unwrap()
        .run(database.pool())
        .await
        .unwrap();

    let mut repository = PgUserRepository::begin(&database).await.unwrap();
    let mut users = Users::new(repository);
    let ada = users
        .create(&context("operator", "tenant-a"), "Ada")
        .await
        .unwrap();
    assert_eq!(
        users
            .get(&context("operator", "tenant-a"), ada.id)
            .await
            .unwrap(),
        ada
    );
    assert_eq!(
        users
            .get(&context("operator", "tenant-b"), ada.id)
            .await
            .unwrap_err()
            .kind(),
        UsersErrorKind::NotFound
    );
    repository = users.into_repository();
    repository.commit().await.unwrap();

    let mut repository = PgUserRepository::begin(&database).await.unwrap();
    let mut users = Users::new(repository);
    let rollback = users
        .create(&context("operator", "tenant-a"), "Rollback")
        .await
        .unwrap();
    repository = users.into_repository();
    repository.rollback().await.unwrap();
    let mut repository = PgUserRepository::begin(&database).await.unwrap();
    let mut users = Users::new(repository);
    assert_eq!(
        users
            .get(&context("operator", "tenant-a"), rollback.id)
            .await
            .unwrap_err()
            .kind(),
        UsersErrorKind::NotFound
    );
    repository = users.into_repository();
    repository.rollback().await.unwrap();

    let first = database.clone();
    let second = database.clone();
    let (one, two) = tokio::join!(
        async move {
            let mut repository = PgUserRepository::begin(&first).await.unwrap();
            let mut users = Users::new(repository);
            let user = users
                .create(&context("operator", "concurrent-a"), "One")
                .await
                .unwrap();
            repository = users.into_repository();
            repository.commit().await.unwrap();
            user
        },
        async move {
            let mut repository = PgUserRepository::begin(&second).await.unwrap();
            let mut users = Users::new(repository);
            let user = users
                .create(&context("operator", "concurrent-b"), "Two")
                .await
                .unwrap();
            repository = users.into_repository();
            repository.commit().await.unwrap();
            user
        },
    );
    assert_ne!(one.id, two.id);
    assert_ne!(one.tenant, two.tenant);
    database.health().await.unwrap();
    database.close().await;
}

#[tokio::test]
#[ignore = "requires the local Phase 6 PostgreSQL service fixture"]
async fn shutdown_waits_for_transaction_cleanup_and_database_health_recovers() {
    let url =
        std::env::var("RUSTCLAMP_TEST_DATABASE_URL").expect("service fixture sets database URL");
    let database = Database::<Primary>::connect_managed(&url, 2).await.unwrap();
    let transaction = database.begin().await.unwrap();
    let close = tokio::spawn(async move {
        database.close().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !close.is_finished(),
        "pool shutdown waits for an execution transaction"
    );
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .unwrap()
        .unwrap();

    let recovered = Database::<Primary>::connect_managed(&url, 2).await.unwrap();
    recovered.health().await.unwrap();
    recovered.close().await;
}
