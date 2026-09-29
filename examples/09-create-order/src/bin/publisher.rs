//! Supervises a bounded transactional-outbox publisher.

use async_nats::jetstream;
use rustclamp_example_create_order::{PUBLISH_POLL, ensure_stream, publish_pending};
use rustclamp_postgres::{Database, Primary};
use std::error::Error;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let database = Database::<Primary>::connect_managed(&database_url, 5).await?;
    let client = async_nats::connect(nats_url).await?;
    let jetstream = jetstream::new(client);
    ensure_stream(&jetstream).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            result = &mut shutdown => {
                result?;
                println!("outbox publisher stopped");
                break;
            }
            result = publish_pending(&database, &jetstream) => match result {
                Ok(0) => sleep(PUBLISH_POLL).await,
                Ok(count) => println!("published {count} outbox message(s)"),
                Err(error) => {
                    eprintln!("outbox publish attempt failed: {error}");
                    sleep(PUBLISH_POLL).await;
                }
            }
        }
    }
    Ok(())
}
