//! Creates an order and atomically records its outbox message.

use rustclamp_example_create_order::{create_order, migrate};
use rustclamp_postgres::{Database, Primary};
use std::error::Error;

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
        _ => return Err("usage: orders-api migrate | create [sku] [quantity]".into()),
    }
    Ok(())
}

async fn connect_database() -> Result<Database<Primary>, Box<dyn Error + Send + Sync>> {
    let url = std::env::var("DATABASE_URL")?;
    Ok(Database::<Primary>::connect_managed(&url, 5).await?)
}
