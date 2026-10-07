//! One request through API and Worker: framework-served `/metrics`, and one
//! correlation id in the traces on both sides.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Extension, Json};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use rustclamp_core::ModuleId;
use rustclamp_http::ecosystem as axum;
use rustclamp_http::{PrincipalResolver, RequestContext, with_metrics, with_request_context};
use rustclamp_kernel::TargetComposition;
use rustclamp_messaging::{InMemoryMessageBus, MessageBus, MessageEnvelope};
use rustclamp_worker::{
    Delivery, HandlerDeclaration, HandlerFailure, HandlerRegistry, HandlerTarget, WorkerHandlers,
};
use serde::Deserialize;
use tower::ServiceExt;
use tracing::Instrument;

const SEND_EMAIL: &str = "email.send";

#[derive(Deserialize)]
struct Signup {
    email: String,
}

/// No sign-in here: every request is anonymous.
struct Anonymous;

impl PrincipalResolver for Anonymous {
    fn resolve(&self, _: &HeaderMap) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The API and the Worker, sharing an in-memory bus where two processes
/// would share a broker.
struct App {
    api: Router,
    bus: Arc<InMemoryMessageBus>,
    worker: HandlerRegistry,
}

fn app() -> Result<App, Box<dyn Error>> {
    let bus = Arc::new(InMemoryMessageBus::new(8)?);
    let sent = Arc::new(AtomicU64::new(0));

    // Worker: its span carries the envelope's correlation id.
    let counter = Arc::clone(&sent);
    let handler =
        HandlerDeclaration::typed(SEND_EMAIL, 1, move |signup: Signup, delivery: Delivery| {
            let counter = Arc::clone(&counter);
            let span =
                tracing::info_span!("worker", correlation_id = %delivery.message.correlation_id);
            async move {
                tracing::info!(to = %signup.email, "sent welcome email");
                counter.fetch_add(1, Ordering::Relaxed);
                Ok::<_, HandlerFailure>(())
            }
            .instrument(span)
        });
    let worker = TargetComposition::<HandlerTarget, WorkerHandlers>::new(vec![(
        ModuleId::new("example.observability.email"),
        handler,
    )])
    .build(Some(&HandlerTarget))
    .map_err(|error| format!("worker handler assembly failed: {error:?}"))?
    .expect("selected target produces a registry");

    // API: the request's id becomes the message's correlation id.
    let queue = Arc::clone(&bus);
    let signup = post(
        move |Extension(context): Extension<RequestContext>, Json(signup): Json<Signup>| {
            let queue = Arc::clone(&queue);
            let span = tracing::info_span!("api", correlation_id = %context.correlation_id());
            async move {
                let message = MessageEnvelope {
                    correlation_id: context.correlation_id().to_owned(),
                    ..MessageEnvelope::new(
                        SEND_EMAIL,
                        1,
                        serde_json::json!({ "email": signup.email }),
                    )
                };
                match queue.publish(message).await {
                    Ok(()) => {
                        tracing::info!("queued welcome email");
                        StatusCode::ACCEPTED
                    }
                    Err(_) => StatusCode::SERVICE_UNAVAILABLE,
                }
            }
            .instrument(span)
        },
    );
    let api = Router::new().route("/signup", signup);
    let api = with_request_context(api, Arc::new(Anonymous), Duration::from_secs(10));
    // The app's own counters follow the framework's request metrics.
    let api = with_metrics(api, "/metrics", move || {
        format!(
            "# TYPE emails_sent_total counter\nemails_sent_total {}\n",
            sent.load(Ordering::Relaxed)
        )
    });
    Ok(App { api, bus, worker })
}

/// Sends one signup, lets the Worker handle what it queued, then scrapes
/// `/metrics`. Returns the request id the framework assigned, and the scrape.
async fn one_request(app: &App) -> Result<(String, String), Box<dyn Error>> {
    let response = app
        .api
        .clone()
        .oneshot(
            Request::post("/signup")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"email":"ada@example.test"}"#))?,
        )
        .await?;
    if response.status() != StatusCode::ACCEPTED {
        return Err(format!("signup answered {}", response.status()).into());
    }
    let request_id = response.headers()["x-request-id"].to_str()?.to_owned();

    let message = app.bus.receive().await?.ok_or("the queue was empty")?;
    app.worker
        .dispatch(Delivery {
            message,
            attempt: 1,
        })
        .await?;

    let scrape = app
        .api
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty())?)
        .await?;
    let metrics = String::from_utf8(to_bytes(scrape.into_body(), 64 * 1024).await?.to_vec())?;
    Ok((request_id, metrics))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().with_target(false).init();
    let (_, metrics) = one_request(&app()?).await?;
    print!("{metrics}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn one_correlation_id_reaches_the_worker_and_metrics_count_the_request() {
        let logs = Captured::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let _trace = tracing::subscriber::set_default(subscriber);

        let (request_id, metrics) = one_request(&app().unwrap()).await.unwrap();

        assert!(
            metrics.contains("http_request_duration_seconds_count 1\n"),
            "{metrics}"
        );
        assert!(
            metrics.contains("http_server_errors_total 0\n"),
            "{metrics}"
        );
        assert!(metrics.contains("emails_sent_total 1\n"), "{metrics}");
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        let line = |message: &str| {
            logs.lines()
                .find(|line| line.contains(message))
                .unwrap_or_else(|| panic!("no {message:?} in {logs}"))
                .to_owned()
        };
        assert!(
            line("queued welcome email").contains(&format!("api{{correlation_id={request_id}}}"))
        );
        assert!(
            line("sent welcome email").contains(&format!("worker{{correlation_id={request_id}}}"))
        );
    }
}
