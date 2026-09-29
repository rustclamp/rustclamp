# ADR 0010: Hashing and encryption

Status: Accepted, 2026-09-29.

## Context

Apps need password hashes, encrypted values and secrets, signed URLs, and
an encrypted `.env` for deployment, as Laravel's `Hash`, `Crypt` and
`env:encrypt` provide. Cryptographic primitives must not be written by hand.

## Decision

- An optional `crypto` feature adds `rustclamp::crypto`, built on the
  RustCrypto crates: `argon2` (Argon2id, PHC strings), `chacha20poly1305`
  (XChaCha20-Poly1305 with a random 24-byte nonce per message), `sha2`,
  `hmac` and `base64ct`, with `getrandom` for keys, salts and nonces.
- `Key` is 32 bytes, written `base64:...` in `APP_KEY` as in Laravel; its
  `Debug` hides it. A missing or malformed `APP_KEY` stops the app when a
  `Key` is read from config.
- `hmac_verify` compares in constant time.
- `clamp key:generate`, `clamp env:encrypt` and `clamp env:decrypt` use the
  same `Crypt`, so `.env.encrypted` can be committed and decrypted on the
  server with a key kept outside the repository.
- `tools/boundaries.py` allows these crates' closure for `rustclamp` and
  requires the direct ones to stay optional. The default build still has no
  dependencies.

## Consequences

UUIDs stay std-only (`uuid` feature) and session tokens keep reading
`/dev/urandom`; moving both to `getrandom` would add Windows support.
