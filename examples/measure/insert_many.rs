//! `Query::insert_many` against its control, one `insert_only` a row, both
//! inside one transaction: `cargo run --release --example measure-insert-many
//! --features postgres`. SQLite runs in memory; Postgres runs when
//! `RUSTCLAMP_TEST_DATABASE_URL` names a server, in a table it drops afterwards.

use std::time::{Duration, Instant};

use rustclamp::config::Config;
use rustclamp::db::{Db, Param};

const ROWS: i64 = 3_400;
const SAMPLES: usize = 7;

fn main() {
    let mut engines = vec![("sqlite (memory)", "DB_DATABASE=:memory:".to_owned())];
    if let Ok(url) = std::env::var("RUSTCLAMP_TEST_DATABASE_URL") {
        engines.push((
            "postgres",
            format!("DB_CONNECTION=pgsql\nDATABASE_URL={url}"),
        ));
    }
    for (engine, env) in engines {
        let db = Db::open(&Config::parse(&env));
        db.with(|sql| {
            sql.execute_batch(
                "DROP TABLE IF EXISTS measure_insert_many;
                 CREATE TABLE measure_insert_many (station BIGINT, day TEXT, price DOUBLE PRECISION)",
            )
        })
        .unwrap();
        let data: Vec<(i64, String, f64)> = (0..ROWS)
            .map(|n| {
                (
                    n,
                    format!("2026-10-{:02}", n % 28 + 1),
                    1.0 + n as f64 / 1000.0,
                )
            })
            .collect();
        let columns = ["station", "day", "price"];
        let (mut control, mut many) = (Vec::new(), Vec::new());
        for sample in 0..SAMPLES {
            // Alternate which runs first, so neither always gets the warm cache.
            let mut runs: [(&mut Vec<Duration>, bool); 2] =
                [(&mut control, false), (&mut many, true)];
            if sample % 2 == 1 {
                runs.reverse();
            }
            for (samples, bulk) in runs {
                db.with(|sql| sql.execute_batch("DELETE FROM measure_insert_many"))
                    .unwrap();
                let start = Instant::now();
                db.transaction(|tx| {
                    let table = tx.table("measure_insert_many");
                    if bulk {
                        let rows: Vec<[&dyn Param; 3]> = data
                            .iter()
                            .map(|(s, d, p)| [s as &dyn Param, d, p])
                            .collect();
                        table.insert_many(&columns, &rows)?;
                    } else {
                        for (s, d, p) in &data {
                            table.insert_only(&columns, &[s, d, p])?;
                        }
                    }
                    Ok::<_, rustclamp::db::Error>(())
                })
                .unwrap();
                samples.push(start.elapsed());
                let count = db.table("measure_insert_many").count().unwrap();
                assert_eq!(count, ROWS, "every row went in");
            }
        }
        db.with(|sql| sql.execute_batch("DROP TABLE measure_insert_many"))
            .unwrap();
        let median = |samples: &mut Vec<Duration>| {
            samples.sort();
            samples[samples.len() / 2]
        };
        let (control, many) = (median(&mut control), median(&mut many));
        println!(
            "{engine}: {ROWS} rows, median of {SAMPLES}: row by row {control:.2?}, insert_many {many:.2?} ({:.1}x)",
            control.as_secs_f64() / many.as_secs_f64()
        );
    }
}
