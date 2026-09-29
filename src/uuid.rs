//! UUIDs, std only. Enabled by the `uuid` feature (the `db` feature includes
//! it).
//!
//! Use [`Uuid::v7`] for public IDs: it starts with the time, so new IDs sort
//! after old ones and a unique index appends instead of scattering writes.
//! Use [`Uuid::v4`] only where the creation time must not be readable from
//! the ID, such as tokens.
//!
//! ```
//! use rustclamp::uuid::Uuid;
//!
//! let first = Uuid::v7();
//! let second = Uuid::v7();
//! assert!(first < second, "v7 sorts by creation time");
//! assert_eq!(first.to_string().len(), 36);
//! assert_eq!(first.version(), 7);
//! assert_eq!(Uuid::parse(&first.to_string()), Some(first));
//! ```

use std::fmt;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A 128-bit UUID. Displays in the usual lowercase hyphenated form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// A random UUID (version 4).
    pub fn v4() -> Self {
        let mut bytes = random();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(bytes)
    }

    /// A time-ordered UUID (version 7): 48 bits of Unix milliseconds, then
    /// random bits. IDs made by this process are strictly increasing, even
    /// within one millisecond.
    pub fn v7() -> Self {
        static LAST: Mutex<[u8; 16]> = Mutex::new([0; 16]);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis() as u64);
        let mut bytes = random();
        bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
        bytes[6] = (bytes[6] & 0x0f) | 0x70;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Same millisecond, or a clock step back: count up from the last ID
        // instead (RFC 9562, section 6.2, method 2).
        if bytes <= *last {
            bytes = increment(*last);
        }
        *last = bytes;
        Self(bytes)
    }

    /// Parses the hyphenated form, in either case.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 36
            || [8, 13, 18, 23]
                .iter()
                .any(|&dash| text.as_bytes()[dash] != b'-')
        {
            return None;
        }
        let hex: String = text.chars().filter(|&c| c != '-').collect();
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// The version: 4, 7 or whatever a parsed UUID says.
    pub fn version(&self) -> u8 {
        self.0[6] >> 4
    }

    /// The 16 bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// `bytes` plus one in the random bits after the version and variant, which
/// stay intact.
fn increment(mut bytes: [u8; 16]) -> [u8; 16] {
    for index in (0..16).rev() {
        if index == 6 || index == 8 {
            // Version and variant nibbles: carry through the low bits only.
            let (high, low) = if index == 6 {
                (bytes[6] & 0xf0, bytes[6] & 0x0f)
            } else {
                (bytes[8] & 0xc0, bytes[8] & 0x3f)
            };
            let max = if index == 6 { 0x0f } else { 0x3f };
            if low < max {
                bytes[index] = high | (low + 1);
                return bytes;
            }
            bytes[index] = high;
            continue;
        }
        if index < 6 {
            // Out of random bits: move into the next millisecond.
            let (sum, carry) = bytes[index].overflowing_add(1);
            bytes[index] = sum;
            if !carry {
                return bytes;
            }
            continue;
        }
        let (sum, carry) = bytes[index].overflowing_add(1);
        bytes[index] = sum;
        if !carry {
            return bytes;
        }
    }
    bytes
}

/// 16 bytes from the operating system's random source.
///
/// # Panics
///
/// Where `/dev/urandom` is missing (Windows): a guessable ID is worse than none.
fn random() -> [u8; 16] {
    // ponytail: std-only, Unix only, like session tokens; `getrandom` for Windows
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes))
        .expect("UUIDs need the operating system random source /dev/urandom");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_has_version_and_variant() {
        let id = Uuid::v4();
        assert_eq!(id.version(), 4);
        assert_eq!(id.as_bytes()[8] >> 6, 0b10);
        assert_ne!(id, Uuid::v4());
    }

    #[test]
    fn v7_carries_the_time_and_stays_ordered() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let ids: Vec<Uuid> = (0..1000).map(|_| Uuid::v7()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        let mut millis = [0; 8];
        millis[2..].copy_from_slice(&ids[0].as_bytes()[..6]);
        assert!(u64::from_be_bytes(millis) >= before);
        assert!(
            ids.iter()
                .all(|id| id.version() == 7 && id.as_bytes()[8] >> 6 == 0b10)
        );
    }

    #[test]
    fn increment_keeps_version_and_variant() {
        let mut bytes = [0xff; 16];
        bytes[6] = 0x7f;
        bytes[8] = 0xbf;
        bytes[..6].copy_from_slice(&[0, 0, 0, 0, 0, 1]);
        let next = increment(bytes);
        assert_eq!(next[..6], [0, 0, 0, 0, 0, 2], "carries into the time");
        assert_eq!((next[6] >> 4, next[8] >> 6), (7, 0b10));
    }

    #[test]
    fn parse_round_trips_and_rejects_junk() {
        let text = "0192f1c2-3b4a-7c5d-8e6f-a1b2c3d4e5f6";
        assert_eq!(Uuid::parse(text).unwrap().to_string(), text);
        assert_eq!(Uuid::parse(&text.to_uppercase()).unwrap().to_string(), text);
        assert_eq!(Uuid::parse("0192f1c2-3b4a-7c5d-8e6f-a1b2c3d4e5f"), None);
        assert_eq!(Uuid::parse("0192f1c2x3b4a-7c5d-8e6f-a1b2c3d4e5f6"), None);
        assert_eq!(Uuid::parse("zz92f1c2-3b4a-7c5d-8e6f-a1b2c3d4e5f6"), None);
    }
}
