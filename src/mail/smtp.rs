//! A small SMTP submission client: STARTTLS over rustls, AUTH PLAIN, one
//! message per connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use super::{Outgoing, Settings, civil, now};

const TIMEOUT: Duration = Duration::from_secs(30);

/// The connection, plain until STARTTLS upgrades it.
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

struct Session {
    io: BufReader<Io>,
}

impl Session {
    /// One reply, all its lines: `(code, lines without the code)`.
    fn reply(&mut self) -> Result<(u16, Vec<String>), String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let read = self
                .io
                .read_line(&mut line)
                .map_err(|e| format!("read: {e}"))?;
            if read == 0 {
                return Err("the server closed the connection".into());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let code = line
                .get(..3)
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| format!("unexpected reply {line:?}"))?;
            lines.push(line.get(4..).unwrap_or_default().to_owned());
            // `250-...` continues, `250 ...` (or bare `250`) ends the reply.
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, lines));
            }
        }
    }

    /// Sends `command` and expects one of `ok`. `shown` is what an error
    /// quotes, so AUTH never echoes the password.
    fn command(&mut self, command: &str, shown: &str, ok: &[u16]) -> Result<Vec<String>, String> {
        let io = self.io.get_mut();
        io.write_all(command.as_bytes())
            .and_then(|()| io.write_all(b"\r\n"))
            .and_then(|()| io.flush())
            .map_err(|e| format!("write: {e}"))?;
        let (code, lines) = self.reply()?;
        if ok.contains(&code) {
            Ok(lines)
        } else {
            Err(format!("{shown}: {code} {}", lines.join(" ")))
        }
    }
}

/// Delivers `mail` with `settings`.
pub(super) fn send(settings: &Settings, mail: &Outgoing) -> Result<(), String> {
    let tls = match settings.encryption.as_str() {
        "tls" => true,
        "none" => false,
        other => return Err(format!("MAIL_ENCRYPTION must be tls or none, not {other}")),
    };
    let address = (settings.host.as_str(), settings.port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", settings.host))?
        .next()
        .ok_or_else(|| format!("{} has no address", settings.host))?;
    let stream = TcpStream::connect_timeout(&address, TIMEOUT)
        .map_err(|e| format!("connect {address}: {e}"))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let mut session = Session {
        io: BufReader::new(Io::Plain(stream)),
    };
    let (code, lines) = session.reply()?;
    if code != 220 {
        return Err(format!("greeting: {code} {}", lines.join(" ")));
    }
    let hello = format!("EHLO {}", domain(&settings.from_address));
    let mut capabilities = session.command(&hello, "EHLO", &[250])?;
    if tls {
        if !capabilities
            .iter()
            .any(|line| line.eq_ignore_ascii_case("STARTTLS"))
        {
            // Never fall back to plaintext: that is how STARTTLS is stripped.
            return Err("the server does not offer STARTTLS".into());
        }
        session.command("STARTTLS", "STARTTLS", &[220])?;
        if !session.io.buffer().is_empty() {
            return Err("the server sent data before the TLS handshake".into());
        }
        let Io::Plain(stream) = session.io.into_inner() else {
            unreachable!("STARTTLS runs on the plain connection")
        };
        session = Session {
            io: BufReader::new(Io::Tls(Box::new(upgrade(stream, &settings.host)?))),
        };
        capabilities = session.command(&hello, "EHLO", &[250])?;
    }
    if !settings.username.is_empty() {
        let _ = capabilities;
        let token = base64(format!("\0{}\0{}", settings.username, settings.password).as_bytes());
        session.command(&format!("AUTH PLAIN {token}"), "AUTH", &[235])?;
    }
    session.command(
        &format!("MAIL FROM:<{}>", settings.from_address),
        "MAIL FROM",
        &[250],
    )?;
    session.command(
        &format!("RCPT TO:<{}>", mail.to_address),
        "RCPT TO",
        &[250, 251],
    )?;
    session.command("DATA", "DATA", &[354])?;
    let data = message(settings, mail);
    session.command(&data, "message", &[250])?;
    let _ = session.command("QUIT", "QUIT", &[221]);
    Ok(())
}

/// The TLS stream over `stream`, checked against `host` and the web's roots.
fn upgrade(
    stream: TcpStream,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("tls: {e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| format!("tls name {host}: {e}"))?;
    let connection =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| format!("tls: {e}"))?;
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

/// The DATA section: headers, a base64 UTF-8 body, and the closing `.`.
fn message(settings: &Settings, mail: &Outgoing) -> String {
    let from = mailbox(&settings.from_name, &settings.from_address);
    let to = mailbox(&mail.to_name, &mail.to_address);
    let id = format!(
        "<{}@{}>",
        crate::uuid::Uuid::v7(),
        domain(&settings.from_address)
    );
    let mut text = format!(
        "Date: {}\r\nFrom: {from}\r\nTo: {to}\r\nSubject: {}\r\nMessage-ID: {id}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n",
        date(now()),
        header(&mail.subject)
    );
    let body = mail.body.replace("\r\n", "\n").replace('\n', "\r\n");
    for chunk in base64(body.as_bytes()).as_bytes().chunks(76) {
        text.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        text.push_str("\r\n");
    }
    // Dot-stuffing; base64 lines never start with `.`, but headers could.
    let mut data: String = text
        .split("\r\n")
        .map(|line| {
            if line.starts_with('.') {
                format!(".{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    data.push('.');
    data
}

/// `Name <address>`, the name RFC 2047 encoded when it is not ASCII.
fn mailbox(name: &str, address: &str) -> String {
    if name.is_empty() {
        format!("<{address}>")
    } else if name.is_ascii() {
        format!("\"{}\" <{address}>", name.replace(['"', '\\'], ""))
    } else {
        format!("{} <{address}>", header(name))
    }
}

/// A header value, RFC 2047 encoded words when it is not ASCII.
fn header(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    // Encoded words stay under 75 characters: at most 45 bytes each, cut on
    // character boundaries.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(c);
    }
    words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
    words.join("\r\n ")
}

fn domain(address: &str) -> &str {
    address
        .rsplit_once('@')
        .map_or("localhost", |(_, domain)| domain)
}

/// RFC 5322 date in UTC, such as `Wed, 30 Sep 2026 08:15:00 +0000`.
fn date(seconds: i64) -> String {
    let (year, month, day, hour, minute, second, weekday) = civil(seconds);
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {day:02} {} {year} {hour:02}:{minute:02}:{second:02} +0000",
        names[weekday as usize],
        months[month as usize - 1]
    )
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn base64_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }

    #[test]
    fn headers_are_encoded_and_dates_are_rfc5322() {
        assert_eq!(header("Hello"), "Hello");
        assert_eq!(header("Živjo"), "=?UTF-8?B?xb1pdmpv?=");
        assert!(
            header(&"č".repeat(40))
                .lines()
                .all(|line| line.trim().len() <= 75)
        );
        assert_eq!(date(1_790_755_200), "Wed, 30 Sep 2026 08:00:00 +0000");
        assert_eq!(mailbox("Ada \"X\"", "a@b.si"), "\"Ada X\" <a@b.si>");
    }

    fn settings(port: u16, encryption: &str, username: &str) -> Settings {
        Settings {
            mailer: "smtp".into(),
            host: "127.0.0.1".into(),
            port,
            username: username.into(),
            password: "hunter2".into(),
            encryption: encryption.into(),
            from_address: "site@example.si".into(),
            from_name: "Spletna stran".into(),
            admin_address: String::new(),
            daily_cap: 100,
        }
    }

    fn outgoing() -> Outgoing {
        Outgoing {
            id: 1,
            to_address: "owner@example.si".into(),
            to_name: String::new(),
            subject: "Novo sporočilo".into(),
            body: ".leading dot\nline two".into(),
            attempts: 0,
        }
    }

    /// A fake server answering with `script` (one reply per received line,
    /// DATA content read until `.`) and returning what the client sent.
    fn fake_server(script: &'static [&'static str]) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut heard = Vec::new();
            let mut replies = script.iter();
            writer
                .write_all(replies.next().unwrap().as_bytes())
                .unwrap();
            let mut in_data = false;
            for reply in replies {
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return heard;
                    }
                    let line = line.trim_end_matches(['\r', '\n']).to_owned();
                    let done = !in_data || line == ".";
                    heard.push(line.clone());
                    if done {
                        in_data = line == "DATA";
                        break;
                    }
                }
                writer.write_all(reply.as_bytes()).unwrap();
            }
            heard
        });
        (port, server)
    }

    #[test]
    fn plain_conversation_with_multiline_replies() {
        let (port, server) = fake_server(&[
            "220 hi\r\n",
            "250-fake\r\n250-SIZE 1000\r\n250 8BITMIME\r\n",
            "235 ok\r\n",
            "250 ok\r\n",
            "250 ok\r\n",
            "354 go\r\n",
            "250 queued\r\n",
            "221 bye\r\n",
        ]);
        send(&settings(port, "none", "user"), &outgoing()).unwrap();
        let heard = server.join().unwrap();
        assert_eq!(heard[0], "EHLO example.si");
        assert_eq!(
            heard[1],
            format!("AUTH PLAIN {}", base64(b"\0user\0hunter2"))
        );
        assert_eq!(heard[2], "MAIL FROM:<site@example.si>");
        assert_eq!(heard[3], "RCPT TO:<owner@example.si>");
        assert!(heard.contains(&"Subject: =?UTF-8?B?Tm92byBzcG9yb8SNaWxv?=".to_owned()));
        assert!(
            heard.contains(&"From: =?UTF-8?B?U3BsZXRuYSBzdHJhbg==?= <site@example.si>".to_owned())
                || heard.contains(&"From: \"Spletna stran\" <site@example.si>".to_owned())
        );
        assert!(
            heard.contains(&base64(b".leading dot\r\nline two")),
            "body is base64 with CRLF"
        );
        assert_eq!(heard.last().unwrap(), "QUIT");
    }

    #[test]
    fn tls_without_starttls_is_refused_before_any_auth() {
        let (port, server) = fake_server(&["220 hi\r\n", "250 fake\r\n", "221 bye\r\n"]);
        let problem = send(&settings(port, "tls", "user"), &outgoing()).unwrap_err();
        assert_eq!(problem, "the server does not offer STARTTLS");
        let heard = server.join().unwrap();
        assert!(
            !heard.iter().any(|line| line.starts_with("AUTH")),
            "no password in plaintext: {heard:?}"
        );
    }

    #[test]
    fn a_refusal_names_the_step_but_not_the_password() {
        let (port, server) =
            fake_server(&["220 hi\r\n", "250 fake\r\n", "535 bad credentials\r\n"]);
        let problem = send(&settings(port, "none", "user"), &outgoing()).unwrap_err();
        assert_eq!(problem, "AUTH: 535 bad credentials");
        assert!(!problem.contains("hunter2"));
        drop(server);
    }

    /// Needs the network: EHLO, STARTTLS, a verified TLS handshake with a
    /// public submission server, EHLO again. No AUTH, no mail.
    /// `cargo test --features mail -- --ignored live_starttls`, recorded in
    /// `docs/evidence/phase11-mail.md`.
    #[test]
    #[ignore = "needs the network"]
    fn live_starttls_handshake() {
        // MAIL_LIVE_HOST picks another server, such as smtp-relay.brevo.com.
        let host = std::env::var("MAIL_LIVE_HOST").unwrap_or_else(|_| "smtp.gmail.com".into());
        let host = host.as_str();
        let address = (host, 587).to_socket_addrs().unwrap().next().unwrap();
        let stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut session = Session {
            io: BufReader::new(Io::Plain(stream)),
        };
        assert_eq!(session.reply().unwrap().0, 220);
        let caps = session.command("EHLO example.si", "EHLO", &[250]).unwrap();
        assert!(
            caps.iter()
                .any(|line| line.eq_ignore_ascii_case("STARTTLS"))
        );
        session.command("STARTTLS", "STARTTLS", &[220]).unwrap();
        let Io::Plain(stream) = session.io.into_inner() else {
            unreachable!()
        };
        let mut session = Session {
            io: BufReader::new(Io::Tls(Box::new(upgrade(stream, host).unwrap()))),
        };
        let caps = session.command("EHLO example.si", "EHLO", &[250]).unwrap();
        assert!(
            caps.iter().any(|line| line.starts_with("AUTH")),
            "AUTH offered only over TLS: {caps:?}"
        );
        let _ = session.command("QUIT", "QUIT", &[221]);
    }
}
