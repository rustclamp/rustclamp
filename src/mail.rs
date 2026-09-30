//! Outgoing mail (ADR 0015), with the `mail` feature.
//!
//! Every send goes to the `mail_outbox` table; [`Mailer::start`] delivers it
//! from a background thread, under a daily cap counted per UTC day. Over the
//! cap, mail waits for the next day; a failed delivery is retried with a
//! backoff. Nothing is dropped.
//!
//! A [`Recipient`] comes from the app's config or an authenticated user, never
//! from a string, so a request cannot choose who receives mail:
//!
//! ```compile_fail
//! use rustclamp::mail::Recipient;
//! let to: Recipient = "anyone@example.com".into();
//! ```

mod smtp;

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::db::Db;
use crate::db::sqlite::{OptionalExtension, params};
use crate::log::Log;

/// How the app sends mail: `app/config/mail.rs`, from `MAIL_*` keys.
#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    /// `MAIL_MAILER`: `smtp`, or `log` (writes a line per mail, sends nothing).
    pub mailer: String,
    /// `MAIL_HOST`, such as `smtp-relay.brevo.com`.
    pub host: String,
    /// `MAIL_PORT`, usually 587.
    pub port: u16,
    /// `MAIL_USERNAME`; empty sends no AUTH.
    pub username: String,
    /// `MAIL_PASSWORD`.
    pub password: String,
    /// `MAIL_ENCRYPTION`: `tls` (STARTTLS, required) or `none` (a local
    /// catcher such as Mailpit only).
    pub encryption: String,
    /// `MAIL_FROM_ADDRESS`.
    pub from_address: String,
    /// `MAIL_FROM_NAME`.
    pub from_name: String,
    /// `MAIL_ADMIN_ADDRESS`: the site owner, for [`Mailer::admin`].
    pub admin_address: String,
    /// `MAIL_DAILY_CAP`: most mails sent per UTC day.
    pub daily_cap: u32,
}

impl Settings {
    /// The `MAIL_*` keys with their defaults: the `log` mailer, port 587,
    /// STARTTLS, a cap of 100.
    pub fn from_config(config: &Config) -> Self {
        let text = |key: &str, default: &str| config.get(key).unwrap_or(default).to_owned();
        Self {
            mailer: text("MAIL_MAILER", "log"),
            host: text("MAIL_HOST", "localhost"),
            port: config.get_or("MAIL_PORT", 587),
            username: text("MAIL_USERNAME", ""),
            password: text("MAIL_PASSWORD", ""),
            encryption: text("MAIL_ENCRYPTION", "tls"),
            from_address: text("MAIL_FROM_ADDRESS", ""),
            from_name: text("MAIL_FROM_NAME", ""),
            admin_address: text("MAIL_ADMIN_ADDRESS", ""),
            daily_cap: config.get_or("MAIL_DAILY_CAP", 100),
        }
    }
}

/// Never shows the password.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("mailer", &self.mailer)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("encryption", &self.encryption)
            .field("from_address", &self.from_address)
            .field("daily_cap", &self.daily_cap)
            .finish_non_exhaustive()
    }
}

/// Who receives a mail. Built only from the app's config or an
/// authenticated user (ADR 0015, layer 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    address: String,
    name: String,
}

impl Recipient {
    /// The address in config key `key`, if it is set and looks like one.
    pub fn from_config(config: &Config, key: &str) -> Option<Self> {
        Self::checked(config.get(key)?, "")
    }

    /// A logged-in user.
    #[cfg(feature = "auth")]
    pub fn user(user: &crate::web::auth::User) -> Option<Self> {
        Self::checked(&user.email, &user.name)
    }

    /// The address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// An account's own address, for the framework's verification and reset
    /// mail only (ADR 0016): apps cannot reach it.
    #[cfg(feature = "auth")]
    pub(crate) fn account(address: &str, name: &str) -> Option<Self> {
        Self::checked(address, name)
    }

    fn checked(address: &str, name: &str) -> Option<Self> {
        let address = address.trim();
        let valid = address
            .split_once('@')
            .is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.'))
            && !address
                .contains(|c: char| c.is_whitespace() || c.is_control() || "<>,;\"".contains(c));
        valid.then(|| Self {
            address: address.to_owned(),
            name: one_line(name),
        })
    }
}

/// A plain-text mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    to: Recipient,
    subject: String,
    text: String,
}

impl Message {
    /// A mail to `to`. Line breaks in `subject` become spaces, so it cannot
    /// add headers; `text` is sent as UTF-8.
    pub fn new(to: Recipient, subject: &str, text: &str) -> Self {
        Self {
            to,
            subject: one_line(subject),
            text: text.to_owned(),
        }
    }
}

/// What happened to a mail given to [`Mailer::send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Delivered by the server.
    Sent,
    /// Over today's cap: waits for the next UTC day.
    CapReached,
    /// Refused or unreachable: retried later.
    Failed,
}

/// The app's mailer: shared with handlers as router state
/// ([`App`](crate::web::App) adds it).
#[derive(Debug)]
pub struct Mailer {
    settings: Settings,
    /// The last UTC day the cap was logged as reached, so it is logged once.
    warned: Mutex<String>,
}

impl Mailer {
    /// A mailer with `settings`.
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            warned: Mutex::default(),
        }
    }

    /// The site owner, from `MAIL_ADMIN_ADDRESS`.
    pub fn admin(&self) -> Option<Recipient> {
        Recipient::checked(&self.settings.admin_address, "")
    }

    /// Queues `message` in the outbox and returns its id. Delivery happens
    /// on the mailer's thread, within the daily cap.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn send(&self, db: &Db, message: Message) -> crate::db::sqlite::Result<i64> {
        db.with(create_outbox)?;
        db.with(|sql| {
            sql.execute(
                "INSERT INTO mail_outbox (to_address, to_name, subject, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    message.to.address,
                    message.to.name,
                    message.subject,
                    message.text,
                    now()
                ],
            )?;
            Ok(sql.last_insert_rowid())
        })
    }

    /// Starts the thread that delivers the outbox: at once, then every
    /// minute. Mail left `sending` by a crash goes back to the queue, so it
    /// may, rarely, be sent twice rather than never.
    pub fn start(self: std::sync::Arc<Self>, db: Db) {
        if let Err(problem) = db.with(create_outbox).and_then(|()| {
            db.with(|sql| {
                sql.execute(
                    "UPDATE mail_outbox SET status = 'queued', sent_day = NULL WHERE status = 'sending'",
                    [],
                )
            })
        }) {
            Log::error(format_args!("mail: cannot prepare the outbox: {problem}"));
            return;
        }
        std::thread::spawn(move || {
            loop {
                if let Err(problem) = self.deliver(&db) {
                    Log::error(format_args!("mail: delivery stopped: {problem}"));
                }
                std::thread::sleep(Duration::from_secs(60));
            }
        });
    }

    /// Delivers pending mail now with the configured transport.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn deliver(&self, db: &Db) -> crate::db::sqlite::Result<usize> {
        let settings = &self.settings;
        self.deliver_with(db, &today(), &mut |mail| match settings.mailer.as_str() {
            "smtp" => smtp::send(settings, mail),
            _ => {
                Log::info(format_args!(
                    "mail (log mailer): to {} subject {:?}",
                    mail.to_address, mail.subject
                ));
                Ok(())
            }
        })
    }

    /// Delivers pending mail for UTC day `today` through `transport`, oldest
    /// first, until the queue is empty or today's cap is reached. Returns how
    /// many were sent. Tests pass their own day and transport.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn deliver_with(
        &self,
        db: &Db,
        today: &str,
        transport: &mut dyn FnMut(&Outgoing) -> Result<(), String>,
    ) -> crate::db::sqlite::Result<usize> {
        db.with(create_outbox)?;
        let mut sent = 0;
        loop {
            match claim(db, today, self.settings.daily_cap)? {
                Claim::Empty => return Ok(sent),
                Claim::CapReached(waiting) => {
                    self.warn_cap(today, waiting);
                    return Ok(sent);
                }
                Claim::Mail(mail) => match transport(&mail) {
                    Ok(()) => {
                        db.with(|sql| {
                            sql.execute(
                                "UPDATE mail_outbox SET status = 'sent', sent_at = ?2, last_error = NULL WHERE id = ?1",
                                params![mail.id, now()],
                            )
                        })?;
                        sent += 1;
                    }
                    Err(problem) => {
                        // 1, 2, 4 ... minutes, at most an hour.
                        let backoff = 60 * 2_i64.pow(mail.attempts.min(6));
                        Log::warning(format_args!(
                            "mail {} to {} failed (attempt {}): {problem}",
                            mail.id,
                            mail.to_address,
                            mail.attempts + 1
                        ));
                        db.with(|sql| {
                            sql.execute(
                                "UPDATE mail_outbox SET status = 'queued', sent_day = NULL,
                                 attempts = attempts + 1, next_attempt_at = ?2, last_error = ?3 WHERE id = ?1",
                                params![mail.id, now() + backoff.min(3600), problem],
                            )
                        })?;
                    }
                },
            }
        }
    }

    fn warn_cap(&self, today: &str, waiting: i64) {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        if *warned != today {
            *warned = today.to_owned();
            Log::warning(format_args!(
                "mail: daily cap of {} reached for {today} (UTC); {waiting} waiting for tomorrow",
                self.settings.daily_cap
            ));
        }
    }
}

/// A mail being delivered, as the transport sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// The outbox row.
    pub id: i64,
    /// Recipient address.
    pub to_address: String,
    /// Recipient name, may be empty.
    pub to_name: String,
    /// One line.
    pub subject: String,
    /// Plain text.
    pub body: String,
    /// Failed deliveries so far.
    pub attempts: u32,
}

enum Claim {
    Empty,
    CapReached(i64),
    Mail(Outgoing),
}

/// Takes the oldest due mail, in one transaction, only while today's count
/// (sent or being sent) is under `cap`, so parallel deliverers cannot
/// overshoot.
fn claim(db: &Db, today: &str, cap: u32) -> crate::db::sqlite::Result<Claim> {
    db.transaction(|sql| {
        let used: i64 = sql.query_row(
            "SELECT COUNT(*) FROM mail_outbox WHERE sent_day = ?1 AND status IN ('sent', 'sending')",
            [today],
            |row| row.get(0),
        )?;
        if used >= i64::from(cap) {
            let waiting: i64 =
                sql.query_row("SELECT COUNT(*) FROM mail_outbox WHERE status = 'queued'", [], |row| row.get(0))?;
            return Ok(if waiting > 0 { Claim::CapReached(waiting) } else { Claim::Empty });
        }
        let mail = sql
            .query_row(
                "UPDATE mail_outbox SET status = 'sending', sent_day = ?1
                 WHERE id = (SELECT id FROM mail_outbox WHERE status = 'queued' AND next_attempt_at <= ?2
                             ORDER BY id LIMIT 1)
                 RETURNING id, to_address, to_name, subject, body, attempts",
                params![today, now()],
                |row| {
                    Ok(Outgoing {
                        id: row.get(0)?,
                        to_address: row.get(1)?,
                        to_name: row.get(2)?,
                        subject: row.get(3)?,
                        body: row.get(4)?,
                        attempts: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(mail.map_or(Claim::Empty, Claim::Mail))
    })
}

fn create_outbox(sql: &crate::db::sqlite::Connection) -> crate::db::sqlite::Result<()> {
    sql.execute_batch(
        "CREATE TABLE IF NOT EXISTS mail_outbox (
            id INTEGER PRIMARY KEY,
            to_address TEXT NOT NULL,
            to_name TEXT NOT NULL DEFAULT '',
            subject TEXT NOT NULL,
            body TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued',
            attempts INTEGER NOT NULL DEFAULT 0,
            next_attempt_at INTEGER NOT NULL DEFAULT 0,
            sent_day TEXT,
            sent_at INTEGER,
            last_error TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS mail_outbox_status ON mail_outbox (status, id);
        CREATE INDEX IF NOT EXISTS mail_outbox_sent_day ON mail_outbox (sent_day)",
    )
}

/// `text` with line breaks and other control characters as spaces.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs() as i64)
}

/// Today in UTC, `YYYY-MM-DD`.
fn today() -> String {
    let (year, month, day, ..) = civil(now());
    format!("{year:04}-{month:02}-{day:02}")
}

/// Year, month, day, hour, minute, second and weekday (0 = Sunday) of Unix
/// time `seconds` in UTC (Howard Hinnant's days-from-civil, inverted).
fn civil(seconds: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400) as u32;
    let weekday = (days + 4).rem_euclid(7) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        weekday,
    )
}

#[cfg(feature = "web")]
impl crate::web::Request {
    /// The app's [`Mailer`].
    ///
    /// # Panics
    ///
    /// When the router has no `Mailer` state ([`App`](crate::web::App) adds it).
    pub fn mailer(&self) -> &Mailer {
        self.state::<std::sync::Arc<Mailer>>()
            .expect("no Mailer: add .state(Arc::new(Mailer::new(settings))) to the router")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open(&Config::parse("DB_DATABASE=:memory:"))
    }

    fn mailer(cap: u32) -> Mailer {
        let config = Config::parse(&format!(
            "MAIL_DAILY_CAP={cap}\nMAIL_ADMIN_ADDRESS=owner@site.si\n"
        ));
        Mailer::new(Settings::from_config(&config))
    }

    #[test]
    fn recipients_come_from_config_and_are_checked() {
        let config = Config::parse("OWNER=owner@site.si\nBAD=a@b.si\\r\\nBcc: x@y.si\nNONE=nope\n");
        assert_eq!(
            Recipient::from_config(&config, "OWNER").unwrap().address(),
            "owner@site.si"
        );
        assert!(Recipient::from_config(&config, "NONE").is_none());
        assert!(
            Recipient::checked("a@b.si\r\nBcc: x@y.si", "").is_none(),
            "no header injection"
        );
        assert!(Recipient::from_config(&config, "MISSING").is_none());
        let message = Message::new(mailer(1).admin().unwrap(), "Hi\r\nBcc: x@y.si", "body");
        assert_eq!(
            message.subject, "Hi  Bcc: x@y.si",
            "line breaks become spaces"
        );
    }

    #[test]
    fn the_cap_holds_and_the_rest_waits_for_tomorrow() {
        let (db, mailer) = (db(), mailer(2));
        for n in 0..3 {
            mailer
                .send(
                    &db,
                    Message::new(mailer.admin().unwrap(), &format!("n{n}"), "x"),
                )
                .unwrap();
        }
        let mut delivered = Vec::new();
        let mut transport = |mail: &Outgoing| {
            delivered.push(mail.subject.clone());
            Ok(())
        };
        assert_eq!(
            mailer
                .deliver_with(&db, "2026-09-30", &mut transport)
                .unwrap(),
            2
        );
        assert_eq!(
            mailer
                .deliver_with(&db, "2026-09-30", &mut transport)
                .unwrap(),
            0,
            "cap reached"
        );
        assert_eq!(
            mailer
                .deliver_with(&db, "2026-10-01", &mut transport)
                .unwrap(),
            1,
            "next day"
        );
        assert_eq!(delivered, ["n0", "n1", "n2"], "oldest first, none lost");
    }

    #[test]
    fn failed_mail_stays_queued_with_a_backoff() {
        let (db, mailer) = (db(), mailer(10));
        mailer
            .send(&db, Message::new(mailer.admin().unwrap(), "s", "x"))
            .unwrap();
        let sent = mailer
            .deliver_with(&db, "2026-09-30", &mut |_| Err("421 try later".into()))
            .unwrap();
        assert_eq!(sent, 0);
        let (status, attempts, due): (String, i64, i64) = db
            .with(|sql| {
                sql.query_row(
                    "SELECT status, attempts, next_attempt_at FROM mail_outbox",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .unwrap();
        assert_eq!((status.as_str(), attempts), ("queued", 1));
        assert!(due > now(), "retried later, not in a tight loop");
        assert_eq!(
            mailer
                .deliver_with(&db, "2026-09-30", &mut |_| Ok(()))
                .unwrap(),
            0,
            "not due yet"
        );
    }

    #[test]
    fn parallel_deliverers_never_pass_the_cap() {
        let (db, mailer) = (db(), std::sync::Arc::new(mailer(5)));
        for _ in 0..20 {
            mailer
                .send(&db, Message::new(mailer.admin().unwrap(), "s", "x"))
                .unwrap();
        }
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (db, mailer) = (db.clone(), std::sync::Arc::clone(&mailer));
                std::thread::spawn(move || {
                    mailer.deliver_with(&db, "2026-09-30", &mut |_| {
                        std::thread::sleep(Duration::from_millis(2));
                        Ok(())
                    })
                })
            })
            .collect();
        let total: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0, 4));
        assert_eq!(civil(1_790_726_400), (2026, 9, 30, 0, 0, 0, 3));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0, 2));
    }
}
