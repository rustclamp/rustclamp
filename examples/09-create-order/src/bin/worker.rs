//! Consumes order-created messages with a transactional inbox.

use async_nats::jetstream::{self, consumer::pull::Config};
use futures_util::StreamExt;
use rustclamp_example_create_order::{ensure_stream, fulfill_order_once, migrate};
use rustclamp_messaging::MessageEnvelope;
use rustclamp_postgres::{Database, Primary};
use std::error::Error;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let database = Database::<Primary>::connect_managed(&database_url, 5).await?;
    migrate(&database).await?;
    let client = async_nats::connect(nats_url).await?;
    let jetstream = jetstream::new(client);
    ensure_stream(&jetstream).await?;
    let consumer = jetstream
        .get_stream("RUSTCLAMP_ORDERS")
        .await?
        .get_or_create_consumer(
            "orders-worker",
            Config {
                durable_name: Some("orders-worker".into()),
                ack_wait: Duration::from_secs(45),
                max_deliver: 5,
                max_ack_pending: 1,
                filter_subject: "orders.order-created".into(),
                ..Default::default()
            },
        )
        .await?;

    println!("orders worker listening");
    let mut messages = consumer.messages().await?;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let delivery = tokio::select! {
            result = &mut shutdown => {
                result?;
                println!("stopping order fetch; current transaction has drained");
                break;
            }
            delivery = messages.next() => delivery,
        };
        let Some(message) = delivery else { break };
        let message = message?;
        let envelope: MessageEnvelope = serde_json::from_slice(&message.payload)?;
        let applied = fulfill_order_once(&database, &envelope).await?;
        message.ack().await?;
        println!(
            "{} order {}",
            if applied { "fulfilled" } else { "duplicate" },
            envelope.id
        );
    }
    Ok(())
}
