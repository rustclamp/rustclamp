# ADR 0016: Registration, Verification, Password Reset and Confirmation

Status: Accepted, 2026-09-30. Extends ADR 0013 (auth) on ADR 0015 (mail).

## Context

Phase 11 B2 adds the account flows Laravel apps get from Fortify: register, verify email, forgot and reset password, and confirm password. They are the first mail sent to an address someone typed, which ADR 0015's layer 3 forbids unless it goes through a signed URL with its own throttle.

Decided with Lex on 2026-09-30: registration works like Laravel's (on, behind one switch); auth mail is throttled at 3 an hour per client and 1 per 10 minutes per address; it shares the app's daily cap.

## Decision

- **Links are built from `APP_URL`, never from the request's `Host`**, and signed with HMAC-SHA-256 under `APP_KEY` over a purpose, the user's `public_id`, their email and an expiry (checked in constant time). With `auth` and `mail` both on, a missing `APP_URL` or `APP_KEY` stops the app at startup. Expired and tampered links get the same answer.
- **Tokens stay out of logs:** signatures travel in the query string (the request log records paths only), and the reset link's GET moves its token into the session and redirects to a clean `/reset-password`, so neither nginx logs nor `Referer` carry it.
- **Verification** links last 24 hours and set `email_verified_at` (a new, nullable `users` column). A `verified` guard sends unverified users to a notice page with a resend button. Accounts made with `user:create` are verified.
- **Reset** links last 60 minutes and also sign a fingerprint of the current password hash, so a link stops working once the password changes: single use without a tokens table.
- **A password change ends every session.** Login stores a fingerprint of the password hash in the session; `authenticate`, which reloads the user on each request anyway, logs out any session whose fingerprint no longer matches (Laravel's `AuthenticateSession`).
- **No account enumeration:** registering an address that already has an account, and asking to reset an unknown address, give the same answer as success and send nothing. Registration therefore does not log the new user in; the verification link confirms the address and the user then logs in.
- **Recipients:** the framework's own verification and reset mail may address an account's email through a crate-private constructor; apps still cannot build a `Recipient` from a string.
- **Throttles:** every auth mail is limited per client (3 an hour, `TRUST_PROXY` aware) and per address (1 per 10 minutes), both before any account lookup, and counts against `MAIL_DAILY_CAP`.
- **Confirm password** records the time in the session; a `password_confirmed` guard asks again after 3 hours and returns to the page that asked.

## Consequences

Apps with `auth` need `email_verified_at` on `users` (a migration) and `APP_URL`/`APP_KEY`. A spray of registrations can use the daily cap and delay other mail to the next day; the throttles bound it to 3 mails an hour per client.
