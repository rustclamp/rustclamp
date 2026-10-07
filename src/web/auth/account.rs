//! Registration, email verification and password reset (ADR 0016): the
//! flows that mail an account's own address through signed links.

use std::error::Error;

use super::{Auth, FINGERPRINT_KEY, Refused, User, fingerprint, unix_now};
use crate::crypto::{Hash, Key, hmac_sha256, hmac_verify};
use crate::mail::{Message, Recipient};
use crate::web::Request;

type Outcome<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// How long a verification link works.
const VERIFY_FOR: u64 = 24 * 60 * 60;
/// How long a reset link works.
const RESET_FOR: u64 = 60 * 60;
/// The session key holding an opened reset link until the new password.
const RESET_KEY: &str = "_auth_reset";

/// Signs and checks links: `APP_URL` and `APP_KEY`, never the request's host.
pub(super) struct Links {
    url: String,
    key: Key,
    app: String,
}

impl Links {
    pub(super) fn from_config(config: &crate::config::Config) -> Self {
        let url = config
            .get("APP_URL")
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .unwrap_or_else(|| {
                panic!("APP_URL (such as https://example.com) is required for account mail")
            });
        Self {
            url: url.trim_end_matches('/').to_owned(),
            key: Key::from_config(config),
            app: config.get("APP_NAME").unwrap_or("RustClamp").to_owned(),
        }
    }

    fn signature(&self, purpose: &str, user: &User, expires: u64, extra: &str) -> String {
        let data = format!(
            "{purpose}|{}|{}|{expires}|{extra}",
            user.public_id, user.email
        );
        hmac_sha256(self.key.bytes(), data.as_bytes())
    }

    fn link(&self, path: &str, purpose: &str, user: &User, lasts: u64, extra: &str) -> String {
        let expires = unix_now() + lasts;
        let signature = self.signature(purpose, user, expires, extra);
        format!(
            "{}{path}/{}?expires={expires}&signature={signature}",
            self.url, user.public_id
        )
    }

    fn valid(
        &self,
        purpose: &str,
        user: &User,
        expires: &str,
        signature: &str,
        extra: &str,
    ) -> bool {
        let Ok(expires) = expires.parse::<u64>() else {
            return false;
        };
        let data = format!(
            "{purpose}|{}|{}|{expires}|{extra}",
            user.public_id, user.email
        );
        // Checked even when expired, so both failures take the same time.
        let signed = hmac_verify(self.key.bytes(), data.as_bytes(), signature);
        signed && expires > unix_now()
    }
}

impl Auth {
    fn links(&self) -> Outcome<&Links> {
        self.links.as_ref().ok_or_else(|| {
            "account mail needs APP_URL and APP_KEY: build Auth with Auth::from_config".into()
        })
    }

    /// The per-client and per-address limits on auth mail, both counted
    /// before any account is looked up, so they reveal nothing.
    fn mail_allowed(&self, request: &Request, email: &str) -> Result<(), Refused> {
        let client = self.mail_client.client(request);
        self.mail_client.hit(&client).map_err(Refused::Throttled)?;
        self.mail_address.hit(email).map_err(Refused::Throttled)
    }

    fn user_by(
        &self,
        request: &Request,
        column: &str,
        value: &str,
    ) -> Outcome<Option<(User, String)>> {
        Ok(request
            .db()
            .table("users")
            .where_eq(column, &value)
            .first(|row| Ok((User::from_row(row)?, row.get::<String>("password")?)))?)
    }

    fn mail(&self, request: &Request, user: &User, subject: &str, text: String) -> Outcome<()> {
        let to = Recipient::account(&user.email, &user.name)
            .ok_or("the account's address is not valid")?;
        request
            .mailer()
            .send(request.db(), Message::new(to, subject, &text))?;
        Ok(())
    }

    /// Registers `name` with `email` and `password` as a `user`, unverified,
    /// and mails a link to confirm the address. An address that already has
    /// an account gets the same answer and no mail, so registering reveals
    /// nothing. The new user is not logged in: the link confirms the
    /// address, then they log in.
    ///
    /// # Errors
    ///
    /// `Refused::Throttled`, or a database error.
    pub fn register(
        &self,
        request: &Request,
        name: &str,
        email: &str,
        password: &str,
    ) -> Outcome<Result<(), Refused>> {
        let email = email.trim().to_lowercase();
        if let Err(refused) = self.mail_allowed(request, &email) {
            return Ok(Err(refused));
        }
        if self.user_by(request, "email", &email)?.is_some() {
            return Ok(Ok(()));
        }
        let id = self.create_user(request.db(), name, &email, password, "user")?;
        let (user, _) = self
            .user_by(request, "id", &id.to_string())?
            .ok_or("the new user is missing")?;
        self.send_verification_mail(request, &user)?;
        Ok(Ok(()))
    }

    /// Mails the logged-in user a new verification link.
    ///
    /// # Errors
    ///
    /// `Refused::Throttled`, or a database error.
    pub fn resend_verification(&self, request: &Request) -> Outcome<Result<(), Refused>> {
        let Some(user) = request.user().filter(|user| !user.verified).cloned() else {
            return Ok(Ok(()));
        };
        if let Err(refused) = self.mail_allowed(request, &user.email) {
            return Ok(Err(refused));
        }
        self.send_verification_mail(request, &user)?;
        Ok(Ok(()))
    }

    fn send_verification_mail(&self, request: &Request, user: &User) -> Outcome<()> {
        let links = self.links()?;
        let link = links.link("/email/verify", "verify", user, VERIFY_FOR, "");
        let text = format!(
            "Hello {},\n\nConfirm your email address for {} by opening this link within 24 hours:\n\n{link}\n\n\
             If you did not create an account, you can ignore this mail.\n",
            user.name, links.app
        );
        self.mail(request, user, "Confirm your email address", text)
    }

    /// Confirms an address from a verification link
    /// (`/email/verify/{user}?expires=…&signature=…`). `false` for an
    /// unknown, expired or altered link.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn verify(&self, request: &Request) -> Outcome<bool> {
        let links = self.links()?;
        let Some((user, _)) = self.user_by(
            request,
            "public_id",
            request.param("user").unwrap_or_default(),
        )?
        else {
            return Ok(false);
        };
        let (expires, signature) = (
            request.query("expires").unwrap_or_default(),
            request.query("signature").unwrap_or_default(),
        );
        if !links.valid("verify", &user, &expires, &signature, "") {
            return Ok(false);
        }
        self.mark_verified(request.db(), user.id)?;
        Ok(true)
    }

    /// Mails a password reset link to `email` if it belongs to an account
    /// that is not blocked. Every address gets the same answer, so it does
    /// not reveal which have accounts.
    ///
    /// # Errors
    ///
    /// `Refused::Throttled`, or a database error.
    pub fn forgot(&self, request: &Request, email: &str) -> Outcome<Result<(), Refused>> {
        let email = email.trim().to_lowercase();
        if let Err(refused) = self.mail_allowed(request, &email) {
            return Ok(Err(refused));
        }
        let Some((user, hash)) = self.user_by(request, "email", &email)? else {
            return Ok(Ok(()));
        };
        if user.role == "blocked" {
            return Ok(Ok(()));
        }
        let links = self.links()?;
        let link = links.link(
            "/reset-password",
            "reset",
            &user,
            RESET_FOR,
            &fingerprint(&hash),
        );
        let text = format!(
            "Hello {},\n\nSomeone asked to reset your {} password. Choose a new one within 60 minutes:\n\n{link}\n\n\
             If it was not you, ignore this mail: your password stays the same.\n",
            user.name, links.app
        );
        self.mail(request, &user, "Reset your password", text)?;
        Ok(Ok(()))
    }

    /// Opens a reset link (`/reset-password/{user}?expires=…&signature=…`):
    /// when valid, keeps it in the session so the form's URL carries no
    /// token. `false` for an unknown, expired, used or altered link.
    ///
    /// # Errors
    ///
    /// A database error.
    ///
    /// # Panics
    ///
    /// Without [`Sessions::middleware`](crate::web::Sessions::middleware).
    pub fn open_reset(&self, request: &Request) -> Outcome<bool> {
        let public_id = request.param("user").unwrap_or_default().to_owned();
        let (expires, signature) = (
            request.query("expires").unwrap_or_default(),
            request.query("signature").unwrap_or_default(),
        );
        if !self.reset_valid(request, &public_id, &expires, &signature)? {
            return Ok(false);
        }
        let session = request.session().expect("reset needs Sessions::middleware");
        session.put(RESET_KEY, &format!("{public_id}|{expires}|{signature}"));
        Ok(true)
    }

    /// Whether a reset link has been opened in this session and still works.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn reset_pending(&self, request: &Request) -> Outcome<bool> {
        let Some(opened) = request.session().and_then(|session| session.get(RESET_KEY)) else {
            return Ok(false);
        };
        let mut parts = opened.splitn(3, '|');
        let (Some(id), Some(expires), Some(signature)) = (parts.next(), parts.next(), parts.next())
        else {
            return Ok(false);
        };
        self.reset_valid(request, id, expires, signature)
    }

    /// Sets `password` for the account whose reset link was opened in this
    /// session. The link stops working, and every session of that account
    /// ends. `false` when no working link was opened.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn reset(&self, request: &Request, password: &str) -> Outcome<bool> {
        if !self.reset_pending(request)? {
            return Ok(false);
        }
        let session = request
            .session()
            .ok_or("reset needs Sessions::middleware")?;
        let opened = session.take(RESET_KEY).unwrap_or_default();
        let public_id = opened.split('|').next().unwrap_or_default();
        request
            .db()
            .table("users")
            .where_eq("public_id", &public_id)
            .update(&["password"], &[&Hash::make(password)])?;
        // This session was never logged in as them; make sure it is not now.
        session.take(FINGERPRINT_KEY);
        Ok(true)
    }

    fn reset_valid(
        &self,
        request: &Request,
        public_id: &str,
        expires: &str,
        signature: &str,
    ) -> Outcome<bool> {
        let links = self.links()?;
        let Some((user, hash)) = self.user_by(request, "public_id", public_id)? else {
            return Ok(false);
        };
        Ok(user.role != "blocked"
            && links.valid("reset", &user, expires, signature, &fingerprint(&hash)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::config::Config;
    use crate::db::Db;
    use crate::mail::{Mailer, Settings};
    use crate::web::auth::{authenticate, password_confirmed, verified};
    use crate::web::testing::Client;
    use crate::web::{Response, Router, Sessions, csrf, redirect};

    const ROLES: &[&str] = &["super-admin", "admin", "user", "blocked"];
    const ENV: &str = "DB_DATABASE=:memory:\nAPP_URL=https://blog.example\n\
        APP_KEY=base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\nTRUST_PROXY=true\n";

    /// Routes calling each flow, answering with a word the tests read.
    fn app() -> (Router, Db) {
        let config = Config::parse(ENV);
        let db = Db::open(&config);
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, public_id TEXT NOT NULL,
                 name TEXT NOT NULL, email TEXT NOT NULL UNIQUE, password TEXT NOT NULL,
                 role TEXT NOT NULL DEFAULT 'user', email_verified_at TEXT)",
            )
        })
        .unwrap();
        let form = |request: &Request, name: &str| request.form(name).unwrap_or_default();
        let word = |result: Outcome<Result<(), Refused>>| match result.unwrap() {
            Ok(()) => Response::text(200, "sent"),
            Err(refused) => Response::text(429, &format!("{refused:?}")),
        };
        let routes = Router::new()
            .state(db.clone())
            .state(Auth::from_config(ROLES, &config))
            .state(Arc::new(Mailer::new(Settings::from_config(&config))))
            .group(|web| {
                web.middleware(Sessions::new(Duration::from_secs(60)).middleware())
                    .middleware(csrf())
                    .middleware(authenticate)
                    .get("/form", |request| {
                        Response::new(200, "text/html", request.session().unwrap().csrf_field())
                    })
                    .post("/register", move |r| {
                        word(r.auth().register(
                            r,
                            &form(r, "name"),
                            &form(r, "email"),
                            &form(r, "password"),
                        ))
                    })
                    .post("/forgot", move |r| {
                        word(r.auth().forgot(r, &form(r, "email")))
                    })
                    .post("/login", move |r| {
                        match r
                            .auth()
                            .attempt(r, &form(r, "email"), &form(r, "password"))
                            .unwrap()
                        {
                            Ok(_) => Response::text(200, "in"),
                            Err(refused) => Response::text(422, &format!("{refused:?}")),
                        }
                    })
                    .get("/email/verify/{user}", |r| {
                        Response::text(
                            200,
                            if r.auth().verify(r).unwrap() {
                                "verified"
                            } else {
                                "invalid"
                            },
                        )
                    })
                    .get("/reset-password/{user}", |r| {
                        if r.auth().open_reset(r).unwrap() {
                            redirect("/reset-password")
                        } else {
                            Response::text(200, "invalid")
                        }
                    })
                    .get("/reset-password", |r| {
                        let field = r.session().unwrap().csrf_field();
                        let pending = r.auth().reset_pending(r).unwrap();
                        Response::new(
                            200,
                            "text/html",
                            format!("{field}{}", if pending { "form" } else { "invalid" }),
                        )
                    })
                    .post("/reset-password", move |r| {
                        Response::text(
                            200,
                            if r.auth().reset(r, &form(r, "password")).unwrap() {
                                "reset"
                            } else {
                                "invalid"
                            },
                        )
                    })
                    .post("/confirm-password", move |r| {
                        match r.auth().confirm(r, &form(r, "password")).unwrap() {
                            Some(back) => redirect(&back),
                            None => Response::text(422, "wrong"),
                        }
                    })
                    .group(|g| {
                        g.middleware(verified)
                            .get("/home", |_| Response::text(200, "home"))
                    })
                    .group(|g| {
                        g.middleware(password_confirmed)
                            .get("/danger", |_| Response::text(200, "danger"))
                    })
            });
        (routes, db)
    }

    /// The link in the newest mail to `to`, as a path with its query.
    fn link_to(db: &Db, to: &str) -> String {
        let body: String = db
            .with(|sql| {
                sql.query_row(
                    "SELECT body FROM mail_outbox WHERE to_address = ?1 ORDER BY id DESC LIMIT 1",
                    &[&to],
                    |row| row.get(0),
                )
            })
            .unwrap();
        let start = body
            .find("https://blog.example/")
            .expect("a link from APP_URL");
        let url = body[start..].split_whitespace().next().unwrap();
        url.trim_start_matches("https://blog.example").to_owned()
    }

    fn mails(db: &Db) -> i64 {
        db.with(|sql| sql.query_row("SELECT COUNT(*) FROM mail_outbox", &[], |row| row.get(0)))
            .unwrap_or(0)
    }

    fn client<'a>(routes: &'a Router, ip: &str) -> Client<'a> {
        let client = Client::new(routes).with_header("X-Forwarded-For", ip);
        client.get("/form");
        client
    }

    #[test]
    fn registering_mails_a_link_that_verifies_and_reveals_nothing() {
        let (routes, db) = app();
        let visitor = client(&routes, "10.0.0.1");
        let fields = [
            ("name", "Ana"),
            ("email", "Ana@X.si"),
            ("password", "long-enough-pass"),
        ];
        assert_eq!(visitor.post("/register", &fields).body_text(), "sent");
        assert_eq!(mails(&db), 1);
        let link = link_to(&db, "ana@x.si");
        assert!(link.starts_with("/email/verify/"), "{link}");

        // An address that already has an account, not mailed before.
        Auth::new(ROLES)
            .create_user(&db, "Old", "taken@x.si", "long-enough-pass", "user")
            .unwrap();
        let other = client(&routes, "10.0.0.2");
        let taken = [
            ("name", "X"),
            ("email", "taken@x.si"),
            ("password", "long-enough-pass"),
        ];
        assert_eq!(
            other.post("/register", &taken).body_text(),
            "sent",
            "a taken address gets the same answer"
        );
        assert_eq!(mails(&db), 1, "and no mail");

        assert_eq!(
            visitor
                .get(&link.replace("signature=", "signature=0"))
                .body_text(),
            "invalid"
        );
        assert_eq!(visitor.get(&link).body_text(), "verified");
        visitor.get("/form");
        assert_eq!(
            visitor
                .post(
                    "/login",
                    &[("email", "ana@x.si"), ("password", "long-enough-pass")]
                )
                .body_text(),
            "in"
        );
        assert_eq!(
            visitor.get("/home").body_text(),
            "home",
            "verified users pass the guard"
        );
    }

    #[test]
    fn unverified_users_are_sent_to_the_notice() {
        let (routes, _) = app();
        let visitor = client(&routes, "10.0.0.3");
        visitor.post(
            "/register",
            &[
                ("name", "Bo"),
                ("email", "bo@x.si"),
                ("password", "long-enough-pass"),
            ],
        );
        visitor.get("/form");
        visitor.post(
            "/login",
            &[("email", "bo@x.si"), ("password", "long-enough-pass")],
        );
        assert_eq!(visitor.get("/home").location(), Some("/email/verify"));
    }

    #[test]
    fn reset_works_once_and_ends_every_other_session() {
        let (routes, db) = app();
        Auth::new(ROLES)
            .create_user(&db, "Cid", "cid@x.si", "old-password-1", "user")
            .unwrap();
        let elsewhere = client(&routes, "10.0.0.4");
        assert_eq!(
            elsewhere
                .post(
                    "/login",
                    &[("email", "cid@x.si"), ("password", "old-password-1")]
                )
                .body_text(),
            "in"
        );
        assert_eq!(
            elsewhere.get("/home").location(),
            Some("/email/verify"),
            "logged in"
        );

        let visitor = client(&routes, "10.0.0.5");
        assert_eq!(
            visitor
                .post("/forgot", &[("email", "nobody@x.si")])
                .body_text(),
            "sent",
            "unknown: same answer"
        );
        assert_eq!(mails(&db), 0, "and no mail");
        visitor.post("/forgot", &[("email", "cid@x.si")]);
        let link = link_to(&db, "cid@x.si");
        assert_eq!(
            visitor.get(&link).location(),
            Some("/reset-password"),
            "the token leaves the URL"
        );
        assert!(visitor.get("/reset-password").body_text().ends_with("form"));
        assert_eq!(
            visitor
                .post("/reset-password", &[("password", "new-password-2")])
                .body_text(),
            "reset"
        );

        assert_eq!(
            visitor.get(&link).body_text(),
            "invalid",
            "the link is spent once the password changes"
        );
        assert_eq!(
            elsewhere.get("/home").location(),
            Some("/login"),
            "the other session was ended"
        );
        let fresh = client(&routes, "10.0.0.6");
        assert_eq!(
            fresh
                .post(
                    "/login",
                    &[("email", "cid@x.si"), ("password", "new-password-2")]
                )
                .body_text(),
            "in"
        );
    }

    #[test]
    fn auth_mail_is_throttled_per_address_and_per_client() {
        let (routes, db) = app();
        let one = client(&routes, "10.0.0.7");
        assert_eq!(
            one.post("/forgot", &[("email", "a@x.si")]).body_text(),
            "sent"
        );
        assert!(
            one.post("/forgot", &[("email", "a@x.si")])
                .body_text()
                .starts_with("Throttled"),
            "1 per 10 minutes per address"
        );
        let two = client(&routes, "10.0.0.8");
        assert!(
            two.post("/forgot", &[("email", "a@x.si")])
                .body_text()
                .starts_with("Throttled"),
            "whoever asks"
        );
        // That refused attempt counted: two more, then the client is limited.
        for email in ["b@x.si", "c@x.si"] {
            assert_eq!(two.post("/forgot", &[("email", email)]).body_text(), "sent");
        }
        assert!(
            two.post("/forgot", &[("email", "d@x.si")])
                .body_text()
                .starts_with("Throttled"),
            "3 an hour per client"
        );
        assert_eq!(mails(&db), 0, "none of these addresses has an account");
    }

    #[test]
    fn links_use_app_url_whatever_the_host() {
        let (routes, db) = app();
        let visitor = Client::new(&routes)
            .with_header("Host", "evil.example")
            .with_header("X-Forwarded-For", "10.0.0.9");
        visitor.get("/form");
        visitor.post(
            "/register",
            &[
                ("name", "Di"),
                ("email", "di@x.si"),
                ("password", "long-enough-pass"),
            ],
        );
        let body: String = db
            .with(|sql| sql.query_row("SELECT body FROM mail_outbox", &[], |row| row.get(0)))
            .unwrap();
        assert!(
            body.contains("https://blog.example/email/verify/") && !body.contains("evil.example")
        );
    }

    #[test]
    fn confirm_password_returns_to_the_page_that_asked() {
        let (routes, db) = app();
        Auth::new(ROLES)
            .create_user(&db, "Eva", "eva@x.si", "secret-pass-3", "user")
            .unwrap();
        let visitor = client(&routes, "10.0.0.10");
        visitor.post(
            "/login",
            &[("email", "eva@x.si"), ("password", "secret-pass-3")],
        );
        assert_eq!(visitor.get("/danger").location(), Some("/confirm-password"));
        visitor.get("/form");
        assert_eq!(
            visitor
                .post("/confirm-password", &[("password", "wrong")])
                .status,
            422
        );
        assert_eq!(
            visitor
                .post("/confirm-password", &[("password", "secret-pass-3")])
                .location(),
            Some("/danger")
        );
        assert_eq!(visitor.get("/danger").body_text(), "danger");
    }
}
