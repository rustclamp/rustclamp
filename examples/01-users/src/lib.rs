//! One Users operation shared by direct tests, console, HTTP, and PostgreSQL.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

/// Stable Users domain module identity.
pub const USERS_MODULE: rustclamp_core::ModuleId =
    rustclamp_core::ModuleId::new("example.phase6.users");
/// Stable Users HTTP integration identity.
pub const USERS_HTTP_MODULE: rustclamp_core::ModuleId =
    rustclamp_core::ModuleId::new("example.phase6.users-http");

/// Domain user identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UserId(pub i64);

/// Domain user result, with no transport-specific fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct User {
    /// Stable user identity.
    pub id: UserId,
    /// Owning tenant identity.
    pub tenant: String,
    /// Display name.
    pub name: String,
}

/// Per-operation context supplied by the selected interface boundary.
#[derive(Clone)]
pub struct OperationContext {
    principal: Option<String>,
    tenant: String,
    correlation_id: String,
    deadline: Instant,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl OperationContext {
    /// Creates an operation context from authenticated identity and request metadata.
    pub fn new(
        principal: Option<String>,
        tenant: impl Into<String>,
        correlation_id: impl Into<String>,
        deadline: Instant,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            principal,
            tenant: tenant.into(),
            correlation_id: correlation_id.into(),
            deadline,
            cancelled: Arc::new(cancelled),
        }
    }
    /// Returns the authenticated principal.
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }
    /// Returns the selected tenant.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    /// Returns the operation correlation identity.
    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }
    /// Returns the monotonic operation deadline.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    /// Reports whether the request boundary cancelled this operation.
    pub fn is_cancelled(&self) -> bool {
        (self.cancelled)()
    }
}

/// Classified domain outcome, independent of an HTTP status or CLI exit code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsersErrorKind {
    /// The requested name is empty after normalization.
    InvalidName,
    /// The user does not exist in the selected tenant.
    NotFound,
    /// The operation lacks an authenticated principal.
    Unauthorized,
    /// The request or execution was cancelled.
    Cancelled,
    /// The operation deadline elapsed.
    DeadlineExceeded,
    /// Persistence failed; the typed source remains attached.
    Storage,
}

/// Semantic Users operation failure retaining an optional typed infrastructure source.
pub struct UsersError {
    kind: UsersErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl UsersError {
    /// Creates a semantic domain error.
    pub fn new(kind: UsersErrorKind) -> Self {
        Self { kind, source: None }
    }
    /// Maps an infrastructure source while preserving its concrete error value.
    pub fn storage(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind: UsersErrorKind::Storage,
            source: Some(Box::new(source)),
        }
    }
    /// Returns the semantic error category.
    pub const fn kind(&self) -> UsersErrorKind {
        self.kind
    }
}

impl fmt::Debug for UsersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsersError")
            .field("kind", &self.kind)
            .field("source", &self.source.as_ref().map(ToString::to_string))
            .finish()
    }
}
impl fmt::Display for UsersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Users operation failed: {:?}", self.kind)
    }
}
impl Error for UsersError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as _)
    }
}

/// Owned future returned by a repository port without exposing a database driver.
pub type RepositoryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, UsersError>> + Send + 'a>>;

/// Domain-owned persistence port; adapters may use memory or PostgreSQL.
pub trait UserRepository: Send {
    /// Finds a user within one tenant.
    fn find<'a>(&'a mut self, tenant: &'a str, id: UserId) -> RepositoryFuture<'a, Option<User>>;
    /// Creates a user within one tenant.
    fn create<'a>(&'a mut self, tenant: &'a str, name: &'a str) -> RepositoryFuture<'a, User>;
}

/// User behavior independent of transport and storage implementation.
pub struct Users<R> {
    repository: R,
}

impl<R: UserRepository> Users<R> {
    /// Creates the Users operation service around one repository adapter.
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    /// Returns a tenant-scoped user after policy and request checks.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, context), fields(tenant = context.tenant(), correlation_id = context.correlation_id())))]
    pub async fn get(
        &mut self,
        context: &OperationContext,
        id: UserId,
    ) -> Result<User, UsersError> {
        check_context(context)?;
        if context.principal().is_none() {
            return Err(UsersError::new(UsersErrorKind::Unauthorized));
        }
        self.repository
            .find(context.tenant(), id)
            .await?
            .ok_or_else(|| UsersError::new(UsersErrorKind::NotFound))
    }

    /// Creates a user after validating input and operation context.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, context, name), fields(tenant = context.tenant(), correlation_id = context.correlation_id())))]
    pub async fn create(
        &mut self,
        context: &OperationContext,
        name: &str,
    ) -> Result<User, UsersError> {
        check_context(context)?;
        if context.principal().is_none() {
            return Err(UsersError::new(UsersErrorKind::Unauthorized));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(UsersError::new(UsersErrorKind::InvalidName));
        }
        self.repository.create(context.tenant(), name).await
    }

    /// Consumes the service and returns its repository, for scoped transaction completion.
    pub fn into_repository(self) -> R {
        self.repository
    }
}

fn check_context(context: &OperationContext) -> Result<(), UsersError> {
    if context.is_cancelled() {
        return Err(UsersError::new(UsersErrorKind::Cancelled));
    }
    if Instant::now() >= context.deadline() {
        return Err(UsersError::new(UsersErrorKind::DeadlineExceeded));
    }
    Ok(())
}

#[derive(Default)]
struct MemoryState {
    next_id: i64,
    users: BTreeMap<(String, UserId), User>,
}

/// Thread-safe in-memory UserRepository used by fast tests and console.
#[derive(Clone, Default)]
pub struct MemoryUserRepository {
    state: Arc<Mutex<MemoryState>>,
    probe: RepositoryProbe,
}

impl MemoryUserRepository {
    /// Returns a shared operation counter for boundary tests.
    pub fn probe(&self) -> RepositoryProbe {
        self.probe.clone()
    }
}

/// Observation handle for verifying an interface rejects malformed input early.
#[derive(Clone, Default)]
pub struct RepositoryProbe(Arc<AtomicUsize>);

impl RepositoryProbe {
    /// Returns the number of repository operations that reached persistence.
    pub fn calls(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl UserRepository for MemoryUserRepository {
    fn find<'a>(&'a mut self, tenant: &'a str, id: UserId) -> RepositoryFuture<'a, Option<User>> {
        Box::pin(async move {
            self.probe.0.fetch_add(1, Ordering::Relaxed);
            let state = self.state.lock().expect("memory repo lock");
            Ok(state.users.get(&(tenant.to_owned(), id)).cloned())
        })
    }
    fn create<'a>(&'a mut self, tenant: &'a str, name: &'a str) -> RepositoryFuture<'a, User> {
        Box::pin(async move {
            self.probe.0.fetch_add(1, Ordering::Relaxed);
            let mut state = self.state.lock().expect("memory repo lock");
            state.next_id += 1;
            let user = User {
                id: UserId(state.next_id),
                tenant: tenant.to_owned(),
                name: name.to_owned(),
            };
            state
                .users
                .insert((tenant.to_owned(), user.id), user.clone());
            Ok(user)
        })
    }
}

/// Console command input decoded before any operation is invoked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsoleCommand {
    /// Create a user with the supplied display name.
    Create {
        /// Requested display name.
        name: String,
    },
    /// Read a user by ID.
    Get {
        /// Requested user identity.
        id: UserId,
    },
}

/// Parses the tiny example console syntax without invoking the domain service.
pub fn parse_console(args: &[String]) -> Result<ConsoleCommand, String> {
    match args {
        [command, name] if command == "create" => Ok(ConsoleCommand::Create { name: name.clone() }),
        [command, id] if command == "get" => id
            .parse::<i64>()
            .map(|id| ConsoleCommand::Get { id: UserId(id) })
            .map_err(|_| "invalid user id".to_owned()),
        _ => Err("usage: users-console create <name> | get <id>".to_owned()),
    }
}

/// PostgreSQL-backed repository scoped to one execution transaction.
/// Builds the Users schema migration for the selected Primary database.
#[cfg(feature = "postgres")]
pub fn pg_users_migrations() -> Result<
    rustclamp_postgres::Migrations<rustclamp_postgres::Primary>,
    rustclamp_postgres::MigrationPlanError,
> {
    rustclamp_postgres::Migrations::new(vec![rustclamp_postgres::Migration {
        version: 6002,
        description: "Users table and tenant index",
        sql: "CREATE TABLE IF NOT EXISTS rustclamp_users (id BIGSERIAL PRIMARY KEY, tenant TEXT NOT NULL, name TEXT NOT NULL); CREATE INDEX IF NOT EXISTS rustclamp_users_tenant_idx ON rustclamp_users (tenant, id)",
    }])
}

/// Infrastructure adapter that privately holds a transaction for one execution.
#[cfg(feature = "postgres")]
pub struct PgUserRepository {
    transaction:
        Option<rustclamp_postgres::sqlx::Transaction<'static, rustclamp_postgres::sqlx::Postgres>>,
}

#[cfg(feature = "postgres")]
impl PgUserRepository {
    /// Begins one execution-scoped transaction on the qualified pool.
    pub async fn begin(
        database: &rustclamp_postgres::Database<rustclamp_postgres::Primary>,
    ) -> Result<Self, UsersError> {
        let transaction = database.begin().await.map_err(UsersError::storage)?;
        Ok(Self {
            transaction: Some(transaction),
        })
    }
    /// Commits the execution transaction.
    pub async fn commit(mut self) -> Result<(), UsersError> {
        self.transaction
            .take()
            .expect("active transaction")
            .commit()
            .await
            .map_err(UsersError::storage)
    }
    /// Rolls back the execution transaction.
    pub async fn rollback(mut self) -> Result<(), UsersError> {
        self.transaction
            .take()
            .expect("active transaction")
            .rollback()
            .await
            .map_err(UsersError::storage)
    }
}

#[cfg(feature = "postgres")]
impl UserRepository for PgUserRepository {
    fn find<'a>(&'a mut self, tenant: &'a str, id: UserId) -> RepositoryFuture<'a, Option<User>> {
        Box::pin(async move {
            let tx = self.transaction.as_mut().expect("active transaction");
            let row = rustclamp_postgres::sqlx::query_as::<_, (i64, String, String)>(
                "SELECT id, tenant, name FROM rustclamp_users WHERE tenant = $1 AND id = $2",
            )
            .bind(tenant)
            .bind(id.0)
            .fetch_optional(&mut **tx)
            .await
            .map_err(UsersError::storage)?;
            Ok(row.map(|(id, tenant, name)| User {
                id: UserId(id),
                tenant,
                name,
            }))
        })
    }
    fn create<'a>(&'a mut self, tenant: &'a str, name: &'a str) -> RepositoryFuture<'a, User> {
        Box::pin(async move {
            let tx = self.transaction.as_mut().expect("active transaction");
            let row = rustclamp_postgres::sqlx::query_as::<_, (i64, String, String)>("INSERT INTO rustclamp_users (tenant, name) VALUES ($1, $2) RETURNING id, tenant, name")
                .bind(tenant).bind(name).fetch_one(&mut **tx).await.map_err(UsersError::storage)?;
            Ok(User {
                id: UserId(row.0),
                tenant: row.1,
                name: row.2,
            })
        })
    }
}
