//! Runs the email handler in its own process and acknowledges successful work.

use async_nats::jetstream::{self, AckKind, consumer::pull::Config, stream};
use futures_util::StreamExt;
use rustclamp_core::{Clock, ModuleId};
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::MessageEnvelope;
use rustclamp_worker::{
    DeadReason, Delivery, HandlerDeclaration, HandlerRegistry, HandlerTarget, Outcome, RetryPolicy,
    WorkerHandlers,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::error::Error;
use std::time::{Duration, SystemTime};

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

    let handler = HandlerDeclaration::typed(SUBJECT, 1, |email: EmailPayload, _| async move {
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

fn retry_delay(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(5),
        3 => Duration::from_secs(15),
        _ => Duration::from_secs(30),
    }
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

async fn handle_message(
    registry: &HandlerRegistry,
    envelope: MessageEnvelope,
    attempts: i64,
) -> DeliveryOutcome {
    let policy = RetryPolicy::new(MAX_ATTEMPTS as u32, retry_delay);
    let attempt = u32::try_from(attempts).unwrap_or(u32::MAX);
    #[cfg(feature = "tracing")]
    let (correlation_id, message_id) = (envelope.correlation_id.clone(), envelope.id.clone());
    let delivery = registry.deliver(
        Delivery {
            message: envelope,
            attempt,
        },
        &policy,
        &SystemClock,
    );
    #[cfg(feature = "tracing")]
    let delivery = {
        use tracing::Instrument;
        delivery.instrument(tracing::info_span!(
            "worker.delivery",
            %correlation_id,
            %message_id,
            attempt
        ))
    };
    let Ok(outcome) = tokio::time::timeout(MAX_HANDLER_TIME, delivery).await else {
        return DeliveryOutcome::DeadLetter {
            classification: "unknown_outcome",
            reason: "handler timed out; a remote side effect may have occurred".into(),
        };
    };
    match outcome {
        Outcome::Done(_) => DeliveryOutcome::Ack,
        Outcome::Retry(delay) => DeliveryOutcome::Retry(delay),
        Outcome::DeadLetter {
            reason: DeadReason::NoHandler,
            ..
        } => DeliveryOutcome::Reject,
        Outcome::DeadLetter { reason, error } => DeliveryOutcome::DeadLetter {
            classification: match reason {
                DeadReason::Expired => "expired",
                DeadReason::RetryExhausted => "retry_exhausted",
                DeadReason::UnknownOutcome => "unknown_outcome",
                DeadReason::Permanent | DeadReason::InvalidPayload | DeadReason::NoHandler => {
                    "permanent_failure"
                }
            },
            reason: error.map_or_else(
                || "message deadline passed before execution".into(),
                |e| e.to_string(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustclamp_worker::HandlerFailure;

    fn envelope() -> MessageEnvelope {
        MessageEnvelope {
            id: "test-email".into(),
            name: SUBJECT.into(),
            schema_version: 1,
            correlation_id: "test".into(),
            causation_id: None,
            deadline_unix_ms: None,
            payload: json!({"to":"a@example.test","subject":"test","body":"test"}),
        }
    }

    fn registry(failure: HandlerFailure) -> HandlerRegistry {
        TargetComposition::<HandlerTarget, WorkerHandlers>::new(vec![(
            EMAIL_MODULE,
            HandlerDeclaration::new(SUBJECT, 1, move |_delivery: Delivery| {
                let failure = match &failure {
                    HandlerFailure::Retryable(error) => {
                        HandlerFailure::Retryable(std::io::Error::other(error.to_string()).into())
                    }
                    HandlerFailure::Permanent(error) => {
                        HandlerFailure::Permanent(std::io::Error::other(error.to_string()).into())
                    }
                    HandlerFailure::UnknownOutcome(error) => HandlerFailure::UnknownOutcome(
                        std::io::Error::other(error.to_string()).into(),
                    ),
                };
                async move { Err(failure) }
            }),
        )])
        .build(Some(&HandlerTarget))
        .expect("test handler is valid")
        .expect("worker target is selected")
    }

    #[test]
    fn retry_backoff_is_bounded_and_exhaustion_dead_letters() {
        assert_eq!(retry_delay(1), Duration::from_secs(1));
        assert_eq!(retry_delay(2), Duration::from_secs(5));
        assert_eq!(retry_delay(3), Duration::from_secs(15));
        assert_eq!(retry_delay(4), Duration::from_secs(30));
        assert_eq!(retry_delay(99), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn handler_failure_classes_have_distinct_delivery_outcomes() {
        let retry = registry(HandlerFailure::Retryable(
            std::io::Error::other("temporary").into(),
        ));
        assert!(matches!(
            handle_message(&retry, envelope(), 1).await,
            DeliveryOutcome::Retry(_)
        ));
        assert!(matches!(
            handle_message(&retry, envelope(), MAX_ATTEMPTS).await,
            DeliveryOutcome::DeadLetter {
                classification: "retry_exhausted",
                ..
            }
        ));

        let permanent = registry(HandlerFailure::Permanent(
            std::io::Error::other("invalid").into(),
        ));
        assert!(matches!(
            handle_message(&permanent, envelope(), 1).await,
            DeliveryOutcome::DeadLetter {
                classification: "permanent_failure",
                ..
            }
        ));

        let unknown = registry(HandlerFailure::UnknownOutcome(
            std::io::Error::other("ambiguous").into(),
        ));
        assert!(matches!(
            handle_message(&unknown, envelope(), 1).await,
            DeliveryOutcome::DeadLetter {
                classification: "unknown_outcome",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn expired_messages_and_missing_routes_do_not_run_handlers() {
        let registry = registry(HandlerFailure::Retryable(
            std::io::Error::other("unused").into(),
        ));
        let mut expired = envelope();
        expired.deadline_unix_ms = Some(0);
        assert!(matches!(
            handle_message(&registry, expired, 1).await,
            DeliveryOutcome::DeadLetter {
                classification: "expired",
                ..
            }
        ));

        let mut unknown_route = envelope();
        unknown_route.name = "missing".into();
        assert!(matches!(
            handle_message(&registry, unknown_route, 1).await,
            DeliveryOutcome::Reject
        ));
    }
}
