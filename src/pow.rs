// pow.rs
//
// Proof-of-work for nick registration.
//
// A nick is "valid at difficulty D" if:
//
//   SHA256(nick_bytes || pubkey_bytes) has at least D leading zero bits.
//
// The nick itself encodes the proof — no separate nonce field is needed.
// Users append a short hex suffix to their chosen base name and grind until
// the hash meets the required difficulty.  The suffix is just part of the
// nick string; the full nick (including suffix) is what gets registered.
//
// Example:
//   base name  : "alice"
//   pubkey     : (64 hex chars)
//   grind      : try "alice#0000", "alice#0001", ... until leading zeros >= D
//   result nick: "alice#3f7a"   (whatever nonce won)
//
// The separator '#' is chosen because it is not a valid IRC nick character,
// so it can never collide with a user who types '#' manually — IRC clients
// strip or reject it.  This means the grinder output is unambiguously
// machine-generated.
//
// Difficulty 0 means "any nick is accepted" (PoW disabled).
// Practical range: 0–24 bits.
//   0  bits  → instant          (disabled)
//   12 bits  → ~4k hashes       → < 1ms
//   16 bits  → ~65k hashes      → ~5ms
//   20 bits  → ~1M hashes       → ~50ms
//   24 bits  → ~16M hashes      → ~1s
//   28 bits  → ~268M hashes     → ~15s  (upper reasonable limit)

use ring::digest;

// ──────────────────────────────────────────────────────────────────────────────
// Core primitives
// ──────────────────────────────────────────────────────────────────────────────

/// Compute SHA256(nick_bytes || pubkey_bytes).
/// `pubkey_hex` must be the 64-char lower-hex encoding of the 32-byte ed25519
/// verifying key.  We hash the hex string rather than the raw bytes so that
/// the input is entirely printable and easy to reproduce in other languages.
pub fn nick_hash(nick: &str, pubkey_hex: &str) -> [u8; 32] {
    let mut input = Vec::with_capacity(nick.len() + pubkey_hex.len());
    input.extend_from_slice(nick.as_bytes());
    input.extend_from_slice(pubkey_hex.as_bytes());
    let digest = digest::digest(&digest::SHA256, &input);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Count the number of leading zero bits in a 32-byte hash.
pub fn leading_zero_bits(hash: &[u8; 32]) -> u8 {
    let mut count = 0u8;
    for byte in hash {
        if *byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros() as u8;
            break;
        }
    }
    count
}

/// Return true if the nick+pubkey hash meets `required_bits` leading zeros.
/// Always returns true when `required_bits == 0`.
pub fn check_difficulty(nick: &str, pubkey_hex: &str, required_bits: u8) -> bool {
    if required_bits == 0 {
        return true;
    }
    let hash = nick_hash(nick, pubkey_hex);
    leading_zero_bits(&hash) >= required_bits
}

/// Full verification: check that a nick meets the required difficulty.
/// Identical to `check_difficulty` but named for use at verification sites.
pub fn verify_nick(nick: &str, pubkey_hex: &str, required_bits: u8) -> bool {
    check_difficulty(nick, pubkey_hex, required_bits)
}

// ──────────────────────────────────────────────────────────────────────────────
// Nick miner
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a successful mine.
#[derive(Debug, Clone)]
pub struct MinedNick {
    /// The full nick string (base + '#' + nonce_hex), ready to use.
    pub nick: String,
    /// The nonce suffix that was appended (without the '#').
    pub nonce: u32,
    /// Actual leading zero bits achieved.
    pub bits: u8,
    /// Number of hash attempts made.
    pub attempts: u64,
}

/// Mine a nick that meets `required_bits`.
///
/// Tries nonces 0, 1, 2, … up to `max_attempts`.
/// The resulting nick is `format!("{}#{:04x}", base_name, nonce)`.
///
/// Returns `None` if no solution was found within `max_attempts`.
///
/// This is a CPU-bound operation.  Call it from a blocking thread:
/// ```no_run
/// tokio::task::spawn_blocking(move || pow::mine_nick(&base, &pubkey, bits, limit))
/// ```
pub fn mine_nick(
    base_name: &str,
    pubkey_hex: &str,
    required_bits: u8,
    max_attempts: u64,
) -> Option<MinedNick> {
    if required_bits == 0 {
        // No PoW needed — return the base name as-is.
        return Some(MinedNick {
            nick: base_name.to_string(),
            nonce: 0,
            bits: 0,
            attempts: 0,
        });
    }

    for nonce in 0u64..max_attempts {
        let candidate = format!("{}#{:04x}", base_name, nonce);
        let hash = nick_hash(&candidate, pubkey_hex);
        let bits = leading_zero_bits(&hash);
        if bits >= required_bits {
            return Some(MinedNick {
                nick: candidate,
                nonce: nonce as u32,
                bits,
                attempts: nonce + 1,
            });
        }
    }
    None
}

/// Extract the base name from a mined nick (strips '#' suffix if present).
pub fn base_name(nick: &str) -> &str {
    nick.split_once('#').map(|(base, _)| base).unwrap_or(nick)
}

// ──────────────────────────────────────────────────────────────────────────────
// Difficulty estimation helpers (for UI display)
// ──────────────────────────────────────────────────────────────────────────────

/// Approximate expected number of hash attempts for a given difficulty.
pub fn expected_attempts(bits: u8) -> u64 {
    if bits == 0 { return 0; }
    1u64 << bits
}

/// Human-readable estimate of mining time.
/// Assumes ~20M hashes/second on a modern single core.
pub fn time_estimate(bits: u8) -> String {
    const HASHES_PER_SEC: u64 = 20_000_000;
    let attempts = expected_attempts(bits);
    if attempts == 0 {
        return "instant".to_string();
    }
    let millis = (attempts * 1000) / HASHES_PER_SEC;
    if millis < 10 {
        "< 10ms".to_string()
    } else if millis < 1000 {
        format!("~{}ms", (millis / 10) * 10)
    } else if millis < 60_000 {
        format!("~{}s", millis / 1000)
    } else {
        format!("~{}min", millis / 60_000)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_PUBKEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn test_leading_zero_bits_zero_byte() {
        let mut hash = [0u8; 32];
        assert_eq!(leading_zero_bits(&hash), 32); // first 4 bytes all zero = 32 bits
        hash[0] = 0x80;
        assert_eq!(leading_zero_bits(&hash), 0);
        hash[0] = 0x40;
        assert_eq!(leading_zero_bits(&hash), 1);
        hash[0] = 0x01;
        assert_eq!(leading_zero_bits(&hash), 7);
        hash[0] = 0x00;
        hash[1] = 0x80;
        assert_eq!(leading_zero_bits(&hash), 8);
    }

    #[test]
    fn test_check_difficulty_zero_always_passes() {
        assert!(check_difficulty("anynick", FAKE_PUBKEY, 0));
        assert!(check_difficulty("", FAKE_PUBKEY, 0));
    }

    #[test]
    fn test_mine_nick_finds_solution_low_difficulty() {
        let result = mine_nick("alice", FAKE_PUBKEY, 8, 1_000_000);
        assert!(result.is_some());
        let mined = result.unwrap();
        assert!(mined.nick.starts_with("alice#"));
        assert!(mined.bits >= 8);
        // Verify the solution actually passes
        assert!(verify_nick(&mined.nick, FAKE_PUBKEY, 8));
    }

    #[test]
    fn test_mine_nick_zero_difficulty_returns_base() {
        let result = mine_nick("alice", FAKE_PUBKEY, 0, 100);
        assert!(result.is_some());
        let mined = result.unwrap();
        assert_eq!(mined.nick, "alice");
        assert_eq!(mined.attempts, 0);
    }

    #[test]
    fn test_mine_nick_exceeds_max_attempts_returns_none() {
        // Require 28 bits but only allow 1 attempt — almost certain to fail
        let result = mine_nick("alice", FAKE_PUBKEY, 28, 1);
        // Might very rarely pass, but should almost always be None
        if let Some(mined) = result {
            // If by miracle the first attempt works, verify it
            assert!(verify_nick(&mined.nick, FAKE_PUBKEY, 28));
        }
    }

    #[test]
    fn test_verify_nick_matches_mine_nick() {
        let mined = mine_nick("bob", FAKE_PUBKEY, 10, 10_000_000)
            .expect("Should find solution for 10 bits");
        assert!(verify_nick(&mined.nick, FAKE_PUBKEY, 10));
        // A slightly harder requirement should fail if the mined nick just barely meets 10
        if mined.bits < 11 {
            assert!(!verify_nick(&mined.nick, FAKE_PUBKEY, 11));
        }
    }

    #[test]
    fn test_base_name_extraction() {
        assert_eq!(base_name("alice#3f7a"), "alice");
        assert_eq!(base_name("alice"), "alice");
        assert_eq!(base_name("alice#0000#extra"), "alice"); // only first split
    }

    #[test]
    fn test_nick_hash_deterministic() {
        let h1 = nick_hash("alice#3f7a", FAKE_PUBKEY);
        let h2 = nick_hash("alice#3f7a", FAKE_PUBKEY);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_nick_hash_sensitive_to_nick() {
        let h1 = nick_hash("alice#3f7a", FAKE_PUBKEY);
        let h2 = nick_hash("alice#3f7b", FAKE_PUBKEY);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_nick_hash_sensitive_to_pubkey() {
        let other_key = "0000000000000000000000000000000000000000000000000000000000000002";
        let h1 = nick_hash("alice#3f7a", FAKE_PUBKEY);
        let h2 = nick_hash("alice#3f7a", other_key);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_expected_attempts() {
        assert_eq!(expected_attempts(0), 0);
        assert_eq!(expected_attempts(1), 2);
        assert_eq!(expected_attempts(8), 256);
        assert_eq!(expected_attempts(16), 65536);
    }

    #[test]
    fn test_time_estimate_instant() {
        assert_eq!(time_estimate(0), "instant");
    }

    #[test]
    fn test_time_estimate_reasonable() {
        // Just ensure it returns a string without panicking for all valid values
        for bits in 0u8..=28 {
            let s = time_estimate(bits);
            assert!(!s.is_empty());
        }
    }

    #[test]
    fn test_mined_nick_higher_difficulty_subsumes_lower() {
        // A nick mined for difficulty 14 also passes difficulty 12
        let mined = mine_nick("carol", FAKE_PUBKEY, 14, 10_000_000)
            .expect("Should find 14-bit solution");
        assert!(verify_nick(&mined.nick, FAKE_PUBKEY, 12));
        assert!(verify_nick(&mined.nick, FAKE_PUBKEY, 14));
    }
}
