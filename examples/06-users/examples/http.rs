//! HTTP adapter over the same Users operations used by the console and tests.

use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use rustclamp_core::{Module, ModuleId};
use rustclamp_example_users::{
    MemoryUserRepository, OperationContext, USERS_HTTP_MODULE, UserId, UserRepository, Users,
    UsersError, UsersErrorKind,
};
use rustclamp_http::ecosystem as axum;
use rustclamp_http::{
    HttpRoute, HttpRoutes, PrincipalResolver, Public, Representation, RequestContext, negotiate,
    with_body_limit, with_request_context,
};
use rustclamp_kernel::TargetComposition;

/// Public HTTP integration that contributes Users routes.
pub struct UsersHttp;
impl Module for UsersHttp {
    const ID: ModuleId = USERS_HTTP_MODULE;
}

#[derive(serde::Deserialize)]
struct CreateUserRequest {
    name: String,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct UserResponse {
    id: i64,
    name: String,
}

impl From<rustclamp_example_users::User> for UserResponse {
    fn from(user: rustclamp_example_users::User) -> Self {
        Self {
            id: user.id.0,
            name: user.name,
        }
    }
}

struct StateData<R> {
    users: Arc<tokio::sync::Mutex<Users<R>>>,
}

impl<R> Clone for StateData<R> {
    fn clone(&self) -> Self {
        Self {
            users: self.users.clone(),
        }
    }
}

/// Compiles the Users HTTP contribution before any resource is initialized.
pub fn users_router<R: UserRepository + 'static>(
    users: Arc<tokio::sync::Mutex<Users<R>>>,
) -> Result<Router, String> {
    let state = StateData { users };
    let declarations = vec![
        (
            UsersHttp::ID,
            HttpRoute::<Public>::new("/users", post(create_user::<R>).with_state(state.clone())),
        ),
        (
            UsersHttp::ID,
            HttpRoute::<Public>::new("/users/{id}", get(get_user::<R>).with_state(state.clone())),
        ),
        (
            UsersHttp::ID,
            HttpRoute::<Public>::new(
                "/users/{id}/stream",
                get(stream_user::<R>).with_state(state),
            ),
        ),
    ];
    let target = HttpRoutes::<Public>::new();
    let compiled = TargetComposition::<HttpRoutes<Public>, Public>::new(declarations)
        .build(Some(&target))
        .map_err(|error| format!("route compilation failed: {error:?}"))?
        .ok_or_else(|| "HTTP target unexpectedly produced no router".to_owned())?;
    Ok(Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(compiled))
}

/// Adds process-selected authentication, request limits, and tracing middleware.
pub fn application_router<R: UserRepository + 'static>(
    users: Arc<tokio::sync::Mutex<Users<R>>>,
) -> Result<Router, String> {
    let router = users_router(users)?;
    let router = with_request_context(router, Arc::new(DemoIdentity), Duration::from_secs(10));
    let router = with_body_limit(router, 16 * 1024);
    Ok(router.layer(rustclamp_http::tower_http::trace::TraceLayer::new_for_http()))
}

struct DemoIdentity;
impl PrincipalResolver for DemoIdentity {
    fn resolve(&self, headers: &HeaderMap) -> Result<Option<String>, String> {
        match headers
            .get(header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
        {
            Some("Bearer demo") => Ok(Some("demo-user".to_owned())),
            Some(_) => Err("invalid demo credential".to_owned()),
            None => Ok(None),
        }
    }
}

fn operation_context(context: &RequestContext) -> OperationContext {
    let cancellation = context.cancellation().clone();
    OperationContext::new(
        context.principal().map(str::to_owned),
        context.tenant().unwrap_or("default"),
        context.correlation_id(),
        context.deadline().into_std(),
        move || cancellation.is_cancelled(),
    )
}

async fn create_user<R: UserRepository + 'static>(
    State(state): State<StateData<R>>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<CreateUserRequest>,
) -> Response {
    let operation = operation_context(&context);
    match state
        .users
        .lock()
        .await
        .create(&operation, &input.name)
        .await
    {
        Ok(user) => (StatusCode::CREATED, Json(UserResponse::from(user))).into_response(),
        Err(error) => present_error(error),
    }
}

async fn get_user<R: UserRepository + 'static>(
    State(state): State<StateData<R>>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let operation = operation_context(&context);
    let user = match state.users.lock().await.get(&operation, UserId(id)).await {
        Ok(user) => user,
        Err(error) => return present_error(error),
    };
    let representation = match negotiate(
        headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok()),
    ) {
        Ok(value) => value,
        Err(error) => return (StatusCode::NOT_ACCEPTABLE, error.to_string()).into_response(),
    };
    present_user(user, representation)
}

async fn stream_user<R: UserRepository + 'static>(
    State(state): State<StateData<R>>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<i64>,
) -> Response {
    let operation = operation_context(&context);
    let user = match state.users.lock().await.get(&operation, UserId(id)).await {
        Ok(user) => user,
        Err(error) => return present_error(error),
    };
    let chunks = vec![
        Ok::<_, io::Error>(Bytes::from(format!(
            "event: user\ndata: {}\n\n",
            serde_json::to_string(&UserResponse::from(user)).expect("serializable user")
        ))),
        Ok(Bytes::from("event: complete\ndata: true\n\n")),
    ];
    let mut response = rustclamp_http::streaming_body(stream::iter(chunks)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn present_user(user: rustclamp_example_users::User, representation: Representation) -> Response {
    let output = UserResponse::from(user);
    let (content_type, body) = match representation {
        Representation::Json => (
            "application/json",
            serde_json::to_vec(&output).expect("serializable user"),
        ),
        Representation::Html => (
            "text/html; charset=utf-8",
            format!(
                "<article><h1>{}</h1><span>{}</span></article>",
                escape(&output.name),
                output.id
            )
            .into_bytes(),
        ),
        Representation::Xml => (
            "application/xml",
            format!(
                "<user><id>{}</id><name>{}</name></user>",
                output.id,
                escape(&output.name)
            )
            .into_bytes(),
        ),
        Representation::Binary => {
            let mut bytes = output.id.to_be_bytes().to_vec();
            bytes.extend_from_slice(output.name.as_bytes());
            ("application/octet-stream", bytes)
        }
        Representation::Stream => {
            let event = format!(
                "data: {}\n\n",
                serde_json::to_string(&output).expect("serializable user")
            );
            let body = rustclamp_http::streaming_body(stream::iter(vec![Ok::<_, io::Error>(
                Bytes::from(event),
            )]));
            let mut response = body.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            return response;
        }
    };
    let mut response = Body::from(body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn escape(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            match ch {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&apos;",
                _ => return ch.to_string(),
            }
            .to_owned()
        })
        .collect()
}

fn present_error(error: UsersError) -> Response {
    let status = match error.kind() {
        UsersErrorKind::InvalidName => StatusCode::BAD_REQUEST,
        UsersErrorKind::NotFound => StatusCode::NOT_FOUND,
        UsersErrorKind::Unauthorized => StatusCode::FORBIDDEN,
        UsersErrorKind::Cancelled | UsersErrorKind::DeadlineExceeded => StatusCode::REQUEST_TIMEOUT,
        UsersErrorKind::Storage => StatusCode::INTERNAL_SERVER_ERROR,
    };
    #[cfg(feature = "tracing")]
    tracing::error!(error = ?error, "Users operation failed");
    rustclamp_http::HttpError::new(error, status, "Users operation failed").into_response()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt().with_env_filter("info").init();
    let users = Arc::new(tokio::sync::Mutex::new(Users::new(
        MemoryUserRepository::default(),
    )));
    // Routes are validated and compiled before binding the process-owned listener.
    let router = application_router(users)?;
    let address = std::env::var("HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let signal = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(true);
    });
    eprintln!("listening on {}", listener.local_addr()?);
    let result = rustclamp_http::serve(
        listener,
        router,
        rustclamp_http::ShutdownReceiver(shutdown_rx),
        Duration::from_secs(5),
    )
    .await;
    signal.abort();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rustclamp_example_users::{MemoryUserRepository, Users};
    use tower::ServiceExt;

    fn headers() -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/users")
            .header(header::AUTHORIZATION, "Bearer demo")
            .header("x-tenant-id", "tenant-a")
            .header("x-correlation-id", "http-test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"Ada <Lovelace>"}"#))
            .unwrap()
    }

    #[tokio::test]
    async fn one_domain_operation_is_reused_for_http_create_and_representation() {
        let repository = MemoryUserRepository::default();
        let probe = repository.probe();
        let state = Arc::new(tokio::sync::Mutex::new(Users::new(repository)));
        let router = application_router(state).unwrap();
        let created = router.clone().oneshot(headers()).await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let user: UserResponse =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(user.name, "Ada <Lovelace>");

        let request = Request::builder()
            .uri(format!("/users/{}", user.id))
            .header(header::AUTHORIZATION, "Bearer demo")
            .header("x-tenant-id", "tenant-a")
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("Ada &lt;Lovelace&gt;"));
        assert_eq!(probe.calls(), 2);

        let response = router
            .oneshot(headers().map(|_| Body::from("{ invalid json")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            probe.calls(),
            2,
            "decoder rejection must precede domain operations"
        );
    }

    #[tokio::test]
    async fn all_representations_share_one_semantic_user_result() {
        let user = rustclamp_example_users::User {
            id: UserId(7),
            tenant: "t".into(),
            name: "A&B".into(),
        };
        for (representation, media, expected) in [
            (Representation::Json, "application/json", "A&B"),
            (Representation::Html, "text/html; charset=utf-8", "A&amp;B"),
            (Representation::Xml, "application/xml", "A&amp;B"),
            (Representation::Binary, "application/octet-stream", "A&B"),
            (Representation::Stream, "text/event-stream", "A&B"),
        ] {
            let response = present_user(user.clone(), representation);
            assert_eq!(response.headers()[header::CONTENT_TYPE], media);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert!(
                bytes
                    .windows(expected.len())
                    .any(|window| window == expected.as_bytes())
            );
        }
        let _ = Extension::<RequestContext>;
    }

    #[tokio::test]
    async fn tenant_and_authorization_are_enforced_by_the_operation() {
        let repo = MemoryUserRepository::default();
        let mut users = Users::new(repo);
        let context = OperationContext::new(
            Some("demo-user".into()),
            "tenant-a",
            "r1",
            std::time::Instant::now() + Duration::from_secs(1),
            || false,
        );
        let created = users.create(&context, "Ada").await.unwrap();
        let other = OperationContext::new(
            Some("demo-user".into()),
            "tenant-b",
            "r2",
            std::time::Instant::now() + Duration::from_secs(1),
            || false,
        );
        assert_eq!(
            users.get(&other, created.id).await.unwrap_err().kind(),
            UsersErrorKind::NotFound
        );
        let anonymous = OperationContext::new(
            None,
            "tenant-a",
            "r3",
            std::time::Instant::now() + Duration::from_secs(1),
            || false,
        );
        assert_eq!(
            users.get(&anonymous, created.id).await.unwrap_err().kind(),
            UsersErrorKind::Unauthorized
        );
    }
}
