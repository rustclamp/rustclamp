//! Maps an in-process domain event to an explicit cross-process message.

use rustclamp_messaging::MessageEnvelope;
use serde_json::{Value, json};

/// In-process event emitted after a user is created.
struct UserCreated {
    id: i64,
    tenant: String,
    name: String,
}

/// Metadata assigned by the publishing boundary.
struct PublishContext {
    message_id: String,
    correlation_id: String,
}

fn map_user_created(event: UserCreated, context: PublishContext) -> MessageEnvelope {
    let payload: Value = json!({
        "user_id": event.id,
        "tenant": event.tenant,
        "name": event.name,
    });
    MessageEnvelope {
        id: context.message_id,
        name: "users.user-created".to_owned(),
        schema_version: 1,
        correlation_id: context.correlation_id,
        causation_id: None,
        payload,
    }
}

fn main() -> Result<(), serde_json::Error> {
    let event = UserCreated {
        id: 42,
        tenant: "acme".to_owned(),
        name: "Ada".to_owned(),
    };
    let envelope = map_user_created(
        event,
        PublishContext {
            message_id: "message-42".to_owned(),
            correlation_id: "request-7".to_owned(),
        },
    );
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}
