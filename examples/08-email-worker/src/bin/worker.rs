//! Runs the email handler in its own process and acknowledges successful work.

use async_nats::jetstream::{self, consumer::pull::Config, stream};
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
const EMAIL_MODULE: ModuleId = ModuleId::new("example.phase7.email-worker");

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
            subjects: vec![SUBJECT.into()],
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
        let envelope: MessageEnvelope = serde_json::from_slice(&message.payload)?;
        registry.dispatch(envelope).await?;
        message.ack().await?;
    }
    Ok(())
}
