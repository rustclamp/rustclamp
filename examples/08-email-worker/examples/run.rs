//! Runs the API-to-worker email path in one development composition.

use rustclamp_core::ModuleId;
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::{InMemoryMessageBus, MessageBus, MessageEnvelope};
use rustclamp_worker::{HandlerDeclaration, HandlerFailure, HandlerTarget, WorkerHandlers};
use serde::Deserialize;
use serde_json::json;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

const EMAIL_MODULE: ModuleId = ModuleId::new("example.phase7.email-worker");
const SEND_EMAIL: &str = "email.send";

type PortFuture<'a> = Pin<Box<dyn Future<Output = Result<(), EmailError>> + Send + 'a>>;

/// Domain request to send one email.
#[derive(Clone)]
struct Email {
    to: String,
    subject: String,
    body: String,
}

/// Domain-owned port used by the API to request email delivery.
trait EmailSender: Send + Sync {
    fn send(&self, email: Email) -> PortFuture<'_>;
}

#[derive(Debug)]
enum EmailError {
    QueueUnavailable,
    DeliveryFailed,
}

impl std::fmt::Display for EmailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueUnavailable => f.write_str("email queue is unavailable"),
            Self::DeliveryFailed => f.write_str("email delivery failed"),
        }
    }
}

impl Error for EmailError {}

struct QueuedEmailSender {
    bus: Arc<InMemoryMessageBus>,
    sequence: AtomicU64,
    correlation_id: String,
}

impl EmailSender for QueuedEmailSender {
    fn send(&self, email: Email) -> PortFuture<'_> {
        let message = MessageEnvelope {
            id: format!("email-{}", self.sequence.fetch_add(1, Ordering::Relaxed)),
            name: SEND_EMAIL.to_owned(),
            schema_version: 1,
            correlation_id: self.correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "to": email.to,
                "subject": email.subject,
                "body": email.body,
            }),
        };
        let bus = Arc::clone(&self.bus);
        Box::pin(async move {
            bus.publish(message)
                .await
                .map_err(|_| EmailError::QueueUnavailable)
        })
    }
}

#[derive(Deserialize)]
struct EmailPayload {
    to: String,
    subject: String,
    body: String,
}

impl From<EmailPayload> for Email {
    fn from(payload: EmailPayload) -> Self {
        Self {
            to: payload.to,
            subject: payload.subject,
            body: payload.body,
        }
    }
}

trait MailGateway: Send + Sync {
    fn deliver(&self, email: Email) -> Result<(), EmailError>;
}

#[derive(Clone, Default)]
struct MemoryMailGateway(Arc<Mutex<Vec<Email>>>);

impl MailGateway for MemoryMailGateway {
    fn deliver(&self, email: Email) -> Result<(), EmailError> {
        self.0
            .lock()
            .map_err(|_| EmailError::DeliveryFailed)?
            .push(email);
        Ok(())
    }
}

fn send_email(email: Email, gateway: &dyn MailGateway) -> Result<(), EmailError> {
    gateway.deliver(email)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let bus = Arc::new(InMemoryMessageBus::new(8)?);
    let gateway = MemoryMailGateway::default();
    let worker_gateway = gateway.clone();
    let handler = HandlerDeclaration::new(SEND_EMAIL, 1, move |message| {
        let gateway = worker_gateway.clone();
        async move {
            let payload: EmailPayload = serde_json::from_value(message.payload)
                .map_err(|error| HandlerFailure::Permanent(Box::new(error)))?;
            send_email(payload.into(), &gateway)
                .map_err(|error| HandlerFailure::Permanent(Box::new(error)))
        }
    });
    let registry =
        TargetComposition::<HandlerTarget, WorkerHandlers>::new(vec![(EMAIL_MODULE, handler)])
            .build(Some(&HandlerTarget))
            .map_err(|error| format!("worker handler assembly failed: {error:?}"))?
            .expect("selected target produces a registry");

    let api = QueuedEmailSender {
        bus: Arc::clone(&bus),
        sequence: AtomicU64::new(1),
        correlation_id: "signup-1".to_owned(),
    };
    api.send(Email {
        to: "ada@example.test".to_owned(),
        subject: "Welcome".to_owned(),
        body: "Your account is ready.".to_owned(),
    })
    .await?;

    let message = bus.receive().await?.ok_or("email queue was empty")?;
    registry.dispatch(message).await?;
    println!(
        "delivered {} email(s)",
        gateway.0.lock().map_err(|_| "mailbox poisoned")?.len()
    );
    Ok(())
}
