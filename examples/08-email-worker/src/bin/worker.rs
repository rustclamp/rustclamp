//! Runs the email handler in its own process and acknowledges successful work.

use async_nats::jetstream::{self, AckKind, consumer::pull::Config, stream};
use futures_util::StreamExt;
use rustclamp_core::ModuleId;
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::MessageEnvelope;
use rustclamp_worker::{HandlerDeclaration, HandlerFailure, HandlerTarget, WorkerHandlers};
use serde::Deserialize;
use serde_json::{Value, json};
use std::error::Error;
use std::time::Duration;

const STREAM: &str = "RUSTCLAMP_EMAIL";
const SUBJECT: &str = "email.send";
const DEAD_LETTER_SUBJECT: &str = "email.dead";
const MAX_ATTEMPTS: i64 = 5;
const EMAIL_MODULE: ModuleId = ModuleId::new("example.phase7.email-worker");

enum DeliveryOutcome {
    Ack,
    Retry(Duration),
    Reject,
    DeadLetter {
        classification: &'static str,
        reason: String,
    },
}

#[derive(Deserialize)]
struct EmailPayload {
    to: String,
    subject: String,
    body: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let client = async_nats::connect(url).await?;
    let jetstream = jetstream::new(client);
    let stream = jetstream
        .get_or_create_stream(stream::Config {
            name: STREAM.into(),
            subjects: vec![SUBJECT.into(), DEAD_LETTER_SUBJECT.into()],
            max_messages: 10_000,
            ..Default::default()
        })
        .await?;
    let consumer = stream
        .get_or_create_consumer(
            "email-worker",
            Config {
                durable_name: Some("email-worker".into()),
                ack_wait: Duration::from_secs(30),
                max_deliver: MAX_ATTEMPTS,
                max_ack_pending: 1,
                filter_subject: SUBJECT.into(),
                ..Default::default()
            },
        )
        .await?;

    let handler = HandlerDeclaration::new(SUBJECT, 1, |message: MessageEnvelope| async move {
        let email: EmailPayload = serde_json::from_value(message.payload)
            .map_err(|error| HandlerFailure::Permanent(Box::new(error)))?;
        println!(
            "delivered email to {}: {} — {}",
            email.to, email.subject, email.body
        );
        Ok(())
    });
    let registry =
        TargetComposition::<HandlerTarget, WorkerHandlers>::new(vec![(EMAIL_MODULE, handler)])
            .build(Some(&HandlerTarget))
            .map_err(|error| {
                std::io::Error::other(format!("worker handler assembly failed: {error:?}"))
            })?
            .expect("selected target produces a registry");

    println!("email worker listening on {SUBJECT}");
    let mut messages = consumer.messages().await?;
    while let Some(message) = messages.next().await {
        let message = message?;
        let attempts = message.info()?.delivered;
        let outcome = match serde_json::from_slice::<MessageEnvelope>(&message.payload) {
            Err(error) => DeliveryOutcome::DeadLetter {
                classification: "invalid_envelope",
                reason: error.to_string(),
            },
            Ok(envelope) => match registry.dispatch(envelope).await {
                Ok(()) => DeliveryOutcome::Ack,
                Err(rustclamp_worker::DispatchError::NoHandler { .. }) => DeliveryOutcome::Reject,
                Err(rustclamp_worker::DispatchError::Decode(error)) => {
                    DeliveryOutcome::DeadLetter {
                        classification: "invalid_envelope",
                        reason: error.to_string(),
                    }
                }
                Err(rustclamp_worker::DispatchError::Handler(HandlerFailure::Permanent(error))) => {
                    DeliveryOutcome::DeadLetter {
                        classification: "permanent_failure",
                        reason: error.to_string(),
                    }
                }
                Err(rustclamp_worker::DispatchError::Handler(HandlerFailure::UnknownOutcome(
                    error,
                ))) => DeliveryOutcome::DeadLetter {
                    classification: "unknown_outcome",
                    reason: error.to_string(),
                },
                Err(rustclamp_worker::DispatchError::Handler(HandlerFailure::Retryable(error)))
                    if attempts >= MAX_ATTEMPTS =>
                {
                    DeliveryOutcome::DeadLetter {
                        classification: "retry_exhausted",
                        reason: error.to_string(),
                    }
                }
                Err(rustclamp_worker::DispatchError::Handler(HandlerFailure::Retryable(_))) => {
                    DeliveryOutcome::Retry(retry_delay(attempts))
                }
            },
        };
        match outcome {
            DeliveryOutcome::Ack => message.ack().await?,
            DeliveryOutcome::Retry(delay) => {
                message.ack_with(AckKind::Nak(Some(delay))).await?;
            }
            DeliveryOutcome::Reject => message.ack_with(AckKind::Term).await?,
            DeliveryOutcome::DeadLetter {
                classification,
                reason,
            } => {
                let original: Value = serde_json::from_slice(&message.payload).unwrap_or_else(
                    |_| json!({ "raw": String::from_utf8_lossy(&message.payload) }),
                );
                let dead_letter = serde_json::to_vec(&json!({
                    "classification": classification,
                    "attempts": attempts,
                    "reason": reason,
                    "original": original,
                }))?;
                let published = jetstream
                    .publish(DEAD_LETTER_SUBJECT, dead_letter.into())
                    .await?;
                published.await?;
                message.ack().await?;
            }
        }
    }
    Ok(())
}

fn retry_delay(attempt: i64) -> Duration {
    match attempt {
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(5),
        3 => Duration::from_secs(15),
        _ => Duration::from_secs(30),
    }
}
