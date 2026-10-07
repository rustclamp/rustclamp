//! Creates an order and atomically records its outbox message.

use rustclamp_example_create_order::{
    PaymentFailure, PaymentFuture, PaymentGateway, create_order, migrate, payment_attempt,
    settle_payment,
};
use rustclamp_postgres::{Database, Primary};
use std::error::Error;
use std::time::Duration;

#[derive(Clone, Copy)]
enum DemoPayment {
    Approve,
    Decline,
    Timeout,
    Retryable,
}

struct DemoPaymentGateway(DemoPayment);

impl PaymentGateway for DemoPaymentGateway {
    fn charge(&self, _order_id: i64) -> PaymentFuture<'_> {
        let outcome = self.0;
        Box::pin(async move {
            match outcome {
                DemoPayment::Approve => Ok(()),
                DemoPayment::Decline => Err(PaymentFailure::Declined),
                DemoPayment::Timeout => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok(())
                }
                DemoPayment::Retryable => Err(PaymentFailure::Retryable),
            }
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let database = connect_database().await?;
    match std::env::args().nth(1).as_deref() {
        Some("migrate") => {
            migrate(&database).await?;
            println!("order schema ready; widget inventory seeded");
        }
        Some("create") => {
            let sku = std::env::args().nth(2).unwrap_or_else(|| "widget".into());
            let quantity = std::env::args()
                .nth(3)
                .unwrap_or_else(|| "1".into())
                .parse::<i32>()?;
            let correlation_id = format!("order-request-{}", std::process::id());
            let order_id = create_order(&database, &sku, quantity, &correlation_id).await?;
            println!("created order {order_id}; outbox intent committed");
        }
        Some("payment") => {
            let order_id = std::env::args().nth(2).ok_or("missing order id")?.parse()?;
            let mode = match std::env::args().nth(3).as_deref() {
                Some("approve") => DemoPayment::Approve,
                Some("decline") => DemoPayment::Decline,
                Some("timeout") => DemoPayment::Timeout,
                Some("retryable") => DemoPayment::Retryable,
                _ => return Err("payment mode must be approve, decline, timeout, or retryable".into()),
            };
            let gateway = DemoPaymentGateway(mode);
            let decision = payment_attempt(&gateway, order_id, Duration::from_millis(100)).await;
            let changed = settle_payment(&database, order_id, decision).await?;
            println!("payment decision {decision:?}; state changed: {changed}");
        }
        _ => return Err("usage: orders-api migrate | create [sku] [quantity] | payment <order-id> <approve|decline|timeout|retryable>".into()),
    }
    Ok(())
}

async fn connect_database() -> Result<Database<Primary>, Box<dyn Error + Send + Sync>> {
    let url = std::env::var("DATABASE_URL")?;
    Ok(Database::<Primary>::connect_managed(&url, 5).await?)
}
