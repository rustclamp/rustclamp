//! Runs the email handler in its own process and acknowledges successful work.

use async_nats::jetstream::{self, AckKind, consumer::pull::Config, stream};
use futures_util::StreamExt;
use rustclamp_core::ModuleId;
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::MessageEnvelope;
use rustclamp_worker::{
    DispatchError, HandlerDeclaration, HandlerFailure, HandlerRegistry, HandlerTarget,
    WorkerHandlers,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STREAM: &str = "RUSTCLAMP_EMAIL";
const SUBJECT: &str = "email.send";
const DEAD_LETTER_SUBJECT: &str = "email.dead";
const MAX_ATTEMPTS: i64 = 5;
const MAX_HANDLER_TIME: Duration = Duration::from_secs(25);
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
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init()
        .ok();

    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let client = async_nats::connect(url).await?;
    let jetstream = jetstream::new(client);
    let stream = jetstream
        .get_or_create_stream(stream::Config {
            name: STREAM.into(),
            subjects: vec![SUBJECT.into(), DEAD_LETTER_SUBJECT.into()],
            max_messages: 10_000,
            max_bytes: 64 * 1024 * 1024,
            discard: stream::DiscardPolicy::New,
            ..Default::default()
        })
        .await?;
    let consumer = stream
        .get_or_create_consumer(
            "email-worker",
            Config {
                durable_name: Some("email-worker".into()),
                ack_wait: Duration::from_secs(45),
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
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let delivery = tokio::select! {
            result = &mut shutdown => {
                result?;
                println!("stopping fetch; current delivery has drained");
                break;
            }
            delivery = messages.next() => delivery,
        };
        let Some(message) = delivery else { break };
        let message = message?;
        let attempts = message.info()?.delivered;
        let outcome = match serde_json::from_slice::<MessageEnvelope>(&message.payload) {
            Err(error) => DeliveryOutcome::DeadLetter {
                classification: "invalid_envelope",
                reason: error.to_string(),
            },
            Ok(envelope) => handle_message(&registry, envelope, attempts).await,
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

async fn handle_message(
    registry: &HandlerRegistry,
    envelope: MessageEnvelope,
    attempts: i64,
) -> DeliveryOutcome {
    let correlation_id = envelope.correlation_id.clone();
    let message_id = envelope.id.clone();
    let budget = if let Some(deadline) = envelope.deadline_unix_ms {
        let remaining = deadline.saturating_sub(now_ms());
        if remaining == 0 {
            return DeliveryOutcome::DeadLetter {
                classification: "expired",
                reason: "message deadline passed before execution".into(),
            };
        }
        MAX_HANDLER_TIME.min(Duration::from_millis(remaining))
    } else {
        MAX_HANDLER_TIME
    };
    let dispatch = registry.dispatch(envelope);
    #[cfg(feature = "tracing")]
    let dispatch = {
        use tracing::Instrument;
        dispatch.instrument(tracing::info_span!(
            "worker.delivery",
            %correlation_id,
            %message_id,
            attempt = attempts
        ))
    };
    #[cfg(not(feature = "tracing"))]
    let _ = (correlation_id, message_id);

    let result = match tokio::time::timeout(budget, dispatch).await {
        Ok(result) => result,
        Err(_) => {
            return DeliveryOutcome::DeadLetter {
                classification: "unknown_outcome",
                reason: "handler timed out; a remote side effect may have occurred".into(),
            };
        }
    };

    match result {
        Ok(()) => DeliveryOutcome::Ack,
        Err(DispatchError::NoHandler { .. }) => DeliveryOutcome::Reject,
        Err(DispatchError::Decode(error)) => DeliveryOutcome::DeadLetter {
            classification: "invalid_envelope",
            reason: error.to_string(),
        },
        Err(DispatchError::Handler(HandlerFailure::Permanent(error))) => {
            DeliveryOutcome::DeadLetter {
                classification: "permanent_failure",
                reason: error.to_string(),
            }
        }
        Err(DispatchError::Handler(HandlerFailure::UnknownOutcome(error))) => {
            DeliveryOutcome::DeadLetter {
                classification: "unknown_outcome",
                reason: error.to_string(),
            }
        }
        Err(DispatchError::Handler(HandlerFailure::Retryable(error)))
            if attempts >= MAX_ATTEMPTS =>
        {
            DeliveryOutcome::DeadLetter {
                classification: "retry_exhausted",
                reason: error.to_string(),
            }
        }
        Err(DispatchError::Handler(HandlerFailure::Retryable(_))) => {
            DeliveryOutcome::Retry(retry_delay(attempts))
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
