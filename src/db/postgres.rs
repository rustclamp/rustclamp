//! A small Postgres client over std TCP: protocol 3.0, SCRAM-SHA-256,
//! optional TLS, simple and extended queries in text format (ADR 0034).
//!
//! [`Db`](super::Db) uses it through `pool`. Not included: MD5 and channel
//! binding, binary format, `COPY`, `LISTEN`, cancellation, prepared-statement
//! caching, client certificates.

use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use base64ct::{Base64, Encoding};
use hmac::{Hmac, KeyInit, Mac};
use rustls::pki_types::pem::PemObject;
use sha2::{Digest, Sha256};

/// Largest backend message accepted, so a bad length can't allocate gigabytes.
// ponytail: a single row or value over this fails the query; raise it if an app stores such rows.
const MAX_MESSAGE: usize = 256 << 20;
const SCRAM_ITERATIONS_MAX: u32 = 10_000_000;

/// How the connection is encrypted. There is no `prefer`: a downgrade
/// can't happen silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SslMode {
    /// Plain TCP.
    Disable,
    /// Encrypted, but the certificate is not checked: protects against
    /// eavesdropping, not against an active attacker.
    Require,
    /// Encrypted, and the certificate must be valid for the host.
    VerifyFull,
}

impl SslMode {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "disable" => Ok(Self::Disable),
            "require" => Ok(Self::Require),
            "verify-full" => Ok(Self::VerifyFull),
            other => Err(format!(
                "sslmode must be disable, require or verify-full, not {other}"
            )),
        }
    }

    /// `verify-full` for any host but this machine, as ADR 0034 sets.
    pub(crate) fn default_for(host: &str) -> Self {
        if matches!(host, "localhost" | "127.0.0.1" | "::1") {
            Self::Disable
        } else {
            Self::VerifyFull
        }
    }
}

/// Where and how to connect.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) database: String,
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) sslmode: SslMode,
    /// A PEM file of CA certificates that replaces the web's roots (`verify-full`).
    pub(crate) root_cert: Option<String>,
    pub(crate) application_name: Option<String>,
    /// Command-line style session settings, `-csearch_path=app` (the `options` startup parameter).
    pub(crate) options: Option<String>,
    /// Connect, read and write timeout.
    pub(crate) timeout: Duration,
}

impl Config {
    /// Reads `postgres://user:pass@host:port/db?sslmode=..&sslrootcert=..&application_name=..&options=..`.
    /// User and password are percent-decoded. Unknown parameters are an error.
    pub(crate) fn parse_url(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))
            .ok_or("the database URL must start with postgres://")?;
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
        let (userinfo, hostport) = authority.rsplit_once('@').unwrap_or(("", authority));
        let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or("unterminated [ in the host")?;
            (host, after.strip_prefix(':'))
        } else {
            match hostport.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (hostport, None),
            }
        };
        if host.is_empty() {
            return Err("the database URL has no host".into());
        }
        let port = match port {
            None | Some("") => 5432,
            Some(port) => port.parse().map_err(|_| format!("bad port {port:?}"))?,
        };
        let mut config = Self {
            host: host.to_owned(),
            port,
            database: percent_decode(database)?,
            user: percent_decode(user)?,
            password: percent_decode(password)?,
            sslmode: SslMode::default_for(host),
            root_cert: None,
            application_name: None,
            options: None,
            timeout: Duration::from_secs(30),
        };
        if config.database.is_empty() {
            config.database.clone_from(&config.user);
        }
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value)?;
            match key {
                "sslmode" => config.sslmode = SslMode::parse(&value)?,
                "sslrootcert" => config.root_cert = Some(value),
                "application_name" => config.application_name = Some(value),
                "options" => config.options = Some(value),
                other => return Err(format!("unsupported database URL parameter {other}")),
            }
        }
        Ok(config)
    }
}

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).ok_or("truncated %-escape")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| format!("bad %-escape %{hex}"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "the database URL is not UTF-8".into())
}

/// A value sent as a parameter or read from a column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Datum {
    Null,
    Integer(i64),
    Real(f64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
}

impl Datum {
    /// The text-format encoding, `None` for `NULL`.
    fn encode(&self) -> Option<String> {
        Some(match self {
            Self::Null => return None,
            Self::Integer(n) => n.to_string(),
            Self::Real(n) => n.to_string(),
            Self::Bool(true) => "t".into(),
            Self::Bool(false) => "f".into(),
            Self::Text(text) => text.clone(),
            Self::Bytes(bytes) => format!("\\x{}", hex(bytes)),
        })
    }

    /// Reads a text-format column of type `oid`. Types not listed come back as text.
    fn decode(oid: u32, raw: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(raw).map_err(|_| "a column is not valid UTF-8")?;
        let bad = |kind: &str| format!("bad {kind} {text:?} from the server");
        Ok(match oid {
            20 | 21 | 23 => Self::Integer(text.parse().map_err(|_| bad("integer"))?),
            700 | 701 => Self::Real(text.parse().map_err(|_| bad("float"))?),
            16 => Self::Bool(text == "t"),
            17 => Self::Bytes(
                text.strip_prefix("\\x")
                    .and_then(unhex)
                    .ok_or_else(|| bad("bytea"))?,
            ),
            _ => Self::Text(text.to_owned()),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// An `ErrorResponse` from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerError {
    /// The five-character SQLSTATE, such as `23505` for a unique violation.
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) detail: Option<String>,
}

/// Why a call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// The server refused the statement; the connection is still usable.
    Server(ServerError),
    /// Connect, TLS, auth, I/O or protocol trouble. After an I/O or protocol
    /// error [`Client::is_broken`] is true and the connection must be dropped.
    Connection(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(e) => {
                write!(f, "{} (SQLSTATE {})", e.message, e.code)?;
                match &e.detail {
                    Some(detail) => write!(f, ": {detail}"),
                    None => Ok(()),
                }
            }
            Self::Connection(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

fn refuse(message: impl Into<String>) -> Error {
    Error::Connection(message.into())
}

/// The result of one statement.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Rows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Datum>>,
    /// Rows inserted, updated, deleted or selected, from the command tag.
    pub(crate) affected: u64,
}

/// The connection, plain or encrypted.
enum Io {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Io {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Io {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

/// One connection. Not `Sync`; a pool lends it to one caller at a time.
pub(crate) struct Client {
    io: BufReader<Io>,
    broken: bool,
    /// From the last `ReadyForQuery`: inside a transaction block, failed or not.
    in_transaction: bool,
}

impl Client {
    /// Connects, encrypts per `config.sslmode`, authenticates, and waits for `ReadyForQuery`.
    pub(crate) fn connect(config: &Config) -> Result<Self, Error> {
        let mut nonce = [0u8; 18];
        getrandom::fill(&mut nonce).map_err(|e| refuse(format!("no random source: {e}")))?;
        Self::connect_with_nonce(config, &Base64::encode_string(&nonce))
    }

    fn connect_with_nonce(config: &Config, nonce: &str) -> Result<Self, Error> {
        // Every address in turn (`localhost` is `::1` then `127.0.0.1`), like libpq.
        let mut last = format!("{} has no address", config.host);
        let mut stream = None;
        for address in (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|e| refuse(format!("resolve {}: {e}", config.host)))?
        {
            match TcpStream::connect_timeout(&address, config.timeout) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(e) => last = format!("connect {address}: {e}"),
            }
        }
        let mut stream = stream.ok_or_else(|| refuse(last))?;
        let _ = stream.set_read_timeout(Some(config.timeout));
        let _ = stream.set_write_timeout(Some(config.timeout));
        let _ = stream.set_nodelay(true);
        let encrypted = config.sslmode != SslMode::Disable;
        let io = if encrypted {
            let mut request = Vec::new();
            request.extend_from_slice(&8i32.to_be_bytes());
            request.extend_from_slice(&80_877_103i32.to_be_bytes());
            stream
                .write_all(&request)
                .map_err(|e| refuse(format!("write: {e}")))?;
            // Exactly one unbuffered byte, so nothing a middlebox appends is read as plaintext.
            let mut answer = [0u8; 1];
            stream
                .read_exact(&mut answer)
                .map_err(|e| refuse(format!("read: {e}")))?;
            match answer[0] {
                b'S' => Io::Tls(Box::new(upgrade(stream, config).map_err(refuse)?)),
                // Never fall back to plaintext: that is how TLS is stripped.
                b'N' => return Err(refuse("the server does not support TLS")),
                other => return Err(refuse(format!("unexpected SSLRequest answer {other:#x}"))),
            }
        } else {
            Io::Plain(stream)
        };
        let mut client = Self {
            io: BufReader::new(io),
            broken: false,
            in_transaction: false,
        };
        if let Err(e) = client.startup(config, encrypted, nonce) {
            client.broken = true; // no Terminate to a server that never finished the handshake
            return Err(e);
        }
        Ok(client)
    }

    /// Whether a transaction is open (`BEGIN` ran, `COMMIT` or `ROLLBACK` has not).
    pub(crate) fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Whether an I/O or protocol error left the connection unusable.
    pub(crate) fn is_broken(&self) -> bool {
        self.broken
    }

    /// Runs one parameterized statement (extended protocol, text format).
    pub(crate) fn query(&mut self, sql: &str, params: &[Datum]) -> Result<Rows, Error> {
        if sql.contains('\0') {
            return Err(refuse("the SQL contains a NUL byte"));
        }
        if params.len() > usize::from(i16::MAX as u16) {
            return Err(refuse("too many parameters"));
        }
        let mut out = Vec::new();
        let mut parse = cstr("");
        parse.extend(cstr(sql));
        parse.extend((params.len() as i16).to_be_bytes());
        parse.extend(params.iter().flat_map(|_| 0u32.to_be_bytes()));
        frame(&mut out, b'P', &parse);
        let mut bind = cstr(""); // portal
        bind.extend(cstr("")); // statement
        bind.extend(0i16.to_be_bytes()); // every parameter in text format
        bind.extend((params.len() as i16).to_be_bytes());
        for param in params {
            match param.encode() {
                None => bind.extend((-1i32).to_be_bytes()),
                Some(text) => {
                    bind.extend((text.len() as i32).to_be_bytes());
                    bind.extend(text.as_bytes());
                }
            }
        }
        bind.extend(0i16.to_be_bytes()); // every result in text format
        frame(&mut out, b'B', &bind);
        let mut describe = vec![b'P'];
        describe.extend(cstr(""));
        frame(&mut out, b'D', &describe);
        let mut execute = cstr("");
        execute.extend(0i32.to_be_bytes());
        frame(&mut out, b'E', &execute);
        frame(&mut out, b'S', &[]);
        self.send(&out)?;
        self.collect()
    }

    /// Runs `sql`, which may hold several statements, with the simple protocol.
    pub(crate) fn execute_batch(&mut self, sql: &str) -> Result<(), Error> {
        if sql.contains('\0') {
            return Err(refuse("the SQL contains a NUL byte"));
        }
        let mut out = Vec::new();
        frame(&mut out, b'Q', &cstr(sql));
        self.send(&out)?;
        self.collect().map(|_| ())
    }

    /// Reads one statement's responses up to `ReadyForQuery`. After an
    /// `ErrorResponse` it still drains to `ReadyForQuery`, so the connection stays usable.
    fn collect(&mut self) -> Result<Rows, Error> {
        let mut result = Rows::default();
        let mut types = Vec::new();
        let mut error = None;
        loop {
            let (tag, body) = self.read_message()?;
            let mut r = Reader(&body);
            match tag {
                b'T' => {
                    for _ in 0..r.i16().map_err(|e| self.fail(e))? {
                        let name = r.cstr().map_err(|e| self.fail(e))?;
                        r.take(6).map_err(|e| self.fail(e))?; // table oid, column number
                        let oid = r.i32().map_err(|e| self.fail(e))? as u32;
                        r.take(8).map_err(|e| self.fail(e))?; // size, modifier, format
                        result.columns.push(name);
                        types.push(oid);
                    }
                }
                b'D' => {
                    let row = self.data_row(&mut r, &types)?;
                    result.rows.push(row);
                }
                b'C' => {
                    let tag = r.cstr().map_err(|e| self.fail(e))?;
                    result.affected = tag
                        .rsplit(' ')
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                }
                b'E' => {
                    error.get_or_insert(parse_error(&body));
                }
                b'Z' => {
                    self.in_transaction = body.first() != Some(&b'I');
                    return match error {
                        Some(e) => Err(Error::Server(e)),
                        None => Ok(result),
                    };
                }
                // ParseComplete, BindComplete, NoData, EmptyQueryResponse,
                // CloseComplete, ParameterStatus, Notice, Notification.
                b'1' | b'2' | b'3' | b'n' | b'I' | b'S' | b'N' | b'A' => {}
                other => return Err(self.fail(format!("unexpected message {:?}", other as char))),
            }
        }
    }

    fn data_row(&mut self, r: &mut Reader<'_>, types: &[u32]) -> Result<Vec<Datum>, Error> {
        let mut row = Vec::with_capacity(types.len());
        let count = r.i16().map_err(|e| self.fail(e))?;
        if usize::try_from(count).ok() != Some(types.len()) {
            return Err(self.fail("a row does not match its description"));
        }
        for oid in types {
            let len = r.i32().map_err(|e| self.fail(e))?;
            row.push(if len < 0 {
                Datum::Null
            } else {
                let raw = r.take(len as usize).map_err(|e| self.fail(e))?;
                Datum::decode(*oid, raw).map_err(|e| self.fail(e))?
            });
        }
        Ok(row)
    }

    fn startup(&mut self, config: &Config, encrypted: bool, nonce: &str) -> Result<(), Error> {
        let mut body = 196_608i32.to_be_bytes().to_vec(); // protocol 3.0
        let mut pairs = vec![
            ("user", config.user.as_str()),
            ("database", config.database.as_str()),
            ("client_encoding", "UTF8"),
        ];
        if let Some(name) = &config.application_name {
            pairs.push(("application_name", name));
        }
        if let Some(options) = &config.options {
            pairs.push(("options", options));
        }
        for (key, value) in pairs {
            body.extend(cstr(key));
            body.extend(cstr(value));
        }
        body.push(0);
        let mut out = ((body.len() + 4) as i32).to_be_bytes().to_vec();
        out.extend(body);
        self.send(&out)?;
        loop {
            let (tag, body) = self.read_message()?;
            match tag {
                b'R' => self.authenticate(&body, config, encrypted, nonce)?,
                b'E' => return Err(Error::Server(parse_error(&body))),
                b'Z' => return Ok(()),
                // ParameterStatus, BackendKeyData (unused: no cancellation), Notice.
                b'S' | b'K' | b'N' => {}
                other => return Err(refuse(format!("unexpected message {:?}", other as char))),
            }
        }
    }

    fn authenticate(
        &mut self,
        body: &[u8],
        config: &Config,
        encrypted: bool,
        nonce: &str,
    ) -> Result<(), Error> {
        let mut r = Reader(body);
        match r.i32().map_err(refuse)? {
            0 => Ok(()),
            3 if !encrypted => Err(refuse(
                "the server asked for a cleartext password; refusing to send it without TLS (set sslmode)",
            )),
            3 => {
                let mut out = Vec::new();
                frame(&mut out, b'p', &cstr(&config.password));
                self.send(&out)
            }
            5 => Err(refuse(
                "the server asked for MD5 password authentication, which is not supported; use scram-sha-256",
            )),
            10 => {
                let mut offered = false;
                while let Ok(name) = r.cstr() {
                    offered |= name == "SCRAM-SHA-256";
                }
                if !offered {
                    return Err(refuse(
                        "the server offers no SASL mechanism but SCRAM-SHA-256-PLUS",
                    ));
                }
                self.scram(&config.password, nonce)
            }
            other => Err(refuse(format!("unsupported authentication method {other}"))),
        }
    }

    fn scram(&mut self, password: &str, nonce: &str) -> Result<(), Error> {
        // Postgres takes the user from the startup message and ignores the SCRAM name.
        let mut scram = Scram::new("", password, nonce);
        let first = scram.first();
        let mut out = Vec::new();
        let mut body = cstr("SCRAM-SHA-256");
        body.extend((first.len() as i32).to_be_bytes());
        body.extend(first.as_bytes());
        frame(&mut out, b'p', &body);
        self.send(&out)?;
        let server_first = self.sasl(11)?;
        let last = scram.respond(&server_first).map_err(refuse)?;
        let mut out = Vec::new();
        frame(&mut out, b'p', last.as_bytes());
        self.send(&out)?;
        let server_final = self.sasl(12)?;
        scram.verify(&server_final).map_err(refuse)
    }

    /// The text of the next `Authentication` message, which must be of kind `expect`.
    fn sasl(&mut self, expect: i32) -> Result<String, Error> {
        let (tag, body) = self.read_message()?;
        match tag {
            b'E' => Err(Error::Server(parse_error(&body))),
            b'R' if body.get(..4) == Some(&expect.to_be_bytes()) => {
                String::from_utf8(body[4..].to_vec())
                    .map_err(|_| refuse("the SASL data is not UTF-8"))
            }
            _ => Err(refuse("unexpected message during SASL authentication")),
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let io = self.io.get_mut();
        match io.write_all(bytes).and_then(|()| io.flush()) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.fail(format!("write: {e}"))),
        }
    }

    fn read_message(&mut self) -> Result<(u8, Vec<u8>), Error> {
        let mut head = [0u8; 5];
        if let Err(e) = self.io.read_exact(&mut head) {
            return Err(self.fail(format!("read: {e}")));
        }
        let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]);
        let Some(len) = usize::try_from(len)
            .ok()
            .filter(|n| (4..=MAX_MESSAGE).contains(n))
        else {
            return Err(self.fail(format!("bad message length {len}")));
        };
        let mut body = vec![0; len - 4];
        if let Err(e) = self.io.read_exact(&mut body) {
            return Err(self.fail(format!("read: {e}")));
        }
        Ok((head[0], body))
    }

    /// Marks the connection broken and returns the error.
    fn fail(&mut self, message: impl Into<String>) -> Error {
        self.broken = true;
        Error::Connection(message.into())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.broken {
            let mut out = Vec::new();
            frame(&mut out, b'X', &[]);
            let io = self.io.get_mut();
            let _ = io.write_all(&out).and_then(|()| io.flush());
        }
    }
}

/// A NUL-terminated string.
fn cstr(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.push(0);
    out
}

/// Appends a tagged message: tag, length (self included), body.
fn frame(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    out.extend(((body.len() + 4) as i32).to_be_bytes());
    out.extend(body);
}

/// A cursor over a message body.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.0.len() {
            return Err("a message is shorter than it says".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn i16(&mut self) -> Result<i16, String> {
        self.take(2).map(|b| i16::from_be_bytes([b[0], b[1]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        self.take(4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn cstr(&mut self) -> Result<String, String> {
        let end = self
            .0
            .iter()
            .position(|b| *b == 0)
            .ok_or("an unterminated string")?;
        let text = String::from_utf8_lossy(&self.0[..end]).into_owned();
        self.0 = &self.0[end + 1..];
        Ok(text)
    }
}

/// `ErrorResponse` fields: a one-byte code and a string each, then a zero byte.
fn parse_error(body: &[u8]) -> ServerError {
    let mut error = ServerError {
        code: String::new(),
        message: String::new(),
        detail: None,
    };
    let mut r = Reader(body);
    while let Ok(field) = r.take(1) {
        if field[0] == 0 {
            break;
        }
        let Ok(value) = r.cstr() else { break };
        match field[0] {
            b'C' => error.code = value,
            b'M' => error.message = value,
            b'D' => error.detail = Some(value),
            _ => {}
        }
    }
    error
}

/// The SCRAM-SHA-256 client (RFC 5802, RFC 7677). No channel binding (`n,,`):
/// TLS already authenticates the server under `verify-full`, and the server
/// signature is checked either way.
// ponytail: SASLprep is not applied; the password goes in as typed. A password
// with non-ASCII characters that SASLprep would change fails to authenticate
// (an error, never a weaker login); add stringprep-style mapping if one shows up.
struct Scram {
    user: String,
    password: String,
    nonce: String,
    first_bare: String,
    auth_message: String,
    server_key: Vec<u8>,
}

impl Scram {
    fn new(user: &str, password: &str, nonce: &str) -> Self {
        Self {
            user: user.replace('=', "=3D").replace(',', "=2C"),
            password: password.to_owned(),
            nonce: nonce.to_owned(),
            first_bare: String::new(),
            auth_message: String::new(),
            server_key: Vec::new(),
        }
    }

    /// The client-first message.
    fn first(&mut self) -> String {
        self.first_bare = format!("n={},r={}", self.user, self.nonce);
        format!("n,,{}", self.first_bare)
    }

    /// From the server-first message to the client-final message.
    fn respond(&mut self, server_first: &str) -> Result<String, String> {
        let field = |key: &str| {
            server_first
                .split(',')
                .find_map(|part| part.strip_prefix(key))
                .ok_or_else(|| format!("the SCRAM challenge has no {key}"))
        };
        let combined = field("r=")?;
        if !combined.starts_with(&self.nonce) || combined.len() == self.nonce.len() {
            return Err("the SCRAM nonce does not extend ours".into());
        }
        let salt = Base64::decode_vec(field("s=")?).map_err(|_| "the SCRAM salt is not base64")?;
        let iterations: u32 = field("i=")?
            .parse()
            .map_err(|_| "bad SCRAM iteration count")?;
        if iterations == 0 || iterations > SCRAM_ITERATIONS_MAX {
            return Err(format!("unreasonable SCRAM iteration count {iterations}"));
        }
        let (client_key, stored_key, server_key) = scram_keys(&self.password, &salt, iterations);
        let without_proof = format!("c=biws,r={combined}");
        self.auth_message = format!("{},{server_first},{without_proof}", self.first_bare);
        let signature = hmac(&stored_key, self.auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(&signature)
            .map(|(a, b)| a ^ b)
            .collect();
        self.server_key = server_key;
        Ok(format!(
            "{without_proof},p={}",
            Base64::encode_string(&proof)
        ))
    }

    /// Checks the server-final message, so a server that never knew the password is caught.
    fn verify(&self, server_final: &str) -> Result<(), String> {
        let sent = server_final
            .strip_prefix("v=")
            .ok_or_else(|| format!("the SCRAM server refused: {server_final}"))?;
        let sent = Base64::decode_vec(sent).map_err(|_| "the SCRAM signature is not base64")?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.server_key).expect("HMAC takes any key length");
        mac.update(self.auth_message.as_bytes());
        // `verify_slice` compares in constant time.
        mac.verify_slice(&sent)
            .map_err(|_| "the SCRAM server signature is wrong".to_owned())
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `(ClientKey, StoredKey, ServerKey)` for a password, salt and iteration count.
fn scram_keys(password: &str, salt: &[u8], iterations: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // PBKDF2-HMAC-SHA-256 with one 32-byte block (RFC 8018).
    let base =
        Hmac::<Sha256>::new_from_slice(password.as_bytes()).expect("HMAC takes any key length");
    let mut mac = base.clone();
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u = mac.finalize().into_bytes();
    let mut salted = u;
    for _ in 1..iterations {
        let mut mac = base.clone();
        mac.update(&u);
        u = mac.finalize().into_bytes();
        salted.iter_mut().zip(u.iter()).for_each(|(s, u)| *s ^= u);
    }
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = Sha256::digest(&client_key).to_vec();
    (client_key, stored_key, hmac(&salted, b"Server Key"))
}

/// The TLS stream over `stream`: `verify-full` checks the certificate against
/// `config.root_cert` or the web's roots, `require` accepts any.
fn upgrade(
    stream: TcpStream,
    config: &Config,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls: {e}"))?;
    let tls = if config.sslmode == SslMode::Require {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        let roots = match &config.root_cert {
            None => rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            },
            Some(path) => {
                let mut roots = rustls::RootCertStore::empty();
                for cert in rustls::pki_types::CertificateDer::pem_file_iter(path)
                    .map_err(|e| format!("read {path}: {e}"))?
                {
                    roots
                        .add(cert.map_err(|e| format!("read {path}: {e}"))?)
                        .map_err(|e| format!("{path}: {e}"))?;
                }
                if roots.is_empty() {
                    return Err(format!("{path} holds no certificate"));
                }
                roots
            }
        };
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    let host = config.host.as_str();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| format!("tls name {host}: {e}"))?;
    let connection =
        rustls::ClientConnection::new(Arc::new(tls), name).map_err(|e| format!("tls: {e}"))?;
    let mut stream = rustls::StreamOwned::new(connection, stream);
    // Handshake now, so a bad certificate fails here with a clear error.
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| format!("tls handshake with {host}: {e}"))?;
    }
    Ok(stream)
}

/// `sslmode=require`: accepts any certificate. The traffic is encrypted but
/// the peer is not authenticated, as in libpq. Handshake signatures are still
/// checked, so the peer holds the key of the certificate it sent.
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn config(port: u16, sslmode: SslMode) -> Config {
        Config {
            host: "127.0.0.1".into(),
            port,
            database: "app".into(),
            user: "alice".into(),
            password: "pencil".into(),
            sslmode,
            root_cert: None,
            application_name: None,
            options: None,
            timeout: Duration::from_secs(5),
        }
    }

    /// The backend side of one connection; `script` plays the server.
    fn serve<T: Send + 'static>(
        script: impl FnOnce(&mut TcpStream) -> T + Send + 'static,
    ) -> (u16, JoinHandle<T>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            script(&mut stream)
        });
        (port, handle)
    }

    fn send(stream: &mut TcpStream, tag: u8, body: &[u8]) {
        let mut out = Vec::new();
        frame(&mut out, tag, body);
        stream.write_all(&out).unwrap();
    }

    fn auth(stream: &mut TcpStream, code: i32, rest: &[u8]) {
        send(stream, b'R', &[&code.to_be_bytes()[..], rest].concat());
    }

    fn ready(stream: &mut TcpStream) {
        send(stream, b'Z', b"I");
    }

    /// The startup message's key/value pairs.
    fn read_startup(stream: &mut TcpStream) -> Vec<String> {
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).unwrap();
        let mut body = vec![0; i32::from_be_bytes(len) as usize - 4];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(body[..4], 196_608i32.to_be_bytes());
        body[4..]
            .split(|b| *b == 0)
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect()
    }

    fn read_msg(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut head = [0u8; 5];
        stream.read_exact(&mut head).unwrap();
        let mut body =
            vec![0; i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize - 4];
        stream.read_exact(&mut body).unwrap();
        (head[0], body)
    }

    /// Trust authentication: startup, `AuthenticationOk`, a parameter, `ReadyForQuery`.
    fn handshake(stream: &mut TcpStream) -> Vec<String> {
        let startup = read_startup(stream);
        auth(stream, 0, &[]);
        send(stream, b'S', b"server_version\x0017.0\x00");
        send(stream, b'K', &[0; 8]);
        send(stream, b'N', b"SNotice\x00\x00");
        ready(stream);
        startup
    }

    fn error_body(code: &str, message: &str, detail: &str) -> Vec<u8> {
        format!("SERROR\0C{code}\0M{message}\0D{detail}\0\0").into_bytes()
    }

    fn row_description(columns: &[(&str, u32)]) -> Vec<u8> {
        let mut body = (columns.len() as i16).to_be_bytes().to_vec();
        for (name, oid) in columns {
            body.extend(cstr(name));
            body.extend([0; 6]);
            body.extend(oid.to_be_bytes());
            body.extend([0; 8]);
        }
        body
    }

    fn data_row(values: &[Option<&[u8]>]) -> Vec<u8> {
        let mut body = (values.len() as i16).to_be_bytes().to_vec();
        for value in values {
            match value {
                None => body.extend((-1i32).to_be_bytes()),
                Some(v) => {
                    body.extend((v.len() as i32).to_be_bytes());
                    body.extend(*v);
                }
            }
        }
        body
    }

    /// Reads Parse, Bind, Describe, Execute, Sync; returns Parse's and Bind's bodies.
    fn read_extended(stream: &mut TcpStream) -> (Vec<u8>, Vec<u8>) {
        let tags: Vec<_> = (0..5).map(|_| read_msg(stream)).collect();
        assert_eq!(tags.iter().map(|m| m.0).collect::<Vec<_>>(), b"PBDES");
        (tags[0].1.clone(), tags[1].1.clone())
    }

    #[test]
    fn urls_are_parsed_and_percent_decoded() {
        let c = Config::parse_url("postgres://al%40ice:p%3Ass%2Fw@db.example.com:6543/shop?sslmode=require&sslrootcert=%2Fetc%2Fca.pem&application_name=app&options=-csearch_path%3Dx").unwrap();
        assert_eq!((c.user.as_str(), c.password.as_str()), ("al@ice", "p:ss/w"));
        assert_eq!(
            (c.host.as_str(), c.port, c.database.as_str()),
            ("db.example.com", 6543, "shop")
        );
        assert_eq!(c.sslmode, SslMode::Require);
        assert_eq!(c.root_cert.as_deref(), Some("/etc/ca.pem"));
        assert_eq!(c.application_name.as_deref(), Some("app"));
        assert_eq!(c.options.as_deref(), Some("-csearch_path=x"));

        let c = Config::parse_url("postgresql://bob@localhost").unwrap();
        assert_eq!(
            (c.port, c.database.as_str(), c.sslmode),
            (5432, "bob", SslMode::Disable)
        );
        let c = Config::parse_url("postgres://u:p@[::1]:5433/d").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("::1", 5433));
        assert_eq!(
            Config::parse_url("postgres://u@db.example.com/d")
                .unwrap()
                .sslmode,
            SslMode::VerifyFull
        );

        for bad in [
            "mysql://u@h/d",
            "postgres:///d",
            "postgres://u@h:port/d",
            "postgres://u@h/d?sslmode=prefer",
            "postgres://u@h/d?surprise=1",
            "postgres://u%zz@h/d",
        ] {
            assert!(Config::parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn values_use_the_text_format() {
        assert_eq!(Datum::Integer(-7).encode().as_deref(), Some("-7"));
        assert_eq!(Datum::Real(1.5).encode().as_deref(), Some("1.5"));
        assert_eq!(Datum::Bool(true).encode().as_deref(), Some("t"));
        assert_eq!(
            Datum::Bytes(vec![0, 255, 16]).encode().as_deref(),
            Some("\\x00ff10")
        );
        assert_eq!(Datum::Null.encode(), None);
        assert_eq!(Datum::decode(23, b"42"), Ok(Datum::Integer(42)));
        assert_eq!(Datum::decode(701, b"2.5"), Ok(Datum::Real(2.5)));
        assert_eq!(Datum::decode(16, b"f"), Ok(Datum::Bool(false)));
        assert_eq!(
            Datum::decode(17, b"\\xdead"),
            Ok(Datum::Bytes(vec![0xde, 0xad]))
        );
        assert_eq!(
            Datum::decode(1114, b"2026-10-13 10:00:00"),
            Ok(Datum::Text("2026-10-13 10:00:00".into()))
        );
        assert!(Datum::decode(17, b"dead").is_err());
        assert!(Datum::decode(20, b"x").is_err());
    }

    #[test]
    fn messages_are_framed_with_a_self_inclusive_length() {
        let mut out = Vec::new();
        frame(&mut out, b'Q', &cstr("SELECT 1"));
        assert_eq!(out, b"Q\0\0\0\x0dSELECT 1\0");
        let error = parse_error(&error_body("23505", "dup", "Key (a)=(1) exists."));
        assert_eq!(error.code, "23505");
        assert_eq!(error.detail.as_deref(), Some("Key (a)=(1) exists."));
    }

    /// RFC 7677, section 3.
    #[test]
    fn scram_matches_the_rfc_7677_vector() {
        let nonce = "rOprNGfwEbeRWgbNEkqO";
        let mut scram = Scram::new("user", "pencil", nonce);
        assert_eq!(scram.first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        assert_eq!(
            scram.respond(server_first).unwrap(),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        assert!(
            scram
                .verify("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
                .is_ok()
        );
        assert!(
            scram
                .verify("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .is_err()
        );
        assert!(scram.verify("e=invalid-proof").is_err());
        // A nonce that doesn't start with ours, and no extension, are refused.
        assert!(
            Scram::new("user", "pencil", nonce)
                .respond("r=other,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096")
                .is_err()
        );
        assert!(
            Scram::new("user", "pencil", nonce)
                .respond("r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096")
                .is_err()
        );
        assert!(
            Scram::new("user", "pencil", nonce)
                .respond("r=rOprNGfwEbeRWgbNEkqOx,s=AA==,i=99999999")
                .is_err()
        );
    }

    #[test]
    fn startup_sends_the_user_and_database() {
        let (port, server) = serve(|s| {
            let startup = handshake(s);
            assert_eq!(read_msg(s).0, b'X', "Terminate on drop");
            startup
        });
        let client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        assert!(!client.is_broken());
        drop(client);
        let startup = server.join().unwrap();
        assert_eq!(&startup[..4], ["user", "alice", "database", "app"]);
    }

    /// The server half of SCRAM for `password`; `sign` is what it answers with.
    fn serve_scram(s: &mut TcpStream, password: &str, honest: bool) {
        read_startup(s);
        auth(s, 10, b"SCRAM-SHA-256-PLUS\0SCRAM-SHA-256\0\0");
        let (tag, body) = read_msg(s);
        assert_eq!(tag, b'p');
        let mut r = Reader(&body);
        assert_eq!(r.cstr().unwrap(), "SCRAM-SHA-256");
        let len = r.i32().unwrap() as usize;
        let first = String::from_utf8(r.take(len).unwrap().to_vec()).unwrap();
        let bare = first.strip_prefix("n,,").unwrap();
        let client_nonce = bare.split("r=").nth(1).unwrap();
        let (salt, iterations) = (b"0123456789abcdef", 4096);
        let combined = format!("{client_nonce}server-part");
        let server_first = format!(
            "r={combined},s={},i={iterations}",
            Base64::encode_string(salt)
        );
        auth(s, 11, server_first.as_bytes());
        let (_, body) = read_msg(s);
        let last = String::from_utf8(body).unwrap();
        let (without_proof, proof) = last.split_once(",p=").unwrap();
        assert_eq!(without_proof, format!("c=biws,r={combined}"));
        let (client_key, stored_key, server_key) = scram_keys(password, salt, iterations);
        let auth_message = format!("{bare},{server_first},{without_proof}");
        let signature = hmac(&stored_key, auth_message.as_bytes());
        let expected: Vec<u8> = client_key
            .iter()
            .zip(&signature)
            .map(|(a, b)| a ^ b)
            .collect();
        assert_eq!(
            proof,
            Base64::encode_string(&expected),
            "the proof must match the password"
        );
        let mut server_signature = hmac(&server_key, auth_message.as_bytes());
        if !honest {
            server_signature[0] ^= 1;
        }
        auth(
            s,
            12,
            format!("v={}", Base64::encode_string(&server_signature)).as_bytes(),
        );
        if honest {
            auth(s, 0, &[]);
            ready(s);
        }
    }

    #[test]
    fn scram_login_against_a_fake_server() {
        let (port, server) = serve(|s| serve_scram(s, "pencil", true));
        let client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_wrong_server_signature_is_rejected() {
        let (port, server) = serve(|s| serve_scram(s, "pencil", false));
        let error = Client::connect(&config(port, SslMode::Disable))
            .err()
            .unwrap();
        assert!(error.to_string().contains("server signature"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn md5_is_refused_with_a_clear_error() {
        let (port, server) = serve(|s| {
            read_startup(s);
            auth(s, 5, &[1, 2, 3, 4]);
            // The client must hang up without sending a password.
            let mut rest = Vec::new();
            s.read_to_end(&mut rest).unwrap_or(0);
            rest
        });
        let error = Client::connect(&config(port, SslMode::Disable))
            .err()
            .unwrap();
        assert!(error.to_string().contains("MD5"), "{error}");
        assert!(server.join().unwrap().is_empty());
    }

    #[test]
    fn cleartext_is_refused_without_tls() {
        let (port, server) = serve(|s| {
            read_startup(s);
            auth(s, 3, &[]);
            let mut rest = Vec::new();
            s.read_to_end(&mut rest).unwrap_or(0);
            rest
        });
        let error = Client::connect(&config(port, SslMode::Disable))
            .err()
            .unwrap();
        assert!(error.to_string().contains("cleartext"), "{error}");
        assert!(
            server.join().unwrap().is_empty(),
            "no password may reach the server"
        );
    }

    #[test]
    fn tls_is_never_silently_dropped() {
        let (port, server) = serve(|s| {
            let mut request = [0u8; 8];
            s.read_exact(&mut request).unwrap();
            assert_eq!(request[4..], 80_877_103i32.to_be_bytes());
            s.write_all(b"N").unwrap();
        });
        let error = Client::connect(&config(port, SslMode::VerifyFull))
            .err()
            .unwrap();
        assert!(error.to_string().contains("TLS"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn extended_query_round_trip_with_typed_results() {
        let (port, server) = serve(|s| {
            handshake(s);
            let (parse, bind) = read_extended(s);
            send(s, b'1', &[]);
            send(s, b'2', &[]);
            let columns = [
                ("id", 20),
                ("name", 25),
                ("data", 17),
                ("ok", 16),
                ("score", 701),
                ("note", 25),
            ];
            send(s, b'T', &row_description(&columns));
            send(
                s,
                b'D',
                &data_row(&[
                    Some(b"7"),
                    Some(b"Ann"),
                    Some(b"\\x00ff"),
                    Some(b"t"),
                    Some(b"1.5"),
                    None,
                ]),
            );
            send(s, b'C', b"SELECT 1\0");
            ready(s);
            (parse, bind)
        });
        let mut client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        let params = [
            Datum::Integer(7),
            Datum::Null,
            Datum::Text("x".into()),
            Datum::Bytes(vec![1]),
        ];
        let rows = client
            .query("SELECT * FROM t WHERE id = $1", &params)
            .unwrap();
        assert_eq!(rows.columns, ["id", "name", "data", "ok", "score", "note"]);
        assert_eq!(
            rows.rows,
            [vec![
                Datum::Integer(7),
                Datum::Text("Ann".into()),
                Datum::Bytes(vec![0, 255]),
                Datum::Bool(true),
                Datum::Real(1.5),
                Datum::Null,
            ]]
        );
        assert_eq!(rows.affected, 1);
        let (parse, bind) = server.join().unwrap();
        assert_eq!(&parse[..1], b"\0", "unnamed statement");
        assert!(
            parse
                .windows(29)
                .any(|w| w == b"SELECT * FROM t WHERE id = $1"[..29].as_ref())
        );
        // portal "", statement "", all-text parameter formats, 4 parameters: "7", NULL, "x", "\x01".
        let expected: Vec<u8> = [
            &b"\0\0"[..],
            &[0, 0, 0, 4],
            &[0, 0, 0, 1],
            b"7",
            &[255; 4],
            &[0, 0, 0, 1],
            b"x",
            &[0, 0, 0, 4],
            b"\\x01",
            &[0, 0],
        ]
        .concat();
        assert_eq!(bind, expected);
    }

    #[test]
    fn command_tags_give_the_affected_rows() {
        let (port, server) = serve(|s| {
            handshake(s);
            read_extended(s);
            send(s, b'1', &[]);
            send(s, b'2', &[]);
            send(s, b'n', &[]);
            send(s, b'C', b"UPDATE 3\0");
            ready(s);
        });
        let mut client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        let rows = client.query("UPDATE t SET a = 1", &[]).unwrap();
        assert_eq!((rows.affected, rows.rows.len()), (3, 0));
        server.join().unwrap();
    }

    #[test]
    fn a_server_error_leaves_the_connection_usable() {
        let (port, server) = serve(|s| {
            handshake(s);
            read_extended(s);
            send(s, b'1', &[]);
            send(
                s,
                b'E',
                &error_body(
                    "23505",
                    "duplicate key value",
                    "Key (email)=(a@b) already exists.",
                ),
            );
            ready(s);
            read_extended(s);
            send(s, b'1', &[]);
            send(s, b'2', &[]);
            send(s, b'n', &[]);
            send(s, b'C', b"INSERT 0 1\0");
            ready(s);
        });
        let mut client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        let Err(Error::Server(error)) =
            client.query("INSERT INTO t VALUES ($1)", &[Datum::Integer(1)])
        else {
            panic!("expected a server error");
        };
        assert_eq!(error.code, "23505");
        assert_eq!(
            error.detail.as_deref(),
            Some("Key (email)=(a@b) already exists.")
        );
        assert!(!client.is_broken());
        assert_eq!(
            client
                .query("INSERT INTO t VALUES ($1)", &[Datum::Integer(2)])
                .unwrap()
                .affected,
            1
        );
        server.join().unwrap();
    }

    #[test]
    fn execute_batch_reports_the_first_error() {
        let (port, server) = serve(|s| {
            handshake(s);
            let (tag, body) = read_msg(s);
            assert_eq!(
                (tag, body.as_slice()),
                (b'Q', &b"CREATE TABLE a (); CREATE TABLE a ();\0"[..])
            );
            send(s, b'C', b"CREATE TABLE\0");
            send(
                s,
                b'E',
                &error_body("42P07", "relation \"a\" already exists", ""),
            );
            ready(s);
        });
        let mut client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        let Err(Error::Server(error)) =
            client.execute_batch("CREATE TABLE a (); CREATE TABLE a ();")
        else {
            panic!("expected a server error");
        };
        assert_eq!(error.code, "42P07");
        assert!(!client.is_broken());
        server.join().unwrap();
    }

    #[test]
    fn a_dropped_connection_marks_the_client_broken() {
        let (port, server) = serve(|s| {
            handshake(s);
            read_extended(s);
            // Hang up mid-statement.
        });
        let mut client = Client::connect(&config(port, SslMode::Disable)).unwrap();
        let Err(Error::Connection(_)) = client.query("SELECT 1", &[]) else {
            panic!("expected a connection error");
        };
        assert!(client.is_broken());
        server.join().unwrap();
        assert!(client.query("SELECT 1", &[]).is_err());
    }

    /// Against a real server: `RUSTCLAMP_TEST_DATABASE_URL=postgres://.. cargo test --features postgres -- --ignored`.
    #[test]
    #[ignore = "needs a Postgres server"]
    fn live_server() {
        let url =
            std::env::var("RUSTCLAMP_TEST_DATABASE_URL").expect("set RUSTCLAMP_TEST_DATABASE_URL");
        let mut client = Client::connect(&Config::parse_url(&url).unwrap()).unwrap();
        client.execute_batch("CREATE TEMP TABLE t (id BIGINT PRIMARY KEY, name TEXT, data BYTEA, ok BOOLEAN, score DOUBLE PRECISION)").unwrap();
        let insert = "INSERT INTO t VALUES ($1, $2, $3, $4, $5)";
        let row = [
            Datum::Integer(1),
            Datum::Text("Zoë 'x'".into()),
            Datum::Bytes(vec![0, 255]),
            Datum::Bool(true),
            Datum::Real(0.25),
        ];
        assert_eq!(client.query(insert, &row).unwrap().affected, 1);
        let Err(Error::Server(error)) = client.query(insert, &row) else {
            panic!("expected a duplicate")
        };
        assert_eq!(error.code, "23505");
        let rows = client.query("SELECT * FROM t WHERE id = $1 UNION ALL SELECT 2, NULL, NULL, NULL, NULL WHERE $1 = 1", &[Datum::Integer(1)]).unwrap();
        assert_eq!(rows.rows[0], row);
        assert!(rows.rows[1][1..].iter().all(|d| *d == Datum::Null));
    }
}
