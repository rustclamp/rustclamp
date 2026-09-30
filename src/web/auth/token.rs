//! Bearer API tokens (ADR 0023): the token guard for APIs whose clients send
//! `Authorization: Bearer <token>` instead of a session cookie.
//!
//! The app owns the `api_tokens` table: `id`, `public_id` (unique), `name`,
//! `token_hash` and `role`, plus timestamps. A token is
//! `{public_id}.{secret}`; only a SHA-256 of the secret is stored, so a leaked
//! table cannot be used, and the token is shown once, by
//! [`Auth::issue_token`]. [`bearer`] resolves it to a [`Principal`] that
//! handlers read with [`Request::principal`], and [`token_role`] guards routes
//! by role through the same before rule as users.

use std::error::Error;

use super::{Auth, allows_role};
use crate::crypto::{constant_time_eq, sha256};
use crate::db::Db;
use crate::db::sqlite::params;
use crate::web::{Next, Request, Response, problem};

/// Who a valid bearer token speaks for. Not a [`User`](super::User): a token
/// has a name and a role, and needs no email or password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The token's `public_id`.
    pub token: String,
    /// What the token was issued for, such as `ci` or `mobile app`.
    pub name: String,
    /// One slug from the app's roles.
    pub role: String,
}

impl Auth {
    /// Issues a token named `name` with `role`, stored hashed. Returns the
    /// token; it cannot be read back.
    ///
    /// # Errors
    ///
    /// An unknown role or a database error.
    pub fn issue_token(
        &self,
        db: &Db,
        name: &str,
        role: &str,
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        self.known(role)?;
        let public_id = crate::uuid::Uuid::v7().to_string();
        let secret = crate::web::session::random_token();
        db.table("api_tokens").insert(
            &["public_id", "name", "token_hash", "role"],
            params![public_id, name, sha256(secret.as_bytes()), role],
        )?;
        Ok(format!("{public_id}.{secret}"))
    }

    /// Revokes the token with `public_id`; whether there was one.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn revoke_token(
        &self,
        db: &Db,
        public_id: &str,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        Ok(db
            .table("api_tokens")
            .where_eq("public_id", &public_id)
            .delete()?
            > 0)
    }

    /// Whether `principal` holds `role` or one above it, through the same
    /// before rule as [`at_least`](Self::at_least): `super-admin` always
    /// passes, `blocked` never does.
    pub fn principal_at_least(&self, principal: &Principal, role: &str) -> bool {
        allows_role(&principal.role, || {
            self.ranks_at_least(&principal.role, role)
        })
    }
}

/// The principal `token` speaks for, if it is a live token.
fn resolve(db: &Db, token: &str) -> Result<Option<Principal>, crate::db::sqlite::Error> {
    let Some((public_id, secret)) = token.split_once('.') else {
        return Ok(None);
    };
    let found = db
        .table("api_tokens")
        .where_eq("public_id", &public_id)
        .first(|row| {
            Ok((
                Principal {
                    token: row.get("public_id")?,
                    name: row.get("name")?,
                    role: row.get("role")?,
                },
                row.get::<_, String>("token_hash")?,
            ))
        })?;
    Ok(found
        .filter(|(_, hash)| constant_time_eq(hash.as_bytes(), sha256(secret.as_bytes()).as_bytes()))
        .map(|(principal, _)| principal)
        .filter(|principal| principal.role != "blocked"))
}

/// Middleware for API routes: reads `Authorization: Bearer <token>` and adds
/// the [`Principal`] to the request for [`Request::principal`]. A missing,
/// malformed, unknown or wrong token is answered `401` as problem JSON with
/// `WWW-Authenticate: Bearer`; the reason is never said.
pub fn bearer(request: &Request, next: Next) -> Response {
    let token = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let refused = || {
        problem(401, "A valid bearer token is required.").with_header("WWW-Authenticate", "Bearer")
    };
    let Some(token) = token else {
        return refused();
    };
    match resolve(request.db(), token) {
        Ok(Some(principal)) => next(&request.clone().with_extension(principal)),
        Ok(None) => refused(),
        Err(problem) => {
            crate::log::Log::error(format_args!("auth: could not read api token: {problem}"));
            crate::web::error(500)
        }
    }
}

/// Guard, after [`bearer`]: `role` or above ([`Auth::principal_at_least`]).
/// Requests without a principal are answered `401`, anyone below `role`
/// `403`, which the router turns into `404` unless it
/// [reveals](crate::web::Router::reveal_forbidden) it.
pub fn token_role(
    role: &'static str,
) -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
    move |request, next| match request.principal() {
        None => problem(401, "A valid bearer token is required.")
            .with_header("WWW-Authenticate", "Bearer"),
        Some(principal) if request.auth().principal_at_least(principal, role) => next(request),
        Some(_) => Response::text(403, ""),
    }
}

impl Request {
    /// The token's principal, when [`bearer`] runs on this route.
    pub fn principal(&self) -> Option<&Principal> {
        self.extension()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::web::Router;

    const ROLES: &[&str] = &["super-admin", "admin", "user", "blocked"];

    fn app() -> (Router, Db, Auth) {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE api_tokens (id INTEGER PRIMARY KEY, public_id TEXT NOT NULL UNIQUE,
                 name TEXT NOT NULL, token_hash TEXT NOT NULL, role TEXT NOT NULL,
                 created_at TEXT DEFAULT CURRENT_TIMESTAMP)",
            )
        })
        .unwrap();
        let routes = Router::new()
            .json_errors()
            .reveal_forbidden()
            .state(db.clone())
            .state(Auth::new(ROLES))
            .group(|api| {
                api.middleware(bearer)
                    .get("/api/me", |request| {
                        let who = request.principal().unwrap();
                        Response::text(200, &format!("{} {}", who.name, who.role))
                    })
                    .group(|admin| {
                        admin
                            .middleware(token_role("admin"))
                            .get("/api/admin", |_| Response::text(200, "admin"))
                    })
            });
        (routes, db, Auth::new(ROLES))
    }

    fn get(routes: &Router, path: &str, token: Option<&str>) -> Response {
        let request = Request::get(path);
        routes.handle(&match token {
            Some(token) => request.with_header("Authorization", &format!("Bearer {token}")),
            None => request,
        })
    }

    #[test]
    fn a_valid_token_reaches_the_handler_with_its_principal() {
        let (routes, db, auth) = app();
        let token = auth.issue_token(&db, "ci", "user").unwrap();
        let response = get(&routes, "/api/me", Some(&token));
        assert_eq!(
            (response.status, response.body_text()),
            (200, "ci user".into())
        );
        // Only a hash is stored.
        let stored: String = db
            .table("api_tokens")
            .first(|row| row.get("token_hash"))
            .unwrap()
            .unwrap();
        assert!(!token.contains(&stored) && !stored.contains('.'));
    }

    #[test]
    fn bad_tokens_are_401_problem_json_without_saying_why() {
        let (routes, db, auth) = app();
        let token = auth.issue_token(&db, "ci", "user").unwrap();
        let (id, secret) = token.split_once('.').unwrap();
        let wrong = format!("{id}.{}", secret.replace(&secret[..1], "z"));
        for attempt in [
            None,
            Some("nodot"),
            Some("unknown.secret"),
            Some(wrong.as_str()),
        ] {
            let response = get(&routes, "/api/me", attempt);
            assert_eq!(response.status, 401, "{attempt:?}");
            assert_eq!(response.content_type, "application/problem+json");
            assert_eq!(response.header("www-authenticate"), Some("Bearer"));
        }
        let basic =
            routes.handle(&Request::get("/api/me").with_header("Authorization", "Basic abc"));
        assert_eq!(basic.status, 401);
    }

    #[test]
    fn roles_gate_routes_and_blocked_or_revoked_tokens_fail() {
        let (routes, db, auth) = app();
        let user = auth.issue_token(&db, "u", "user").unwrap();
        let admin = auth.issue_token(&db, "a", "admin").unwrap();
        let root = auth.issue_token(&db, "r", "super-admin").unwrap();
        let blocked = auth.issue_token(&db, "b", "blocked").unwrap();
        assert_eq!(get(&routes, "/api/admin", Some(&user)).status, 403);
        assert_eq!(get(&routes, "/api/admin", Some(&admin)).status, 200);
        assert_eq!(get(&routes, "/api/admin", Some(&root)).status, 200);
        assert_eq!(get(&routes, "/api/me", Some(&blocked)).status, 401);
        assert!(auth.issue_token(&db, "x", "nope").is_err());
        let id = admin.split_once('.').unwrap().0;
        assert!(auth.revoke_token(&db, id).unwrap());
        assert!(!auth.revoke_token(&db, id).unwrap());
        assert_eq!(get(&routes, "/api/admin", Some(&admin)).status, 401);
    }
}
