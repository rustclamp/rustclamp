//! Login, users and roles (ADR 0013), with the `auth` feature.
//!
//! Every user has exactly one role, a slug from the app's ordered list, such
//! as `["super-admin", "admin", "user", "blocked"]`. `super-admin` passes every
//! check, `blocked` fails every check and cannot log in, and anyone else is
//! decided by the policy ([`allows`]).
//!
//! The session holds only the user's id: [`authenticate`] loads the user on
//! every request, so blocking someone or changing their role applies on
//! their next request.
//!
//! The app owns the `users` table: `id`, `public_id`, `name`, `email` (unique),
//! `password`, `role` (NOT NULL, default `user`) and timestamps.

use std::error::Error;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[cfg(feature = "mail")]
mod account;
mod token;

pub use token::{Principal, bearer, token_role};

use super::request::State;
use super::{Next, Request, Response, Throttle, ToValue, Value, error, redirect};
use crate::crypto::Hash;
use crate::db::Db;
use crate::db::{Row, params};

/// The session key holding the logged-in user's id.
const SESSION_KEY: &str = "_user";
/// The session key holding a fingerprint of the password hash at login: when
/// the password changes, every session made with the old one ends.
const FINGERPRINT_KEY: &str = "_auth_fp";
/// The session key holding when the password was last confirmed.
const CONFIRMED_KEY: &str = "_auth_confirmed";
/// The session key holding the page that asked for confirmation.
const INTENDED_KEY: &str = "_auth_intended";
/// How long a confirmed password counts.
const CONFIRM_FOR: u64 = 3 * 60 * 60;
/// Where guests are sent from protected pages.
const LOGIN: &str = "/login";

/// A short fingerprint of a password hash, safe to keep in a session.
fn fingerprint(hash: &str) -> String {
    crate::crypto::sha256(hash.as_bytes())[..32].to_owned()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// A user as the app sees it: never the password hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// The primary key; stays inside the app.
    pub id: i64,
    /// The UUIDv7 used in URLs and payloads.
    pub public_id: String,
    /// The display name.
    pub name: String,
    /// Lowercased.
    pub email: String,
    /// One slug from the app's roles.
    pub role: String,
    /// Whether the email address has been confirmed (`email_verified_at`).
    pub verified: bool,
}

impl User {
    fn from_row(row: &Row) -> crate::db::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            public_id: row.get("public_id")?,
            name: row.get("name")?,
            email: row.get("email")?,
            role: row.get("role")?,
            verified: row.get::<Option<String>>("email_verified_at")?.is_some(),
        })
    }
}

/// `public_id`, `name`, `email` and `role`: no `id`, no password.
impl ToValue for User {
    fn to_value(&self) -> Value {
        Value::map(&[
            ("public_id", &self.public_id),
            ("name", &self.name),
            ("email", &self.email),
            ("role", &self.role),
            ("verified", &self.verified),
        ])
    }
}

/// Why a login was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Unknown email or wrong password; deliberately indistinguishable.
    Invalid,
    /// Too many attempts for this email from this address; retry after this.
    Throttled(Duration),
    /// The password was right, but the account has the `blocked` role.
    Blocked,
}

/// The app's roles and login throttle. Shared with handlers as router state
/// ([`App`](super::App) adds it); read it with [`Request::auth`].
pub struct Auth {
    roles: &'static [&'static str],
    throttle: Throttle,
    /// Signed links for verification and reset mail (ADR 0016).
    #[cfg(feature = "mail")]
    links: Option<account::Links>,
    /// Auth mail per client: 3 an hour.
    #[cfg(feature = "mail")]
    mail_client: Throttle,
    /// Auth mail per address: 1 per 10 minutes.
    #[cfg(feature = "mail")]
    mail_address: Throttle,
}

impl Auth {
    /// Auth over `roles`, highest first.
    ///
    /// # Panics
    ///
    /// When `roles` lacks `super-admin`, `user` or `blocked`: the house rules
    /// depend on them.
    pub fn new(roles: &'static [&'static str]) -> Self {
        for required in ["super-admin", "user", "blocked"] {
            assert!(roles.contains(&required), "roles must include {required}");
        }
        // Made now, so the first unknown-email login is not the slow one.
        dummy_hash();
        Self {
            roles,
            throttle: Throttle::per_minute(5),
            #[cfg(feature = "mail")]
            links: None,
            #[cfg(feature = "mail")]
            mail_client: Throttle::new(3, Duration::from_secs(60 * 60)),
            #[cfg(feature = "mail")]
            mail_address: Throttle::new(1, Duration::from_secs(10 * 60)),
        }
    }

    /// [`new`](Self::new) with the login throttle from config:
    /// `LOGIN_PER_MINUTE` attempts per email and client (default 5), and
    /// `TRUST_PROXY`, so behind nginx each client is its forwarded address,
    /// not the proxy's. [`App`](super::App) builds `Auth` this way.
    ///
    /// With the `mail` feature it also reads `APP_URL` and `APP_KEY` for the
    /// signed links in verification and reset mail.
    ///
    /// # Panics
    ///
    /// With the `mail` feature, when `APP_URL` or `APP_KEY` is missing: links
    /// must never be built from the request's `Host`.
    pub fn from_config(roles: &'static [&'static str], config: &crate::config::Config) -> Self {
        let trust = config.get_or("TRUST_PROXY", false);
        #[cfg(not(feature = "mail"))]
        let _ = trust;
        Self {
            throttle: Throttle::from_config(config, "LOGIN_PER_MINUTE", 5),
            #[cfg(feature = "mail")]
            links: Some(account::Links::from_config(config)),
            #[cfg(feature = "mail")]
            mail_client: Throttle::new(3, Duration::from_secs(60 * 60)).trust_forwarded(trust),
            #[cfg(feature = "mail")]
            mail_address: Throttle::new(1, Duration::from_secs(10 * 60)),
            ..Self::new(roles)
        }
    }

    /// The app's roles, highest first.
    pub fn roles(&self) -> &'static [&'static str] {
        self.roles
    }

    /// Logs in with `email` and `password`: on success the session moves to
    /// a new ID and remembers the user.
    ///
    /// # Errors
    ///
    /// [`Refused`], or a database error.
    ///
    /// # Panics
    ///
    /// Without [`Sessions::middleware`](super::Sessions::middleware) on the route.
    pub fn attempt(
        &self,
        request: &Request,
        email: &str,
        password: &str,
    ) -> Result<std::result::Result<User, Refused>, Box<dyn Error + Send + Sync>> {
        let email = email.trim().to_lowercase();
        let client = self.throttle.client(request);
        if let Err(wait) = self.throttle.hit(&format!("{email}|{client}")) {
            return Ok(Err(Refused::Throttled(wait)));
        }
        let db = request.db();
        let found = db
            .table("users")
            .where_eq("email", &email)
            .first(|row| Ok((User::from_row(row)?, row.get::<String>("password")?)))?;
        // An unknown email costs the same Argon2 check as a wrong password.
        let hash = found
            .as_ref()
            .map_or_else(|| dummy_hash(), |(_, hash)| hash.as_str());
        let verified = Hash::check(password, hash);
        let Some((user, stored)) = found.filter(|_| verified) else {
            return Ok(Err(Refused::Invalid));
        };
        if user.role == "blocked" {
            return Ok(Err(Refused::Blocked));
        }
        let session = request
            .session()
            .expect("login needs Sessions::middleware on this route");
        session.regenerate();
        session.put(SESSION_KEY, &user.id.to_string());
        session.put(FINGERPRINT_KEY, &fingerprint(&stored));
        Ok(Ok(user))
    }

    /// Creates a user with `role`, returning its id. The email is stored
    /// lowercased and the password hashed.
    ///
    /// # Errors
    ///
    /// An unknown role, a taken email, or another database error.
    pub fn create_user(
        &self,
        db: &Db,
        name: &str,
        email: &str,
        password: &str,
        role: &str,
    ) -> Result<i64, Box<dyn Error + Send + Sync>> {
        self.known(role)?;
        Ok(db.table("users").insert(
            &["public_id", "name", "email", "password", "role"],
            params![
                crate::uuid::Uuid::v7(),
                name,
                email.trim().to_lowercase(),
                Hash::make(password),
                role
            ],
        )?)
    }

    /// Replaces the role of user `id`: a user always has exactly one.
    /// Blocking is `sync_role(db, id, "blocked")`, never a delete.
    ///
    /// # Errors
    ///
    /// An unknown role or a database error.
    pub fn sync_role(
        &self,
        db: &Db,
        id: i64,
        role: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.known(role)?;
        db.table("users")
            .where_eq("id", &id)
            .update(&["role"], &[&role])?;
        Ok(())
    }

    /// Marks user `id`'s email as confirmed, as `user:create` does.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn mark_verified(&self, db: &Db, id: i64) -> Result<(), Box<dyn Error + Send + Sync>> {
        db.with(|sql| {
            sql.execute(
                "UPDATE users SET email_verified_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND email_verified_at IS NULL",
                &[&id],
            )
        })?;
        Ok(())
    }

    /// Checks the logged-in user's password for [`password_confirmed`]:
    /// on success, remembers it for 3 hours and returns the page that asked
    /// (or `/`).
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn confirm(
        &self,
        request: &Request,
        password: &str,
    ) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
        let (Some(user), Some(session)) = (request.user(), request.session()) else {
            return Ok(None);
        };
        let hash: Option<String> = request
            .db()
            .table("users")
            .where_eq("id", &user.id)
            .first(|row| row.get("password"))?;
        if !hash.is_some_and(|hash| Hash::check(password, &hash)) {
            return Ok(None);
        }
        session.put(CONFIRMED_KEY, &unix_now().to_string());
        Ok(Some(
            session.take(INTENDED_KEY).unwrap_or_else(|| "/".into()),
        ))
    }

    /// Whether `user` holds `role` or one above it, through [`allows`]:
    /// `super-admin` always passes, `blocked` never does.
    pub fn at_least(&self, user: &User, role: &str) -> bool {
        allows(user, |user| self.ranks_at_least(&user.role, role))
    }

    /// Whether `held` is `needed` or above it in the app's order; a slug
    /// outside the list never is.
    fn ranks_at_least(&self, held: &str, needed: &str) -> bool {
        let rank = |slug: &str| self.roles.iter().position(|known| *known == slug);
        matches!((rank(held), rank(needed)), (Some(held), Some(needed)) if held <= needed)
    }

    fn known(&self, role: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.roles.contains(&role) {
            Ok(())
        } else {
            Err(format!("unknown role {role}; the app's roles are {:?}", self.roles).into())
        }
    }
}

/// The before rule every check goes through: `blocked` is refused,
/// `super-admin` is allowed, anyone else is decided by `policy`.
pub fn allows(user: &User, policy: impl FnOnce(&User) -> bool) -> bool {
    allows_role(&user.role, || policy(user))
}

/// [`allows`] for anything that holds a role, such as a token's [`Principal`].
fn allows_role(role: &str, policy: impl FnOnce() -> bool) -> bool {
    match role {
        "blocked" => false,
        "super-admin" => true,
        _ => policy(),
    }
}

/// Logs out: the session is cleared and moves to a new ID. Call it from a
/// `POST` route, which [`csrf`](super::csrf) protects.
///
/// # Panics
///
/// Without [`Sessions::middleware`](super::Sessions::middleware) on the route.
pub fn logout(request: &Request) {
    request
        .session()
        .expect("logout needs Sessions::middleware on this route")
        .invalidate();
}

/// Middleware loading the logged-in user for [`Request::user`]. Add it after
/// [`Sessions::middleware`](super::Sessions::middleware). A user who has
/// been blocked or deleted since logging in is logged out here.
pub fn authenticate(request: &Request, next: Next) -> Response {
    let Some(session) = request.session() else {
        return next(request);
    };
    let Some(id) = session
        .get(SESSION_KEY)
        .and_then(|id| id.parse::<i64>().ok())
    else {
        return next(request);
    };
    let user = request
        .db()
        .table("users")
        .where_eq("id", &id)
        .first(|row| Ok((User::from_row(row)?, row.get::<String>("password")?)));
    // A changed password (a reset elsewhere) ends this session.
    let current =
        |hash: &str| session.get(FINGERPRINT_KEY).as_deref() == Some(fingerprint(hash).as_str());
    match user {
        Ok(Some((user, hash))) if user.role != "blocked" && current(&hash) => {
            let mut request = request.clone();
            let mut values = (*request.state.0).clone();
            values.push(Arc::new(CurrentUser(user)));
            request.state = State(Arc::new(values));
            next(&request)
        }
        Ok(_) => {
            session.invalidate();
            next(request)
        }
        Err(problem) => {
            crate::log::Log::error(format_args!("auth: could not load user {id}: {problem}"));
            error(500)
        }
    }
}

/// Guard: users whose email is not confirmed yet are sent to
/// `/email/verify`, guests to `/login`.
pub fn verified(request: &Request, next: Next) -> Response {
    match request.user() {
        Some(user) if user.verified => next(request),
        Some(_) => redirect("/email/verify"),
        None => redirect(LOGIN),
    }
}

/// Guard: asks for the password again (at `/confirm-password`) unless it was
/// confirmed in the last 3 hours, then returns to this page.
pub fn password_confirmed(request: &Request, next: Next) -> Response {
    let Some(session) = request.session() else {
        return redirect(LOGIN);
    };
    let fresh = session
        .get(CONFIRMED_KEY)
        .and_then(|at| at.parse::<u64>().ok())
        .is_some_and(|at| unix_now().saturating_sub(at) < CONFIRM_FOR);
    if fresh {
        return next(request);
    }
    if request.method == "GET" {
        session.put(INTENDED_KEY, &request.path);
    }
    redirect("/confirm-password")
}

/// Guard: guests are redirected to `/login`; add it after [`authenticate`].
pub fn required(request: &Request, next: Next) -> Response {
    match request.user() {
        Some(_) => next(request),
        None => redirect(LOGIN),
    }
}

/// Guard: `role` or above ([`Auth::at_least`]). Guests are redirected to
/// `/login`; anyone else is answered `403`, which the router turns into `404`.
pub fn role(role: &'static str) -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
    move |request, next| match request.user() {
        None => redirect(LOGIN),
        Some(user) if request.auth().at_least(user, role) => next(request),
        Some(_) => Response::text(403, ""),
    }
}

/// The user [`authenticate`] loaded, stored per request.
struct CurrentUser(User);

impl Request {
    /// The logged-in user, when [`authenticate`] runs on this route.
    pub fn user(&self) -> Option<&User> {
        self.state::<CurrentUser>().map(|current| &current.0)
    }

    /// The app's [`Auth`].
    ///
    /// # Panics
    ///
    /// When the router has no `Auth` state ([`App`](super::App) adds it).
    pub fn auth(&self) -> &Auth {
        self.state::<Auth>()
            .expect("no Auth: add .state(Auth::new(ROLES)) to the router")
    }
}

/// A hash to check unknown emails against, made once.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| Hash::make("not a real password"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::web::testing::Client;
    use crate::web::{Router, Sessions, csrf};

    const ROLES: &[&str] = &["super-admin", "admin", "user", "blocked"];

    /// A router with users `root` (super-admin), `ada` (admin), `bob` (user)
    /// and `eve` (blocked), each with password `secret-pass`.
    fn app() -> (Router, Db) {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, public_id TEXT NOT NULL,
                 name TEXT NOT NULL, email TEXT NOT NULL UNIQUE, password TEXT NOT NULL,
                 role TEXT NOT NULL DEFAULT 'user', email_verified_at TEXT)",
            )
        })
        .unwrap();
        let auth = Auth::new(ROLES);
        for (name, role) in [
            ("root", "super-admin"),
            ("ada", "admin"),
            ("bob", "user"),
            ("eve", "blocked"),
        ] {
            auth.create_user(&db, name, &format!(" {name}@X.si "), "secret-pass", role)
                .unwrap();
        }
        let routes = Router::new().state(db.clone()).state(auth).group(|web| {
            web.middleware(Sessions::new(Duration::from_secs(60)).middleware())
                .middleware(csrf())
                .middleware(authenticate)
                .get("/login", |request| {
                    Response::new(200, "text/html", request.session().unwrap().csrf_field())
                })
                .post("/login", |request| {
                    let result = request
                        .auth()
                        .attempt(
                            request,
                            &request.form("email").unwrap_or_default(),
                            &request.form("password").unwrap_or_default(),
                        )
                        .unwrap();
                    match result {
                        Ok(user) => Response::text(200, &user.name),
                        Err(refused) => Response::text(422, &format!("{refused:?}")),
                    }
                })
                .post("/logout", |request| {
                    logout(request);
                    redirect("/")
                })
                .group(|admin| {
                    admin.middleware(role("admin")).get("/admin", |request| {
                        Response::text(200, &request.user().unwrap().name)
                    })
                })
                .group(|member| {
                    member
                        .middleware(required)
                        .get("/me", |_| Response::text(200, "me"))
                })
        });
        (routes, db)
    }

    fn login(client: &Client<'_>, email: &str, password: &str) -> Response {
        client.get("/login");
        client.post("/login", &[("email", email), ("password", password)])
    }

    #[test]
    fn guests_are_sent_to_login_and_roles_decide() {
        let (routes, _) = app();
        let guest = Client::new(&routes);
        assert_eq!(guest.get("/admin").location(), Some("/login"));
        assert_eq!(guest.get("/me").location(), Some("/login"));

        for (who, admin) in [("root", 200), ("ada", 200), ("bob", 404)] {
            let client = Client::new(&routes);
            assert_eq!(
                login(&client, &format!("{who}@x.si"), "secret-pass").status,
                200,
                "{who}"
            );
            assert_eq!(
                client.get("/admin").status,
                admin,
                "{who}: a 403 is answered as 404"
            );
            assert_eq!(client.get("/me").status, 200, "{who}");
        }
    }

    #[test]
    fn wrong_password_and_unknown_email_look_the_same() {
        let (routes, _) = app();
        let client = Client::new(&routes);
        let wrong = login(&client, "ada@x.si", "nope").body_text().into_owned();
        let unknown = login(&client, "nobody@x.si", "nope")
            .body_text()
            .into_owned();
        assert_eq!((wrong.as_str(), unknown.as_str()), ("Invalid", "Invalid"));
        assert_eq!(
            login(&client, " ADA@x.si ", "secret-pass").status,
            200,
            "email is case-insensitive"
        );
    }

    #[test]
    fn login_moves_the_session_and_logout_clears_it() {
        let (routes, _) = app();
        let client = Client::new(&routes);
        client.get("/login");
        let before = client.cookie("clamp_session");
        login(&client, "ada@x.si", "secret-pass");
        let after = client.cookie("clamp_session");
        assert_ne!(before, after, "a new session ID on login");
        client.get("/login");
        assert_eq!(client.post("/logout", &[]).location(), Some("/"));
        assert_ne!(
            client.cookie("clamp_session"),
            after,
            "a new session ID on logout"
        );
        assert_eq!(
            client.get("/me").location(),
            Some("/login"),
            "and logged out"
        );
    }

    #[test]
    fn blocked_users_cannot_log_in_and_are_logged_out_mid_session() {
        let (routes, db) = app();
        let client = Client::new(&routes);
        assert_eq!(
            login(&client, "eve@x.si", "secret-pass").body_text(),
            "Blocked"
        );
        assert_eq!(
            login(&client, "eve@x.si", "wrong").body_text(),
            "Invalid",
            "blocked only after the password"
        );

        let bob = Client::new(&routes);
        login(&bob, "bob@x.si", "secret-pass");
        assert_eq!(bob.get("/me").status, 200);
        let id: i64 = db
            .table("users")
            .where_eq("email", &"bob@x.si")
            .first(|row| row.get("id"))
            .unwrap()
            .unwrap();
        routes_auth().sync_role(&db, id, "blocked").unwrap();
        assert_eq!(
            bob.get("/me").location(),
            Some("/login"),
            "blocked on the next request"
        );
    }

    #[test]
    fn login_attempts_are_throttled_per_email_and_address() {
        let (routes, _) = app();
        let client = Client::new(&routes);
        for _ in 0..5 {
            assert_eq!(login(&client, "ada@x.si", "nope").body_text(), "Invalid");
        }
        assert!(
            login(&client, "ada@x.si", "secret-pass")
                .body_text()
                .starts_with("Throttled")
        );
        let elsewhere = Client::new(&routes).from("10.0.0.7");
        assert_eq!(
            login(&elsewhere, "ada@x.si", "secret-pass").status,
            200,
            "other addresses unaffected"
        );
    }

    #[test]
    fn roles_are_one_known_slug_and_the_before_rule_holds() {
        let (_, db) = app();
        let auth = routes_auth();
        assert!(auth.create_user(&db, "x", "x@x.si", "p", "wizard").is_err());
        assert!(auth.sync_role(&db, 1, "wizard").is_err());
        let roles: Vec<String> = db.table("users").get(|row| row.get("role")).unwrap();
        assert!(
            roles.iter().all(|role| ROLES.contains(&role.as_str())),
            "every user has one known role"
        );

        let user = |role: &str| User {
            id: 1,
            public_id: String::new(),
            name: String::new(),
            email: String::new(),
            role: role.into(),
            verified: true,
        };
        assert!(allows(&user("super-admin"), |_| false));
        assert!(!allows(&user("blocked"), |_| true));
        assert!(allows(&user("user"), |_| true) && !allows(&user("user"), |_| false));
        assert!(!auth.at_least(&user("user"), "admin") && auth.at_least(&user("admin"), "user"));
    }

    #[test]
    fn views_never_see_the_password_or_id() {
        let (_, db) = app();
        let user = db
            .table("users")
            .where_eq("email", &"ada@x.si")
            .first(User::from_row)
            .unwrap()
            .unwrap();
        let Value::Map(pairs) = user.to_value() else {
            panic!("a map")
        };
        let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["public_id", "name", "email", "role", "verified"]);
    }

    fn routes_auth() -> Auth {
        Auth::new(ROLES)
    }
}
