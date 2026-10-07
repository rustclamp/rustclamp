//! A small synchronous Redis client (ADR 0027), with the `redis` feature.
//!
//! Std only: RESP2 over one `TcpStream`, shared by every clone and thread
//! behind a lock, like [`Db`](crate::db::Db)'s writer. It connects on first
//! use, and after a network error it connects again on the next call. The
//! command that failed is not retried, because Redis may already have run it.
//!
//! ```no_run
//! use rustclamp::config::Config;
//! use rustclamp::redis::Redis;
//! use std::time::Duration;
//!
//! let redis = Redis::open(&Config::parse("REDIS_URL=redis://127.0.0.1:6379/0")).unwrap();
//! redis.set("greeting", "hello", Some(Duration::from_secs(60))).unwrap();
//! assert_eq!(redis.get("greeting").unwrap().as_deref(), Some("hello"));
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::config::Config;

/// How long a connect, read or write may take before the call fails.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A Redis connection. Clones share the same connection.
#[derive(Clone)]
pub struct Redis {
    settings: Arc<Settings>,
    // ponytail: one connection behind a lock serializes commands across
    // threads; a pool when that shows up in measurements, as with Db (#39).
    connection: Arc<Mutex<Connection>>,
}

#[derive(Default)]
struct Connection {
    stream: Option<BufReader<TcpStream>>,
    /// After a failed connect, calls fail at once until then, so a Redis that
    /// is down costs each caller nothing rather than a connect timeout.
    down_until: Option<Instant>,
}

/// How long calls fail fast after a failed connect.
const BACKOFF: Duration = Duration::from_secs(1);

impl std::fmt::Debug for Redis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redis")
            .field("address", &self.settings.address)
            .field("database", &self.settings.database)
            .finish_non_exhaustive()
    }
}

/// Where Redis is, parsed from a `redis://[[user]:password@]host[:port][/db]` URL.
#[derive(Clone, PartialEq, Eq)]
struct Settings {
    address: String,
    username: Option<String>,
    password: Option<String>,
    database: u32,
}

/// A reply from Redis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A missing key (`$-1` or `*-1`).
    Nil,
    /// `+OK` and other status replies.
    Status(String),
    /// An integer reply.
    Int(i64),
    /// A bulk string; not necessarily UTF-8.
    Bytes(Vec<u8>),
    /// An array reply.
    Array(Vec<Value>),
}

/// Why a Redis call failed.
#[derive(Debug)]
pub enum Error {
    /// `REDIS_URL` could not be read.
    Url(String),
    /// The network failed; the next call connects again.
    Io(std::io::Error),
    /// Redis answered with an error, such as `WRONGTYPE ...`.
    Server(String),
    /// Redis answered with something this client did not expect.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(why) => write!(f, "invalid REDIS_URL: {why}"),
            Self::Io(error) => write!(f, "redis: {error}"),
            Self::Server(message) => write!(f, "redis: {message}"),
            Self::Protocol(why) => write!(f, "redis protocol: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl Redis {
    /// The Redis that `REDIS_URL` names (default `redis://127.0.0.1:6379`).
    /// Nothing connects until the first command.
    ///
    /// # Errors
    ///
    /// [`Error::Url`] when `REDIS_URL` is not a `redis://` URL.
    pub fn open(config: &Config) -> Result<Self, Error> {
        Self::connect(config.get("REDIS_URL").unwrap_or("redis://127.0.0.1:6379"))
    }

    /// The Redis at `url`, `redis://[[user]:password@]host[:port][/db]`.
    /// Nothing connects until the first command.
    ///
    /// # Errors
    ///
    /// [`Error::Url`] when `url` cannot be read. `rediss://` (TLS) is not
    /// supported: reach a remote Redis over a private network or a tunnel.
    pub fn connect(url: &str) -> Result<Self, Error> {
        Ok(Self {
            settings: Arc::new(parse_url(url)?),
            connection: Arc::default(),
        })
    }

    /// Runs one command, such as `["HSET", "user:1", "name", "Ada"]`, and
    /// returns its reply. An error reply is [`Error::Server`].
    ///
    /// # Errors
    ///
    /// As [`Error`] describes.
    pub fn command<A: AsRef<[u8]>>(&self, args: &[A]) -> Result<Value, Error> {
        let mut slot = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.stream.is_none() {
            if slot.down_until.is_some_and(|until| Instant::now() < until) {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "redis was unreachable a moment ago",
                )));
            }
            match self.handshake() {
                Ok(stream) => slot.stream = Some(stream),
                Err(error) => {
                    slot.down_until = Some(Instant::now() + BACKOFF);
                    return Err(error);
                }
            }
        }
        let connection = slot.stream.as_mut().expect("connected above");
        let reply = send(connection, args).and_then(|()| read(connection));
        // Drop the connection after any failure but an error reply: a broken
        // or out-of-step stream would answer the next command with this one's
        // reply.
        if matches!(reply, Err(Error::Io(_) | Error::Protocol(_))) {
            slot.stream = None;
        }
        reply
    }

    /// `GET key` as text, or `None` when the key is missing.
    ///
    /// # Errors
    ///
    /// As [`Redis::command`], and [`Error::Protocol`] when the value is not
    /// UTF-8.
    pub fn get(&self, key: &str) -> Result<Option<String>, Error> {
        match self.command(&["GET", key])? {
            Value::Nil => Ok(None),
            Value::Bytes(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| Error::Protocol(format!("{key} is not UTF-8"))),
            other => Err(unexpected(&other)),
        }
    }

    /// `SET key value`, expiring after `ttl` when given.
    ///
    /// # Errors
    ///
    /// As [`Redis::command`].
    pub fn set(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<(), Error> {
        let reply = match ttl {
            Some(ttl) => self.command(&["SET", key, value, "PX", &millis(ttl)])?,
            None => self.command(&["SET", key, value])?,
        };
        match reply {
            Value::Status(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// `DEL key`; `true` when the key existed.
    ///
    /// # Errors
    ///
    /// As [`Redis::command`].
    pub fn del(&self, key: &str) -> Result<bool, Error> {
        match self.command(&["DEL", key])? {
            Value::Int(removed) => Ok(removed > 0),
            other => Err(unexpected(&other)),
        }
    }

    /// Counts one hit on `key` in a fixed window: the count so far and the
    /// time left in the window, which starts at the key's first hit. Atomic
    /// across every app instance sharing this Redis, so it backs a shared rate
    /// limit ([`Throttle::shared`](crate::web::Throttle::shared)).
    ///
    /// # Errors
    ///
    /// As [`Redis::command`].
    pub fn hit(&self, key: &str, window: Duration) -> Result<(i64, Duration), Error> {
        // One script, so a key can never be left counting without an expiry.
        const HIT: &str = "local n = redis.call('INCR', KEYS[1]) \
            if n == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end \
            return {n, redis.call('PTTL', KEYS[1])}";
        match self.command(&["EVAL", HIT, "1", key, &millis(window)])? {
            Value::Array(reply) => match reply[..] {
                [Value::Int(count), Value::Int(left)] => {
                    Ok((count, Duration::from_millis(left.max(0).unsigned_abs())))
                }
                _ => Err(unexpected(&Value::Array(reply))),
            },
            other => Err(unexpected(&other)),
        }
    }

    fn handshake(&self) -> Result<BufReader<TcpStream>, Error> {
        let settings = &self.settings;
        let stream = connect_stream(&settings.address)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream.set_nodelay(true)?;
        let mut connection = BufReader::new(stream);
        if let Some(password) = &settings.password {
            match &settings.username {
                Some(user) => expect_ok(&mut connection, &["AUTH", user, password])?,
                None => expect_ok(&mut connection, &["AUTH", password])?,
            }
        }
        if settings.database != 0 {
            expect_ok(&mut connection, &["SELECT", &settings.database.to_string()])?;
        }
        Ok(connection)
    }
}

fn connect_stream(address: &str) -> Result<TcpStream, Error> {
    use std::net::ToSocketAddrs;
    let mut last = None;
    for socket in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&socket, TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(last
        .map(Error::Io)
        .unwrap_or_else(|| Error::Url(format!("{address} resolves to no address"))))
}

fn expect_ok(connection: &mut BufReader<TcpStream>, args: &[&str]) -> Result<(), Error> {
    send(connection, args)?;
    match read(connection)? {
        Value::Status(_) => Ok(()),
        other => Err(unexpected(&other)),
    }
}

fn millis(duration: Duration) -> String {
    duration.as_millis().max(1).to_string()
}

fn unexpected(value: &Value) -> Error {
    Error::Protocol(format!("unexpected reply {value:?}"))
}

fn parse_url(url: &str) -> Result<Settings, Error> {
    let rest = url.strip_prefix("redis://").ok_or_else(|| {
        Error::Url(if url.starts_with("rediss://") {
            "rediss:// (TLS) is not supported".into()
        } else {
            "expected redis://".into()
        })
    })?;
    let (credentials, rest) = match rest.rsplit_once('@') {
        Some((credentials, rest)) => (Some(credentials), rest),
        None => (None, rest),
    };
    let (host, database) = match rest.split_once('/') {
        None => (rest, 0),
        Some((host, "")) => (host, 0),
        Some((host, database)) => (
            host,
            database
                .parse()
                .map_err(|_| Error::Url(format!("database {database:?} is not a number")))?,
        ),
    };
    if host.is_empty() {
        return Err(Error::Url("no host".into()));
    }
    let address = if host
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        host.to_owned()
    } else {
        format!("{host}:6379")
    };
    let (username, password) = match credentials.map(|c| c.split_once(':')) {
        None => (None, None),
        Some(None) => {
            return Err(Error::Url(
                "credentials need user:password or :password".into(),
            ));
        }
        Some(Some((user, password))) => (
            (!user.is_empty()).then(|| decode(user)).transpose()?,
            Some(decode(password)?),
        ),
    };
    Ok(Settings {
        address,
        username,
        password,
        database,
    })
}

/// Percent-decodes a URL's user or password, as providers hand them out.
fn decode(text: &str) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = tail
                .get(..2)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| Error::Url("bad %-escape in credentials".into()))?;
            bytes.push(hex);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).map_err(|_| Error::Url("credentials are not UTF-8".into()))
}

fn send<A: AsRef<[u8]>>(connection: &mut BufReader<TcpStream>, args: &[A]) -> Result<(), Error> {
    let mut frame = format!("*{}\r\n", args.len()).into_bytes();
    for arg in args {
        let arg = arg.as_ref();
        frame.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
        frame.extend_from_slice(arg);
        frame.extend_from_slice(b"\r\n");
    }
    connection.get_mut().write_all(&frame)?;
    Ok(())
}

/// Most bytes a bulk reply may announce: Redis' own limit (`proto-max-bulk-len`).
const MAX_BULK: usize = 512 * 1024 * 1024;

fn read(connection: &mut impl BufRead) -> Result<Value, Error> {
    let mut line = Vec::new();
    connection.read_until(b'\n', &mut line)?;
    let line = line
        .strip_suffix(b"\r\n")
        .ok_or_else(|| Error::Protocol("connection closed mid-reply".into()))?;
    let (&kind, body) = line
        .split_first()
        .ok_or_else(|| Error::Protocol("empty line".into()))?;
    let text = std::str::from_utf8(body).map_err(|_| Error::Protocol("non-UTF-8 header".into()))?;
    let number = || {
        text.parse::<i64>()
            .map_err(|_| Error::Protocol(format!("bad length {text:?}")))
    };
    match kind {
        b'+' => Ok(Value::Status(text.to_owned())),
        b'-' => Err(Error::Server(text.to_owned())),
        b':' => Ok(Value::Int(number()?)),
        b'$' => match usize::try_from(number()?) {
            Err(_) => Ok(Value::Nil),
            Ok(length) if length > MAX_BULK => {
                Err(Error::Protocol(format!("bulk of {length} bytes")))
            }
            Ok(length) => {
                let mut bytes = vec![0; length + 2];
                connection.read_exact(&mut bytes)?;
                if !bytes.ends_with(b"\r\n") {
                    return Err(Error::Protocol("bulk without CRLF".into()));
                }
                bytes.truncate(length);
                Ok(Value::Bytes(bytes))
            }
        },
        b'*' => match usize::try_from(number()?) {
            Err(_) => Ok(Value::Nil),
            Ok(count) => (0..count)
                .map(|_| read(connection))
                .collect::<Result<_, _>>()
                .map(Value::Array),
        },
        other => Err(Error::Protocol(format!(
            "unknown reply type {:?}",
            other as char
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A fake Redis on a free port that answers each command in `script` with
    /// its reply, in order, and records what it received.
    fn fake(script: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut seen = Vec::new();
            for reply in script {
                let Value::Array(args) = read(&mut reader).unwrap() else {
                    panic!("commands are arrays")
                };
                seen.push(
                    args.into_iter()
                        .map(|arg| match arg {
                            Value::Bytes(bytes) => String::from_utf8(bytes).unwrap(),
                            other => panic!("argument {other:?}"),
                        })
                        .collect(),
                );
                writer.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (url, server)
    }

    #[test]
    fn commands_and_replies_round_trip() {
        let (url, server) = fake(vec![
            "+OK\r\n",
            "$5\r\nhello\r\n",
            "$-1\r\n",
            ":1\r\n",
            "*2\r\n$1\r\na\r\n:7\r\n",
            "-WRONGTYPE nope\r\n",
            "*2\r\n:3\r\n:59000\r\n",
        ]);
        let redis = Redis::connect(&url).unwrap();
        redis
            .set("k", "hello", Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(redis.get("k").unwrap().as_deref(), Some("hello"));
        assert_eq!(redis.get("missing").unwrap(), None);
        assert!(redis.del("k").unwrap());
        assert_eq!(
            redis.command(&["LRANGE", "l", "0", "-1"]).unwrap(),
            Value::Array(vec![Value::Bytes(b"a".to_vec()), Value::Int(7)])
        );
        assert!(
            matches!(redis.command(&["INCR", "l"]), Err(Error::Server(m)) if m.starts_with("WRONGTYPE"))
        );
        // An error reply keeps the connection: the fake accepts only one.
        assert_eq!(
            redis.hit("ip:1", Duration::from_secs(60)).unwrap(),
            (3, Duration::from_secs(59))
        );
        let seen = server.join().unwrap();
        assert_eq!(seen[0], ["SET", "k", "hello", "PX", "2000"]);
        assert_eq!(seen[6][0], "EVAL");
        assert_eq!(seen[6][2..], ["1", "ip:1", "60000"]);
    }

    #[test]
    fn auth_and_select_run_on_connect() {
        let (url, server) = fake(vec!["+OK\r\n", "+OK\r\n", "+PONG\r\n"]);
        let address = url.trim_start_matches("redis://");
        let redis = Redis::connect(&format!("redis://app:secret@{address}/2")).unwrap();
        assert_eq!(
            redis.command(&["PING"]).unwrap(),
            Value::Status("PONG".into())
        );
        let seen = server.join().unwrap();
        assert_eq!(
            seen,
            [
                vec!["AUTH", "app", "secret"],
                vec!["SELECT", "2"],
                vec!["PING"]
            ]
        );
    }

    #[test]
    fn a_dropped_connection_reconnects_on_the_next_call() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("redis://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            // First connection: read the command, hang up without a reply.
            let (stream, _) = listener.accept().unwrap();
            let _ = read(&mut BufReader::new(stream.try_clone().unwrap()));
            drop(stream);
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read(&mut BufReader::new(stream.try_clone().unwrap()));
            stream.write_all(b"+PONG\r\n").unwrap();
        });
        let redis = Redis::connect(&url).unwrap();
        assert!(matches!(
            redis.command(&["PING"]),
            Err(Error::Protocol(_) | Error::Io(_))
        ));
        assert_eq!(
            redis.command(&["PING"]).unwrap(),
            Value::Status("PONG".into())
        );
        server.join().unwrap();
    }

    #[test]
    fn an_unreachable_redis_fails_fast_until_the_backoff_passes() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let redis = Redis::connect(&format!("redis://127.0.0.1:{port}")).unwrap();
        assert!(
            matches!(redis.command(&["PING"]), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused)
        );
        assert!(
            matches!(redis.command(&["PING"]), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotConnected)
        );
    }

    /// Against a real server, in CI: `RUSTCLAMP_TEST_REDIS_URL`; skipped without it.
    #[test]
    fn a_real_redis() {
        let Ok(url) = std::env::var("RUSTCLAMP_TEST_REDIS_URL") else {
            return;
        };
        let redis = Redis::connect(&url).unwrap();
        let key = format!("rustclamp-test:{}", std::process::id());
        redis.set(&key, "ü", Some(Duration::from_secs(5))).unwrap();
        assert_eq!(redis.get(&key).unwrap().as_deref(), Some("ü"));
        assert!(redis.del(&key).unwrap());
        assert!(!redis.del(&key).unwrap());
        let window = Duration::from_secs(10);
        assert_eq!(redis.hit(&key, window).unwrap().0, 1);
        let (count, left) = redis.hit(&key, window).unwrap();
        assert_eq!(count, 2);
        assert!(left <= window && left > Duration::from_secs(8), "{left:?}");
        assert!(matches!(
            redis.command(&["NOSUCHCOMMAND"]),
            Err(Error::Server(_))
        ));
        assert!(
            redis.del(&key).unwrap(),
            "the connection survives an error reply"
        );
    }

    #[test]
    fn urls() {
        let parsed = |url| parse_url(url).unwrap();
        assert_eq!(parsed("redis://localhost").address, "localhost:6379");
        assert_eq!(parsed("redis://10.0.0.5:6380/3").address, "10.0.0.5:6380");
        assert_eq!(parsed("redis://10.0.0.5:6380/3").database, 3);
        assert_eq!(parsed("redis://h/").database, 0);
        assert_eq!(parsed("redis://[::1]").address, "[::1]:6379");
        assert_eq!(parsed("redis://[::1]:7000").address, "[::1]:7000");
        let auth = parsed("redis://:p%40ss@h");
        assert_eq!(
            (auth.username, auth.password.as_deref()),
            (None, Some("p@ss"))
        );
        for bad in [
            "rediss://h",
            "http://h",
            "redis://",
            "redis://h/x",
            "redis://secret@h",
            "redis://:%zz@h",
        ] {
            assert!(matches!(parse_url(bad), Err(Error::Url(_))), "{bad}");
        }
    }
}
