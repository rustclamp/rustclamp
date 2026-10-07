//! Tiny dependency-light console adapter for the shared Users operation.

use futures_executor::block_on;
use rustclamp_example_users::{
    ConsoleCommand, MemoryUserRepository, OperationContext, Users, parse_console,
};
use std::time::{Duration, Instant};

fn main() -> Result<(), String> {
    let command = parse_console(&std::env::args().skip(1).collect::<Vec<_>>())?;
    let mut users = Users::new(MemoryUserRepository::default());
    let context = OperationContext::new(
        Some("console".to_owned()),
        "default",
        "console",
        Instant::now() + Duration::from_secs(1),
        || false,
    );
    let output = block_on(async {
        match command {
            ConsoleCommand::Create { name } => users
                .create(&context, &name)
                .await
                .map(|user| format!("created user {}: {}", user.id.0, user.name)),
            ConsoleCommand::Get { id } => users
                .get(&context, id)
                .await
                .map(|user| format!("user {}: {}", user.id.0, user.name)),
        }
    })
    .map_err(|error| error.to_string())?;
    println!("{output}");
    Ok(())
}
