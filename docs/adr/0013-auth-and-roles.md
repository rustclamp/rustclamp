# ADR 0013: Authentication and Roles

Status: Accepted, 2026-09-30.

## Context

Phase 11 B brings login and roles to `rustclamp::web`, following the house rules in `~/projects/CLAUDE.md`: every user has exactly one role (`super-admin`, `admin`, app roles, `user`, `blocked`); `super-admin` bypasses every gate and `blocked` is refused by every gate; a 403 on a public route is answered as 404. Laravel apps get this from Fortify and spatie/laravel-permission.

## Decision

- An optional `auth` feature (`web` + `db` + `crypto`). It adds no dependencies: Argon2 hashing, sessions and the database already exist.
- **Mail-dependent flows wait for the mailer** (phase 11 C): registration with email verification, password reset and confirm password. Until then there is no public sign-up; users are created with `cargo run -- user:create <email> <role>`, which prints a generated password once. No seeder ships a default password.
- **Users table owned by the app**, like Laravel's skeleton migration: `id`, `public_id`, `name`, `email` (unique, stored and looked up lowercased), `password`, `role`, timestamps.
- **One `role` column instead of pivot tables.** NOT NULL with default `user`, so a user has exactly one role by construction rather than by discipline. The app lists its roles in order; `sync_role` refuses slugs outside the list. Changing a role is an update, never an insert, which is spatie's `syncRoles`, not `assignRole`.
- **The session holds only the user id.** The auth middleware loads the user on every request, so blocking a user or changing their role takes effect on their next request, and a blocked user's session is ended there.
- **One authorization function** applies the before rule: `blocked` is refused, `super-admin` is allowed, anyone else is decided by the policy. The `role(...)` route guard (at least that role, in the app's order) is a policy through the same function, so neither can skip the rule.
- **Login:** attempts are throttled per lowercased email and client (`LOGIN_PER_MINUTE`, default 5). The client is the same one the throttle middleware uses, so with `TRUST_PROXY=true` it is the forwarded address: behind nginx, where every peer is 127.0.0.1, a stranger cannot lock an admin out. An unknown email and a wrong password give the same message and cost the same Argon2 check (against a dummy hash), so neither the answer nor its timing reveals which emails exist. `blocked` is checked only after the password verifies. Login regenerates the session ID; logout is a CSRF-checked POST that clears the session and issues a new ID.
- **403 is answered as 404 centrally** in `Router::handle`, logged with `http_status_code=403`. The disguised 404 keeps the security headers and drops cookies, so its body and headers match a URL that exists nowhere. A guest on a protected page is redirected to `/login`; 401 is left alone.
- `ToValue for User` never includes the password hash.

## Consequences

Apps need a `users` migration (the web template and blog carry one). Mail-based flows are a later phase. Loading the user on every authenticated request costs one indexed query; that is the price of immediate blocking.
