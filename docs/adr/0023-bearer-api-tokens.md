# ADR 0023: Bearer API tokens and non-email accounts

Status: Proposed, 2026-09-30. Extends ADR 0013.

## Context

The `auth` feature is cookie sessions over an app-owned `users` table with
`email`, `role` and `public_id` (ADR 0013). Two stress-ladder apps needed more
and could only use `crypto::Hash`: level 09 (a REST API) authenticates
`Authorization: Bearer` tokens that carry a role, and level 11 (chat) uses a
bearer token for its WebSocket and has username-only accounts. Each kept its
own token table, lookup and role check, and answered 401 by hand.

## Decision

1. **A token guard beside the session guard.** `web::auth::bearer` is
   middleware that reads `Authorization: Bearer <token>`, resolves it and
   passes a `Principal { token, name, role }` to handlers as a request
   extension (`Request::with_extension`, `Request::principal()`). A client
   cannot forge one, unlike the internal header level 05 used. A missing,
   malformed, unknown or wrong token is one answer: `401` as problem JSON with
   `WWW-Authenticate: Bearer`.
2. **The app owns an `api_tokens` table**, like `users`: `id`, `public_id`
   (unique), `name`, `token_hash`, `role`, timestamps. `Auth::issue_token(db,
   name, role)` returns `{public_id}.{secret}` once; `Auth::revoke_token`
   deletes the row.
3. **Only a hash is stored, and lookup is by `public_id`.** The secret is 32
   random bytes (256 bits) as hex. Its SHA-256 is compared in constant time.
   Not `crypto::Hash` (Argon2): that is salted, so a token could not be found
   by its hash, and its cost (memory-hard, tens of milliseconds) is right for
   guessable passwords but is a per-request denial-of-service lever for a
   secret that cannot be guessed. See question 1.
4. **Roles are the app's existing roles.** A token has exactly one, from
   `Auth::roles`; `token_role("admin")` guards a route through the same
   before rule as users (`super-admin` passes, `blocked` never does, others by
   order), via `Auth::principal_at_least`. A `blocked` token is refused as an
   unknown one. `403` stays a `404` unless the router reveals it.
5. **Accounts without email (not built yet).** `users.email` becomes optional
   in the model: `Auth::login_column("username")` names the unique column
   `attempt` looks up (default `email`), and `User.email` becomes
   `Option<String>`. Email-dependent flows (ADR 0016) need an address and stay
   opt-in. A token can belong to a user (`user_id`), so blocking the user
   blocks the token; the first slice has standalone tokens only.

## Consequences

- Apps get a tested token guard instead of a local table, lookup and 401.
- One extra indexed query per token request; no session, no CSRF (bearer
  requests carry no ambient credentials).
- Breaking, when item 5 lands: `User.email` becomes `Option<String>`.
- No token expiry, scopes, or `last_used_at` yet: each is a column and a
  `WHERE`, added when an app needs one.

## Open questions

1. SHA-256 of a random secret (this ADR) or `crypto::Hash` as issue #33
   suggests? The former is standard for high-entropy tokens and cheap per
   request; the latter reuses existing code but costs an Argon2 check on every
   API call.
2. Standalone tokens with their own role, or tokens owned by a user and
   inheriting that user's (single) role, so blocking a user ends their tokens?
   Level 09 wants roles per token; level 11 wants per-user tokens.
3. Item 5's shape: a configurable login column, or a separate `Account` trait
   the app implements? The first is smaller; the second avoids changing `User`.
4. Should tokens expire by default, and should `bearer` refresh `last_used_at`
   (a write per API request)?
