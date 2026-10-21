//! `Query::upsert_many` against its control, one `INSERT ... ON CONFLICT`
//! a row, both inside one transaction, on a table that already holds every
//! key and with one price in ten changed: `cargo run --release --example
//! measure-upsert-many --features postgres,db`. SQLite runs in memory;
//! Postgres runs when `RUSTCLAMP_TEST_DATABASE_URL` names a server, in a
//! table it drops afterwards.

use std::time::{Duration, Instant};

use rustclamp::config::Config;
use rustclamp::db::{Db, Param};

const ROWS: i64 = 3_400;
const SAMPLES: i64 = 7;
const ONE_ROW: &str = "INSERT INTO measure_upsert_many (series_id, valid_from, price, fetched_at) VALUES (?, ?, ?, ?) \
    ON CONFLICT (series_id, valid_from) DO UPDATE SET price = excluded.price, fetched_at = excluded.fetched_at \
    WHERE measure_upsert_many.price IS DISTINCT FROM excluded.price";

fn main() {
    let mut engines = vec![("sqlite (memory)", "DB_DATABASE=:memory:".to_owned())];
    if let Ok(url) = std::env::var("RUSTCLAMP_TEST_DATABASE_URL") {
        engines.push((
            "postgres",
            format!("DB_CONNECTION=pgsql\nDATABASE_URL={url}"),
        ));
    }
    let columns = ["series_id", "valid_from", "price", "fetched_at"];
    for (engine, env) in engines {
        let db = Db::open(&Config::parse(&env));
        db.with(|sql| {
            sql.execute_batch(
                "DROP TABLE IF EXISTS measure_upsert_many;
                 CREATE TABLE measure_upsert_many (series_id BIGINT, valid_from TEXT, price DOUBLE PRECISION,
                     fetched_at BIGINT, PRIMARY KEY (series_id, valid_from))",
            )
        })
        .unwrap();
        let (mut control, mut many) = (Vec::new(), Vec::new());
        // Run 0 fills the table; each later run changes one price in ten.
        for run in 0..=2 * SAMPLES {
            let data: Vec<(i64, f64)> = (0..ROWS)
                .map(|n| (n, if n % 10 == 0 { run as f64 } else { 1.0 }))
                .collect();
            // Alternate which runs first, so neither always gets the warm cache.
            let bulk = run % 2 == 0;
            let start = Instant::now();
            db.transaction(|tx| {
                if bulk {
                    let rows: Vec<[&dyn Param; 4]> = data
                        .iter()
                        .map(|(n, p)| [n as &dyn Param, &"2026-10-21", p, &run])
                        .collect();
                    tx.table("measure_upsert_many").upsert_many(
                        &columns,
                        &rows,
                        &["series_id", "valid_from"],
                        &["price", "fetched_at"],
                        &["price"],
                    )?;
                } else {
                    for (n, p) in &data {
                        tx.execute(ONE_ROW, &[n, &"2026-10-21", p, &run])?;
                    }
                }
                Ok::<_, rustclamp::db::Error>(())
            })
            .unwrap();
            let elapsed = start.elapsed();
            let written = db
                .table("measure_upsert_many")
                .where_eq("fetched_at", &run)
                .count()
                .unwrap();
            assert_eq!(
                written,
                if run == 0 { ROWS } else { ROWS / 10 },
                "only changed rows are written"
            );
            if run > 0 {
                if bulk {
                    many.push(elapsed)
                } else {
                    control.push(elapsed)
                }
            }
        }
        db.with(|sql| sql.execute_batch("DROP TABLE measure_upsert_many"))
            .unwrap();
        let median = |samples: &mut Vec<Duration>| {
            samples.sort();
            samples[samples.len() / 2]
        };
        let (control, many) = (median(&mut control), median(&mut many));
        println!(
            "{engine}: {ROWS} rows, 1 in 10 changed, median of {SAMPLES}: row by row {control:.2?}, upsert_many {many:.2?} ({:.1}x)",
            control.as_secs_f64() / many.as_secs_f64()
        );
    }
}
