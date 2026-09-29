//! Runs the email handler in its own process and acknowledges successful work.

use async_nats::jetstream::{self, AckKind, consumer::pull::Config, stream};
use futures_util::StreamExt;
use rustclamp_core::ModuleId;
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::MessageEnvelope;
use rustclamp_worker::{HandlerDeclaration, HandlerError, HandlerTarget, WorkerHandlers};
use serde::Deserialize;
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
    DeadLetter,
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
            .map_err(|error| Box::new(error) as HandlerError)?;
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
            Err(_) => DeliveryOutcome::DeadLetter,
            Ok(envelope) => match registry.dispatch(envelope).await {
                Ok(()) => DeliveryOutcome::Ack,
                Err(rustclamp_worker::DispatchError::NoHandler { .. }) => DeliveryOutcome::Reject,
                Err(rustclamp_worker::DispatchError::Decode(_)) => DeliveryOutcome::DeadLetter,
                Err(rustclamp_worker::DispatchError::Handler(_)) if attempts >= MAX_ATTEMPTS => {
                    DeliveryOutcome::DeadLetter
                }
                Err(rustclamp_worker::DispatchError::Handler(_)) => {
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
            DeliveryOutcome::DeadLetter => {
                let published = jetstream
                    .publish(DEAD_LETTER_SUBJECT, message.payload.clone())
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
