//! PostgreSQL transactional outbox proof for CreateOrder.

use async_nats::jetstream::{Context, stream};
use rustclamp_messaging::MessageEnvelope;
use rustclamp_postgres::{Database, Primary, sqlx};
use serde_json::json;
use std::error::Error;
use std::time::Duration;

const STREAM: &str = "RUSTCLAMP_ORDERS";
const SUBJECT: &str = "orders.order-created";

/// Applies the example schema and inserts one initial inventory row.
pub async fn migrate(database: &Database<Primary>) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS inventory (
            sku TEXT PRIMARY KEY,
            available INTEGER NOT NULL CHECK (available >= 0)
        );
        CREATE TABLE IF NOT EXISTS orders (
            id BIGSERIAL PRIMARY KEY,
            sku TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK (quantity > 0),
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE TABLE IF NOT EXISTS outbox (
            id BIGSERIAL PRIMARY KEY,
            message_id TEXT NOT NULL UNIQUE,
            payload TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            published_at TIMESTAMPTZ
        );
        INSERT INTO inventory (sku, available) VALUES ('widget', 100)
            ON CONFLICT (sku) DO NOTHING;",
    )
    .execute(database.pool())
    .await?;
    Ok(())
}

/// Creates an order and its publication intent in one database transaction.
pub async fn create_order(
    database: &Database<Primary>,
    sku: &str,
    quantity: i32,
    correlation_id: &str,
) -> Result<i64, Box<dyn Error + Send + Sync>> {
    if quantity <= 0 {
        return Err(std::io::Error::other("quantity must be positive").into());
    }
    let mut transaction = database.begin().await?;
    let remaining: Option<i32> = sqlx::query_scalar(
        "UPDATE inventory SET available = available - $1
         WHERE sku = $2 AND available >= $1 RETURNING available",
    )
    .bind(quantity)
    .bind(sku)
    .fetch_optional(&mut *transaction)
    .await?;
    if remaining.is_none() {
        return Err(std::io::Error::other("unknown SKU or insufficient inventory").into());
    }
    let order_id: i64 =
        sqlx::query_scalar("INSERT INTO orders (sku, quantity) VALUES ($1, $2) RETURNING id")
            .bind(sku)
            .bind(quantity)
            .fetch_one(&mut *transaction)
            .await?;
    let message = MessageEnvelope {
        id: format!("order-{order_id}"),
        name: SUBJECT.into(),
        schema_version: 1,
        correlation_id: correlation_id.into(),
        causation_id: None,
        deadline_unix_ms: None,
        payload: json!({"order_id": order_id, "sku": sku, "quantity": quantity}),
    };
    sqlx::query("INSERT INTO outbox (message_id, payload) VALUES ($1, $2)")
        .bind(&message.id)
        .bind(serde_json::to_string(&message)?)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(order_id)
}

/// Publishes pending outbox rows and marks them only after broker confirmation.
pub async fn publish_pending(
    database: &Database<Primary>,
    jetstream: &Context,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let mut transaction = database.begin().await?;
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, payload FROM outbox WHERE published_at IS NULL
         ORDER BY id LIMIT 10 FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&mut *transaction)
    .await?;
    for (row_id, payload) in &rows {
        let message: MessageEnvelope = serde_json::from_str(payload)?;
        let ack = jetstream
            .publish(SUBJECT, serde_json::to_vec(&message)?.into())
            .await?;
        ack.await?;
        sqlx::query("UPDATE outbox SET published_at = now() WHERE id = $1")
            .bind(row_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(rows.len())
}

/// Creates the bounded stream used by the supervised publisher.
pub async fn ensure_stream(jetstream: &Context) -> Result<(), Box<dyn Error + Send + Sync>> {
    jetstream
        .get_or_create_stream(stream::Config {
            name: STREAM.into(),
            subjects: vec![SUBJECT.into()],
            max_messages: 10_000,
            max_bytes: 64 * 1024 * 1024,
            discard: stream::DiscardPolicy::New,
            ..Default::default()
        })
        .await?;
    Ok(())
}

/// Delay between supervised publisher polls when no rows are pending.
pub const PUBLISH_POLL: Duration = Duration::from_millis(500);
