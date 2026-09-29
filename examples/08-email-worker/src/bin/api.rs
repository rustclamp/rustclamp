//! Publishes one email request through the shared JetStream stream.

use async_nats::jetstream::{
    self,
    stream::{Config, DiscardPolicy},
};
use rustclamp_messaging::MessageEnvelope;
use serde_json::json;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

const STREAM: &str = "RUSTCLAMP_EMAIL";
const SUBJECT: &str = "email.send";
const DEAD_LETTER_SUBJECT: &str = "email.dead";

struct Email {
    to: String,
    subject: String,
    body: String,
}

trait EmailSender {
    async fn send(&self, email: Email) -> Result<(), Box<dyn Error + Send + Sync>>;
}

struct JetStreamEmailSender {
    jetstream: jetstream::Context,
}

impl EmailSender for JetStreamEmailSender {
    async fn send(&self, email: Email) -> Result<(), Box<dyn Error + Send + Sync>> {
        let message = MessageEnvelope {
            id: format!("email-{}-{}", std::process::id(), timestamp()),
            name: SUBJECT.into(),
            schema_version: 1,
            correlation_id: format!("signup-{}", std::process::id()),
            causation_id: None,
            deadline_unix_ms: Some(now_ms().saturating_add(20_000)),
            payload: json!({
                "to": email.to,
                "subject": email.subject,
                "body": email.body,
            }),
        };
        let ack = self
            .jetstream
            .publish(SUBJECT, serde_json::to_vec(&message)?.into())
            .await?;
        ack.await?;
        println!("queued {} ({})", message.name, message.id);
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let client = async_nats::connect(url).await?;
    let jetstream = jetstream::new(client);
    jetstream
        .get_or_create_stream(Config {
            name: STREAM.into(),
            subjects: vec![SUBJECT.into(), DEAD_LETTER_SUBJECT.into()],
            max_messages: 10_000,
            max_bytes: 64 * 1024 * 1024,
            discard: DiscardPolicy::New,
            ..Default::default()
        })
        .await?;

    let api = JetStreamEmailSender { jetstream };
    api.send(Email {
        to: "ada@example.test".into(),
        subject: "Welcome".into(),
        body: "Your account is ready.".into(),
    })
    .await?;
    Ok(())
}

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
