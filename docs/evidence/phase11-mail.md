# Phase 11 C: mail evidence

## Live STARTTLS handshake (P11-07)

2026-09-30, `cargo test --features mail --lib -- --ignored live_starttls`
(`src/mail/smtp.rs`): EHLO, STARTTLS, a rustls handshake (ring provider)
verified against webpki-roots, EHLO again, QUIT. No AUTH, no mail sent.

| Server | Result |
| --- | --- |
| `smtp.gmail.com:587` | passed; AUTH offered only after TLS |
| `smtp-relay.brevo.com:587` (`MAIL_LIVE_HOST`) | passed; AUTH offered only after TLS |

No real mail was sent: delivery through Brevo uses the quota shared by
jobly.si, dppbase, placaj and after.si, and needs Lex's go-ahead.
