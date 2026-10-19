//! Test helper: a [`Db`] for each engine under test (ADR 0034). SQLite
//! always runs; Postgres runs when `RUSTCLAMP_TEST_DATABASE_URL` names a
//! server, each test in a schema of its own that is dropped afterwards.

use super::{Db, Dialect};
use crate::config::Config;

/// A [`Db`] that cleans up after the test.
pub(crate) struct TestDb {
    /// Taken in `drop`, so the pool's connections close before the schema goes.
    db: Option<Db>,
    #[cfg(feature = "postgres")]
    schema: Option<(super::postgres::Client, String)>,
}

impl std::ops::Deref for TestDb {
    type Target = Db;

    fn deref(&self) -> &Db {
        self.db.as_ref().expect("a TestDb has its Db until dropped")
    }
}

#[cfg(feature = "postgres")]
impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.db.take());
        if let Some((admin, schema)) = &mut self.schema
            && let Err(error) = admin.execute_batch(&format!("DROP SCHEMA {schema} CASCADE"))
        {
            eprintln!("leaked test schema {schema}: {error}");
        }
    }
}

/// An in-memory SQLite database.
pub(crate) fn sqlite() -> TestDb {
    TestDb {
        db: Some(Db::open(&Config::parse("DB_DATABASE=:memory:"))),
        #[cfg(feature = "postgres")]
        schema: None,
    }
}

/// A Postgres database in a fresh schema, or `None` without a test server.
#[cfg(feature = "postgres")]
pub(crate) fn postgres() -> Option<TestDb> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("RUSTCLAMP_TEST_DATABASE_URL").ok()?;
    let schema = format!(
        "rc_test_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let config = super::postgres::Config::parse_url(&url).expect("RUSTCLAMP_TEST_DATABASE_URL");
    let mut admin = super::postgres::Client::connect(&config).expect("test database");
    admin
        .execute_batch(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    // Every pooled connection starts with the schema first on its search path.
    let separator = if url.contains('?') { '&' } else { '?' };
    let db = Db::try_connect(&super::Settings {
        connection: "pgsql".into(),
        database: String::new(),
        url: Some(format!("{url}{separator}options=-csearch_path%3D{schema}")),
        pool: 10,
    })
    .expect("test database");
    Some(TestDb {
        db: Some(db),
        schema: Some((admin, schema)),
    })
}

/// Every engine this run covers.
pub(crate) fn engines() -> Vec<TestDb> {
    #[cfg(feature = "postgres")]
    return [Some(sqlite()), postgres()].into_iter().flatten().collect();
    #[cfg(not(feature = "postgres"))]
    vec![sqlite()]
}

/// Runs `test` once per engine, each with a fresh database.
pub(crate) fn each_engine(test: impl Fn(&Db)) {
    for engine in engines() {
        eprintln!("on {:?}", engine.dialect());
        test(&engine);
    }
}

/// Whether `db` is Postgres.
pub(crate) fn is_postgres(db: &Db) -> bool {
    db.dialect() == Dialect::Postgres
}
