use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{Next, Request, Response, error};

/// The session cookie's name.
pub const COOKIE: &str = "clamp_session";
/// The session key holding the CSRF token, and the form field that carries it.
pub const CSRF_FIELD: &str = "_token";
/// Session keys for the one-time flash message, validation errors and old
/// input that [`Request::render`](super::Request::render) shows.
pub(super) const FLASH: &str = "_flash";
pub(super) const ERRORS: &str = "_errors";
pub(super) const OLD: &str = "_old";
/// Most sessions kept at once. At the cap, expired sessions are pruned, then
/// the least recently used one is dropped, so a flood of cookieless requests
/// cannot exhaust memory. The cost of a flood is logging out idle visitors.
const CAPACITY: usize = 10_000;

type Data = Arc<Mutex<HashMap<String, String>>>;

/// One visitor's session: string values shared by every request carrying its cookie.
#[derive(Clone)]
pub struct Session {
    data: Data,
    /// Set by [`Session::regenerate`]; the middleware issues a new ID.
    renew: Arc<AtomicBool>,
}

impl Session {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The value stored under `key`.
    pub fn get(&self, key: &str) -> Option<String> {
        self.lock().get(key).cloned()
    }

    /// Stores `value` under `key`.
    pub fn put(&self, key: &str, value: &str) {
        self.lock().insert(key.into(), value.into());
    }

    /// Removes and returns the value under `key`, such as a one-time flash message.
    pub fn take(&self, key: &str) -> Option<String> {
        self.lock().remove(key)
    }

    /// A one-time message for the next page, such as "Thanks, your message
    /// was received.": [`Request::render`](super::Request::render) passes it
    /// to the view as `flash`, once.
    pub fn flash(&self, message: &str) {
        self.put(FLASH, message);
    }

    /// Moves the session to a new random ID and a new CSRF token, keeping its
    /// values; the old ID stops working. Call it on login, logout and any
    /// change of privilege, so an ID planted or seen before cannot ride along
    /// (session fixation).
    pub fn regenerate(&self) {
        self.lock().remove(CSRF_FIELD);
        self.renew.store(true, Ordering::Relaxed);
    }

    /// This session's CSRF token, created on first use.
    pub fn csrf_token(&self) -> String {
        self.lock()
            .entry(CSRF_FIELD.into())
            .or_insert_with(random_token)
            .clone()
    }

    /// A hidden form field carrying the CSRF token, for `<form method="post">`.
    pub fn csrf_field(&self) -> String {
        format!(
            r#"<input type="hidden" name="{CSRF_FIELD}" value="{}">"#,
            self.csrf_token()
        )
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
}

impl Eq for Session {}

/// Server-side sessions: the cookie holds only a random ID, the values stay
/// on the server, in memory or, with [`database`](Self::database), in
/// SQLite so they survive restarts. Add [`Sessions::middleware`] to the routes
/// that need [`Request::session`], usually a `web` group.
#[derive(Debug)]
pub struct Sessions {
    lifetime: Duration,
    secure: bool,
    capacity: usize,
    store: Mutex<HashMap<String, (Data, Instant)>>,
    #[cfg(feature = "db")]
    database: Option<crate::db::Db>,
}

impl Sessions {
    /// Sessions that expire after `lifetime` without a request.
    pub fn new(lifetime: Duration) -> Self {
        Self {
            lifetime,
            secure: false,
            capacity: CAPACITY,
            store: Mutex::default(),
            #[cfg(feature = "db")]
            database: None,
        }
    }

    /// Keeps sessions in `db`'s `sessions` table, created when missing, so
    /// visitors stay logged in across restarts and processes. Expired rows
    /// are deleted as new sessions start. Two requests changing the same
    /// session at once: the last one to finish wins.
    ///
    /// # Panics
    ///
    /// When the table cannot be created: the app should stop at startup.
    #[cfg(feature = "db")]
    #[must_use]
    pub fn database(mut self, db: crate::db::Db) -> Self {
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    last_activity INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS sessions_last_activity ON sessions (last_activity)",
            )
        })
        .unwrap_or_else(|error| panic!("cannot create the sessions table: {error}"));
        self.database = Some(db);
        self
    }

    /// Marks the cookie `Secure`, so browsers send it over HTTPS only. Enable
    /// it in production.
    #[must_use]
    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    /// The session for cookie value `id`, or a new one with a fresh ID.
    fn open(&self, id: Option<&str>) -> (String, Data) {
        #[cfg(feature = "db")]
        if let Some(db) = &self.database {
            return self.open_stored(db, id);
        }
        let now = Instant::now();
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // ponytail: in-memory store, lost on restart and per process; a file or Redis store when an app needs one
        if let Some((data, touched)) = id.and_then(|id| store.get_mut(id))
            && now.duration_since(*touched) < self.lifetime
        {
            *touched = now;
            return (id.unwrap_or_default().to_owned(), Arc::clone(data));
        }
        // ponytail: O(n) scans, only at capacity
        if store.len() >= self.capacity {
            store.retain(|_, (_, touched)| now.duration_since(*touched) < self.lifetime);
            if store.len() >= self.capacity
                && let Some(oldest) = store
                    .iter()
                    .min_by_key(|(_, (_, touched))| *touched)
                    .map(|(id, _)| id.clone())
            {
                store.remove(&oldest);
            }
        }
        // An unknown or expired ID is never reused, so a visitor cannot pick their own.
        let id = random_token();
        let data = Data::default();
        store.insert(id.clone(), (Arc::clone(&data), now));
        (id, data)
    }

    #[cfg(feature = "db")]
    fn open_stored(&self, db: &crate::db::Db, id: Option<&str>) -> (String, Data) {
        let now = unix_seconds();
        let fresh = now - self.lifetime.as_secs() as i64;
        let stored: Option<String> = id.and_then(|id| {
            db.with(|sql| {
                sql.query_row(
                    "SELECT payload FROM sessions WHERE id = ?1 AND last_activity > ?2",
                    crate::db::sqlite::params![id, fresh],
                    |row| row.get(0),
                )
            })
            .ok()
        });
        if let (Some(id), Some(payload)) = (id, stored) {
            return (id.to_owned(), Arc::new(Mutex::new(unpack(&payload))));
        }
        // A new session is a good moment to forget expired ones.
        let _ =
            db.with(|sql| sql.execute("DELETE FROM sessions WHERE last_activity <= ?1", [fresh]));
        (random_token(), Data::default())
    }

    /// Moves the session stored under `old` to a new ID and returns it.
    fn rename(&self, old: &str, data: &Data) -> String {
        let id = random_token();
        #[cfg(feature = "db")]
        if let Some(db) = &self.database {
            let _ = db.with(|sql| sql.execute("DELETE FROM sessions WHERE id = ?1", [old]));
            return id;
        }
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store.remove(old);
        store.insert(id.clone(), (Arc::clone(data), Instant::now()));
        id
    }

    /// Writes the session back to the database store, if there is one.
    fn save(&self, id: &str, data: &Data) {
        #[cfg(feature = "db")]
        if let Some(db) = &self.database {
            let payload = pack(&data.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
            let saved = db.with(|sql| {
                sql.execute(
                    "INSERT INTO sessions (id, payload, last_activity) VALUES (?1, ?2, ?3)
                     ON CONFLICT (id) DO UPDATE SET payload = ?2, last_activity = ?3",
                    crate::db::sqlite::params![id, payload, unix_seconds()],
                )
            });
            if let Err(error) = saved {
                crate::log::Log::error(format_args!("session: could not save: {error}"));
            }
        }
        #[cfg(not(feature = "db"))]
        let _ = (id, data);
    }

    /// Middleware attaching the visitor's [`Session`] to the request and
    /// refreshing its cookie on the response.
    pub fn middleware(self) -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
        move |request, next| {
            let (mut id, data) = self.open(request.cookie(COOKIE));
            let renew = Arc::new(AtomicBool::new(false));
            let mut request = request.clone();
            request.session = Some(Session {
                data: Arc::clone(&data),
                renew: Arc::clone(&renew),
            });
            let response = next(&request);
            if renew.load(Ordering::Relaxed) {
                id = self.rename(&id, &data);
            }
            self.save(&id, &data);
            let secure = if self.secure { "; Secure" } else { "" };
            response.with_header(
                "Set-Cookie",
                &format!(
                    "{COOKIE}={id}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
                    self.lifetime.as_secs()
                ),
            )
        }
    }
}

/// Middleware refusing state-changing requests (`POST`, `PUT`, `PATCH`,
/// `DELETE`) whose `_token` form field or `X-CSRF-Token` header does not match
/// the session's token. Refusals are `419`. Add it after [`Sessions::middleware`];
/// without a session every such request is refused.
pub fn csrf() -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
    |request, next| {
        if matches!(request.method.as_str(), "GET" | "HEAD" | "OPTIONS") {
            return next(request);
        }
        let sent = request
            .header("x-csrf-token")
            .map(str::to_owned)
            .or_else(|| request.form(CSRF_FIELD));
        let expected = request
            .session()
            .and_then(|session| session.get(CSRF_FIELD));
        match (sent, expected) {
            (Some(sent), Some(expected)) if same(sent.as_bytes(), expected.as_bytes()) => {
                next(request)
            }
            _ => error(419),
        }
    }
}

/// Seconds since the Unix epoch.
#[cfg(feature = "db")]
fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs() as i64)
}

/// Session values as a form-encoded string, for the database store.
#[cfg(feature = "db")]
fn pack(values: &HashMap<String, String>) -> String {
    use super::request::encode;
    values
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The values [`pack`] wrote.
#[cfg(feature = "db")]
fn unpack(payload: &str) -> HashMap<String, String> {
    use super::request::decode;
    payload
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (decode(key), decode(value)))
        .collect()
}

/// Compares without stopping at the first difference, so timing does not
/// reveal how much of a guessed token was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// 32 random bytes from the operating system, hex-encoded.
///
/// # Panics
///
/// Where `/dev/urandom` is missing (Windows), so the request fails with `500`
/// instead of issuing guessable tokens.
fn random_token() -> String {
    // ponytail: std-only keeps the facade dependency-free (ADR 0003); Unix only.
    // Windows support needs `getrandom` and a boundary exception.
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes))
        .expect("sessions need the operating system random source /dev/urandom");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{Router, json};

    fn app() -> Router {
        Router::new().group(|web| {
            web.middleware(Sessions::new(Duration::from_secs(60)).middleware())
                .middleware(csrf())
                .get("/form", |request| {
                    let session = request.session().unwrap();
                    Response::text(200, &session.csrf_token())
                })
                .post("/form", |request| {
                    let session = request.session().unwrap();
                    session.put("saved", "yes");
                    json("{}")
                })
                .get("/saved", |request| {
                    let saved = request.session().unwrap().get("saved").unwrap_or_default();
                    Response::text(200, &saved)
                })
        })
    }

    fn cookie_of(response: &Response) -> String {
        let header = response.header("set-cookie").unwrap();
        header.split(';').next().unwrap().to_owned()
    }

    #[test]
    fn session_survives_between_requests_and_csrf_guards_posts() {
        let app = app();
        let first = app.handle(&Request::get("/form"));
        let cookie = cookie_of(&first);
        let token = String::from_utf8(first.body).unwrap();
        assert_eq!(token.len(), 64);

        let post = |body: String, cookie: &str| {
            app.handle(
                &Request::post("/form")
                    .with_header("Cookie", cookie)
                    .with_body(body),
            )
        };
        assert_eq!(post(String::new(), &cookie).status, 419);
        assert_eq!(post(format!("{CSRF_FIELD}=wrong"), &cookie).status, 419);
        assert_eq!(
            post(format!("{CSRF_FIELD}={token}"), "clamp_session=other").status,
            419
        );
        assert_eq!(post(format!("{CSRF_FIELD}={token}"), &cookie).status, 200);

        let header = Request::post("/form")
            .with_header("Cookie", &cookie)
            .with_header("X-CSRF-Token", &token);
        assert_eq!(app.handle(&header).status, 200);

        let saved = app.handle(&Request::get("/saved").with_header("Cookie", &cookie));
        assert_eq!(saved.body, b"yes");
        assert_eq!(
            cookie_of(&saved),
            cookie,
            "an existing session keeps its ID"
        );
    }

    /// Logs in on `sessions`: the old ID and token die, the values move over.
    fn regenerate_moves_the_session(sessions: Sessions) {
        let app = Router::new().group(|web| {
            web.middleware(sessions.middleware())
                .get("/token", |request| {
                    let session = request.session().unwrap();
                    session.put("cart", "3");
                    Response::text(200, &session.csrf_token())
                })
                .get("/login", |request| {
                    request.session().unwrap().regenerate();
                    Response::text(200, "")
                })
                .get("/state", |request| {
                    let session = request.session().unwrap();
                    let cart = session.get("cart").unwrap_or_default();
                    Response::text(200, &format!("{cart} {}", session.csrf_token()))
                })
        });
        let first = app.handle(&Request::get("/token"));
        let (old, token) = (cookie_of(&first), String::from_utf8(first.body).unwrap());
        let login = app.handle(&Request::get("/login").with_header("Cookie", &old));
        let new = cookie_of(&login);
        assert_ne!(new, old, "a new ID after login");
        let state = |cookie: &str| {
            String::from_utf8(
                app.handle(&Request::get("/state").with_header("Cookie", cookie))
                    .body,
            )
            .unwrap()
        };
        let moved = state(&new);
        assert!(
            moved.starts_with("3 "),
            "values move to the new ID: {moved}"
        );
        assert!(!moved.ends_with(&token), "the CSRF token is replaced");
        assert!(
            !state(&old).starts_with("3"),
            "the old ID no longer opens the session"
        );
    }

    #[test]
    fn regenerate_moves_a_memory_session() {
        regenerate_moves_the_session(Sessions::new(Duration::from_secs(60)));
    }

    #[cfg(feature = "db")]
    #[test]
    fn regenerate_moves_a_database_session() {
        let db = crate::db::Db::open(&crate::config::Config::parse("DB_DATABASE=:memory:"));
        regenerate_moves_the_session(Sessions::new(Duration::from_secs(60)).database(db));
    }

    #[cfg(feature = "db")]
    #[test]
    fn database_sessions_survive_a_restart_and_expire() {
        use crate::config::Config;
        use crate::db::Db;

        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let app = |lifetime| {
            Router::new().group(|web| {
                web.middleware(Sessions::new(lifetime).database(db.clone()).middleware())
                    .get("/put", |request| {
                        request.session().unwrap().put("name", "Neo & co=1");
                        Response::text(200, "")
                    })
                    .get("/get", |request| {
                        Response::text(
                            200,
                            &request.session().unwrap().get("name").unwrap_or_default(),
                        )
                    })
            })
        };
        let first = app(Duration::from_secs(60));
        let cookie = cookie_of(&first.handle(&Request::get("/put")));
        // A new router is a restarted app: only the database remembers.
        let restarted = app(Duration::from_secs(60));
        let got = restarted.handle(&Request::get("/get").with_header("Cookie", &cookie));
        assert_eq!(got.body, b"Neo & co=1");
        assert_eq!(cookie_of(&got), cookie);

        let expired = app(Duration::ZERO);
        let got = expired.handle(&Request::get("/get").with_header("Cookie", &cookie));
        assert_eq!(got.body, b"");
        assert_ne!(cookie_of(&got), cookie);
        let rows: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM sessions", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(
            rows, 1,
            "the expired session was deleted, the new one saved"
        );
    }

    #[test]
    fn unknown_ids_get_a_fresh_session() {
        let app = app();
        let response =
            app.handle(&Request::get("/saved").with_header("Cookie", "clamp_session=chosen"));
        assert_ne!(cookie_of(&response), "clamp_session=chosen");
        let header = response.header("set-cookie").unwrap();
        assert!(header.contains("HttpOnly") && header.contains("SameSite=Lax"));
        assert!(!header.contains("Secure"));
    }

    #[test]
    fn expired_sessions_are_replaced() {
        let sessions = Sessions::new(Duration::from_millis(50));
        let (id, _) = sessions.open(None);
        std::thread::sleep(Duration::from_millis(60));
        assert_ne!(sessions.open(Some(&id)).0, id);
    }

    #[test]
    fn memory_stays_bounded() {
        let mut sessions = Sessions::new(Duration::from_secs(60));
        sessions.capacity = 3;
        let (kept, _) = sessions.open(None);
        let (dropped, _) = sessions.open(None);
        sessions.open(None);
        std::thread::sleep(Duration::from_millis(2));
        sessions.open(Some(&kept)); // recently used
        sessions.open(None);
        let ids = |sessions: &Sessions| {
            sessions
                .store
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert!(
            !ids(&sessions).contains(&dropped),
            "least recently used goes first"
        );
        assert!(ids(&sessions).contains(&kept));
        for _ in 0..10 {
            sessions.open(None);
        }
        assert_eq!(ids(&sessions).len(), 3);
    }

    #[test]
    fn same_compares_whole_values() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }
}
