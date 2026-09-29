//! Password hashing, encryption and message digests, like Laravel's `Hash`
//! and `Crypt`. Enabled by the `crypto` feature; built on the audited
//! RustCrypto crates, never on hand-written primitives.
//!
//! - [`struct@Hash`]: Argon2id password hashes in the standard `$argon2id$...` form.
//! - [`Key`] and [`Crypt`]: authenticated encryption (XChaCha20-Poly1305) with
//!   the app's `APP_KEY`, written `base64:...` as in Laravel.
//! - [`sha256`] and [`hmac_sha256`]: digests and signatures, such as for
//!   signed URLs; [`hmac_verify`] compares in constant time.
//!
//! ```
//! use rustclamp::crypto::{Crypt, Hash, Key};
//!
//! let hash = Hash::make("correct horse");
//! assert!(Hash::check("correct horse", &hash));
//! assert!(!Hash::check("wrong", &hash));
//!
//! let crypt = Crypt::new(&Key::generate());
//! let sealed = crypt.encrypt(b"card ending 4242");
//! assert_eq!(crypt.decrypt(&sealed).unwrap(), b"card ending 4242");
//! assert!(Crypt::new(&Key::generate()).decrypt(&sealed).is_none(), "wrong key");
//! ```

use std::fmt;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use base64ct::{Base64, Encoding};
use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// Password hashing with Argon2id.
pub struct Hash;

impl Hash {
    /// A salted Argon2id hash of `password`, in the PHC string form that
    /// carries its own parameters. Store it; never store the password.
    ///
    /// # Panics
    ///
    /// When the operating system's random source fails.
    pub fn make(password: &str) -> String {
        Argon2::default()
            .hash_password(password.as_bytes())
            .expect("password hashing needs the operating system random source")
            .to_string()
    }

    /// Whether `password` matches `hash` from [`make`](Self::make). A
    /// malformed hash is a mismatch.
    pub fn check(password: &str, hash: &str) -> bool {
        PasswordHash::new(hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    }
}

/// A 256-bit encryption key. Displays as `base64:...`, the form `APP_KEY`
/// holds; `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; 32]);

impl Key {
    /// A new random key.
    ///
    /// # Panics
    ///
    /// When the operating system's random source fails.
    pub fn generate() -> Self {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).expect("keys need the operating system random source");
        Self(bytes)
    }

    /// Reads `base64:...` (the `base64:` prefix is optional).
    pub fn parse(text: &str) -> Option<Self> {
        let encoded = text.trim().strip_prefix("base64:").unwrap_or(text.trim());
        let bytes = Base64::decode_vec(encoded).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    /// The `APP_KEY` key.
    ///
    /// # Panics
    ///
    /// When `APP_KEY` is missing or not a key: encrypting without one would
    /// lose data, so the app stops at startup. `clamp key:generate` writes one.
    pub fn from_config(config: &Config) -> Self {
        let text = config
            .get("APP_KEY")
            .unwrap_or_else(|| panic!("config key APP_KEY is missing; run clamp key:generate"));
        Self::parse(text)
            .unwrap_or_else(|| panic!("config key APP_KEY is set but is not a base64: key"))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "base64:{}", Base64::encode_string(&self.0))
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

/// Authenticated encryption with one [`Key`]. A changed or truncated
/// ciphertext, or one sealed with another key, fails to decrypt.
pub struct Crypt {
    cipher: XChaCha20Poly1305,
}

impl Crypt {
    /// Encrypts and decrypts with `key`.
    pub fn new(key: &Key) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new_from_slice(&key.0).expect("keys are 32 bytes"),
        }
    }

    /// `plaintext` sealed as base64 text: a random 24-byte nonce, then the
    /// ciphertext and its tag. Encrypting the same text twice differs.
    pub fn encrypt(&self, plaintext: &[u8]) -> String {
        let nonce = XNonce::generate();
        let sealed = self
            .cipher
            .encrypt(&nonce, plaintext)
            .expect("encryption of an in-memory buffer does not fail");
        let mut out = nonce.to_vec();
        out.extend(sealed);
        Base64::encode_string(&out)
    }

    /// The plaintext of [`encrypt`](Self::encrypt)'s output, or `None` when it
    /// was changed, is not ours or is not base64.
    pub fn decrypt(&self, sealed: &str) -> Option<Vec<u8>> {
        let bytes = Base64::decode_vec(sealed.trim()).ok()?;
        if bytes.len() < 24 {
            return None;
        }
        let (nonce, ciphertext) = bytes.split_at(24);
        let nonce = XNonce::try_from(nonce).ok()?;
        self.cipher.decrypt(&nonce, ciphertext).ok()
    }
}

/// The SHA-256 digest of `data`, as lowercase hex.
pub fn sha256(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// The HMAC-SHA-256 of `data` under `key`, as lowercase hex: a signature
/// only holders of `key` can make.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    hex(&mac.finalize().into_bytes())
}

/// Whether `signature` is [`hmac_sha256`] of `data` under `key`, compared in
/// constant time so timing does not reveal a partly right guess.
pub fn hmac_verify(key: &[u8], data: &[u8], signature: &str) -> bool {
    let Some(expected) = unhex(signature) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.verify_slice(&expected).is_ok()
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
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_match_known_answers() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // RFC 4231, test case 2.
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let signature = hmac_sha256(b"key", b"/download/7");
        assert!(hmac_verify(b"key", b"/download/7", &signature));
        assert!(!hmac_verify(b"key", b"/download/8", &signature));
        assert!(!hmac_verify(b"key", b"/download/7", "zz"));
    }

    #[test]
    fn keys_round_trip_and_stay_secret() {
        let key = Key::generate();
        let text = key.to_string();
        assert!(text.starts_with("base64:"));
        assert_eq!(Key::parse(&text), Some(key.clone()));
        assert_eq!(format!("{key:?}"), "Key(..)");
        assert_eq!(Key::parse("base64:c2hvcnQ="), None, "too short");
        let config = Config::parse(&format!("APP_KEY={text}"));
        assert_eq!(Key::from_config(&config), key);
    }

    #[test]
    fn tampering_fails_to_decrypt() {
        let crypt = Crypt::new(&Key::generate());
        let sealed = crypt.encrypt(b"secret");
        assert_ne!(sealed, crypt.encrypt(b"secret"), "a fresh nonce each time");
        let mut bytes = Base64::decode_vec(&sealed).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(crypt.decrypt(&Base64::encode_string(&bytes)).is_none());
        assert!(crypt.decrypt("not base64!").is_none());
        assert!(crypt.decrypt("AAAA").is_none());
    }

    #[test]
    fn malformed_hash_is_a_mismatch() {
        assert!(!Hash::check("x", "not a hash"));
        assert!(Hash::make("same") != Hash::make("same"), "salted");
    }

    #[test]
    #[should_panic(expected = "APP_KEY is missing")]
    fn missing_key_stops_the_app() {
        Key::from_config(&Config::default());
    }
}
