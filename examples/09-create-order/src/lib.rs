//! PostgreSQL transactional outbox proof for CreateOrder.

use async_nats::jetstream::{Context, stream};
use rustclamp_messaging::MessageEnvelope;
use rustclamp_postgres::{Database, Primary, sqlx};
use serde_json::json;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
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
            status TEXT NOT NULL DEFAULT 'created',
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        ALTER TABLE orders ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'created';
        CREATE TABLE IF NOT EXISTS outbox (
            id BIGSERIAL PRIMARY KEY,
            message_id TEXT NOT NULL UNIQUE,
            payload TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            published_at TIMESTAMPTZ
        );
        CREATE TABLE IF NOT EXISTS inbox (
            message_id TEXT PRIMARY KEY,
            received_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE TABLE IF NOT EXISTS fulfillments (
            message_id TEXT PRIMARY KEY REFERENCES inbox(message_id),
            order_id BIGINT NOT NULL UNIQUE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
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

/// Applies the order-created database effect once per message identity.
///
/// Inbox insertion and the protected effect share one transaction. Concurrent
/// duplicates serialize on the inbox primary key; a rollback removes both.
pub async fn fulfill_order_once(
    database: &Database<Primary>,
    message: &MessageEnvelope,
) -> Result<bool, Box<dyn Error + Send + Sync>> {
    if message.name != SUBJECT || message.schema_version != 1 {
        return Err(std::io::Error::other("unsupported order message route or version").into());
    }
    let order_id = message
        .payload
        .get("order_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| std::io::Error::other("order message has no integer order_id"))?;

    let mut transaction = database.begin().await?;
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO inbox (message_id) VALUES ($1)
         ON CONFLICT (message_id) DO NOTHING RETURNING message_id",
    )
    .bind(&message.id)
    .fetch_optional(&mut *transaction)
    .await?;
    if inserted.is_none() {
        transaction.commit().await?;
        return Ok(false);
    }
    sqlx::query("INSERT INTO fulfillments (message_id, order_id) VALUES ($1, $2)")
        .bind(&message.id)
        .bind(order_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(true)
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

/// Future returned by the application-owned payment gateway port.
pub type PaymentFuture<'a> = Pin<Box<dyn Future<Output = Result<(), PaymentFailure>> + Send + 'a>>;

/// Application boundary for one payment attempt.
pub trait PaymentGateway: Send + Sync {
    /// Charges one order or returns a classified failure.
    fn charge(&self, order_id: i64) -> PaymentFuture<'_>;
}

/// Failure known to happen before a payment side effect, or a definite decline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentFailure {
    /// The payment provider definitively declined the charge.
    Declined,
    /// The provider guarantees the charge was not attempted and can be retried by policy.
    Retryable,
}

impl std::fmt::Display for PaymentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => f.write_str("payment declined"),
            Self::Retryable => f.write_str("payment failed before the side effect"),
        }
    }
}

impl Error for PaymentFailure {}

/// Result of one payment attempt, including an ambiguous timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentDecision {
    /// The provider confirmed the charge.
    Authorized,
    /// The provider definitively declined the charge.
    Declined,
    /// The provider guaranteed no side effect, so application policy may retry.
    Retryable,
    /// The attempt timed out and may already have charged the customer.
    UnknownOutcome,
}

/// Runs one bounded payment attempt without retrying ambiguous outcomes.
pub async fn payment_attempt<G: PaymentGateway>(
    gateway: &G,
    order_id: i64,
    timeout: Duration,
) -> PaymentDecision {
    match tokio::time::timeout(timeout, gateway.charge(order_id)).await {
        Ok(Ok(())) => PaymentDecision::Authorized,
        Ok(Err(PaymentFailure::Declined)) => PaymentDecision::Declined,
        Ok(Err(PaymentFailure::Retryable)) => PaymentDecision::Retryable,
        Err(_) => PaymentDecision::UnknownOutcome,
    }
}

/// Persists a payment decision and compensates inventory only after a definite decline.
pub async fn settle_payment(
    database: &Database<Primary>,
    order_id: i64,
    decision: PaymentDecision,
) -> Result<bool, Box<dyn Error + Send + Sync>> {
    if decision == PaymentDecision::Retryable {
        return Ok(false);
    }
    let mut transaction = database.begin().await?;
    let reservation: Option<(String, i32, String)> =
        sqlx::query_as("SELECT sku, quantity, status FROM orders WHERE id = $1 FOR UPDATE")
            .bind(order_id)
            .fetch_optional(&mut *transaction)
            .await?;
    let Some((sku, quantity, status)) = reservation else {
        return Err(std::io::Error::other("order not found").into());
    };
    if !matches!(status.as_str(), "created" | "payment_unknown") {
        transaction.commit().await?;
        return Ok(false);
    }

    let next_status = match decision {
        PaymentDecision::Authorized => "paid",
        PaymentDecision::Declined => "declined",
        PaymentDecision::UnknownOutcome => "payment_unknown",
        PaymentDecision::Retryable => unreachable!("handled before opening transaction"),
    };
    sqlx::query("UPDATE orders SET status = $1 WHERE id = $2")
        .bind(next_status)
        .bind(order_id)
        .execute(&mut *transaction)
        .await?;
    if decision == PaymentDecision::Declined {
        sqlx::query("UPDATE inventory SET available = available + $1 WHERE sku = $2")
            .bind(quantity)
            .bind(sku)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(true)
}
