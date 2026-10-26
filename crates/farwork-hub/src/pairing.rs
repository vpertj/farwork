//! Pairing codes: the one-time secret a device trades for a connector token.
//!
//! Three things live here, all of them pure so they can be unit-tested without
//! a database: code generation, code normalization/digesting, and the per-source
//! claim limiter.
//!
//! ## Why the stored digest is keyed
//!
//! `pairings.code_hash` is SHA-256 over `farwork-pairing-v1:<secret>:<code>`
//! rather than the code alone. A code is 8 characters from a 30-symbol alphabet
//! (~39 bits): a plain digest copied out of a database dump can be reversed
//! offline in minutes, and a still-pending pairing would then be claimable by
//! whoever reversed it. Feeding the hub secret in as key material makes the
//! stored value useless on its own.

use std::collections::HashMap;
use std::sync::Mutex;

use rand::Rng;

/// Crockford-style alphabet **without** the characters people misread or
/// mis-hear: I, L, O, U, 0 and 1 are gone, leaving 30 symbols. A code is read
/// off one screen and typed into another device, so confusability matters more
/// than density.
pub const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters between the hyphens; the full code is `XXXX-XXXX`.
pub const CODE_CHARS: usize = 8;

pub const DEFAULT_TTL_SECONDS: i64 = 600;
pub const MIN_TTL_SECONDS: i64 = 60;
pub const MAX_TTL_SECONDS: i64 = 3_600;

/// Claim attempts allowed from one source address in one rolling minute.
pub const CLAIM_ATTEMPTS_PER_MINUTE: usize = 10;
const WINDOW_SECONDS: i64 = 60;

/// Bucket for callers whose peer address is unknown (embedded router, tests).
/// Everything without an address shares it, so the limit still applies instead
/// of being skipped.
pub const UNKNOWN_SOURCE: &str = "unknown";

/// Never more than this many tracked sources; the keys are caller-controlled, so
/// the table needs a ceiling (see [ClaimLimiter::allows]).
const MAX_TRACKED_SOURCES: usize = 4_096;

/// `XXXX-XXXX` from uniformly random symbols.
pub fn generate_code() -> String {
    let mut rng = rand::rng();
    let mut code = String::with_capacity(CODE_CHARS + 1);
    for index in 0..CODE_CHARS {
        if index == CODE_CHARS / 2 {
            code.push('-');
        }
        code.push(CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char);
    }
    code
}

/// What gets hashed: case-folded, with the separator and any whitespace removed,
/// so `4f2k-9qxm` and ` 4F2K9QXM ` are the same code.
pub fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(|value| *value != '-' && !value.is_whitespace())
        .map(|value| value.to_ascii_uppercase())
        .collect()
}

/// A normalized code is well formed when it is exactly [CODE_CHARS] symbols from
/// [CODE_ALPHABET]. Malformed input is answered as an invalid code, never as a
/// lookup miss with a different shape.
pub fn is_well_formed(normalized: &str) -> bool {
    normalized.len() == CODE_CHARS && normalized.bytes().all(|byte| CODE_ALPHABET.contains(&byte))
}

/// Digest stored in `pairings.code_hash`. Callers pass the normalized code.
pub fn hash_code(secret: &str, normalized: &str) -> String {
    crate::auth::sha256_hex(format!("farwork-pairing-v1:{secret}:{normalized}").as_bytes())
}

/// In-process sliding window of claim attempts, keyed by source address.
///
/// Deliberately not Redis: a single hub process owns the ingress surface today,
/// and a lost counter across a restart only ever loosens a brute-force budget
/// that also has to beat a one-time code with a 10 minute life.
pub struct ClaimLimiter {
    attempts: Mutex<HashMap<String, Vec<i64>>>,
}

impl ClaimLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Record one attempt from `source` and report whether it is inside budget.
    ///
    /// A refused attempt is still recorded: otherwise a caller at the limit could
    /// retry for free until the window slid past.
    pub fn allows(&self, source: &str, now_epoch: i64) -> bool {
        let mut guard = match self.attempts.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.len() >= MAX_TRACKED_SOURCES {
            // Sources are attacker-chosen strings, so drop the ones that already
            // aged out rather than growing without bound.
            let cutoff = now_epoch - WINDOW_SECONDS;
            guard.retain(|_, times| times.iter().any(|at| *at > cutoff));
        }
        let times = guard.entry(source.to_string()).or_default();
        times.retain(|at| now_epoch - *at < WINDOW_SECONDS);
        if times.len() >= CLAIM_ATTEMPTS_PER_MINUTE {
            return false;
        }
        times.push(now_epoch);
        true
    }
}

impl Default for ClaimLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_code_uses_only_unambiguous_symbols() {
        for _ in 0..200 {
            let code = generate_code();
            assert_eq!(code.len(), CODE_CHARS + 1, "shape: {code}");
            assert_eq!(code.as_bytes()[CODE_CHARS / 2], b'-', "shape: {code}");
            for byte in code.bytes().filter(|byte| *byte != b'-') {
                assert!(
                    CODE_ALPHABET.contains(&byte),
                    "{} is outside the alphabet",
                    byte as char
                );
            }
            for forbidden in *b"ILOU01" {
                assert!(
                    !code.bytes().any(|byte| byte == forbidden),
                    "{} must never appear: {code}",
                    forbidden as char
                );
            }
        }
    }

    #[test]
    fn the_alphabet_has_thirty_distinct_symbols() {
        let mut sorted = CODE_ALPHABET.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), CODE_ALPHABET.len(), "alphabet has duplicates");
        assert_eq!(CODE_ALPHABET.len(), 30);
    }

    #[test]
    fn normalization_accepts_the_shapes_a_human_produces() {
        assert_eq!(normalize_code("4f2k-9qxm"), "4F2K9QXM");
        assert_eq!(normalize_code(" 4F2K 9QXM "), "4F2K9QXM");
        assert_eq!(normalize_code("4F2K9QXM"), "4F2K9QXM");
        assert!(is_well_formed(&normalize_code("4F2K-9QXM")));
        assert!(!is_well_formed(&normalize_code("4F2K-9QX")));
        assert!(
            !is_well_formed(&normalize_code("4F2K-9QXI")),
            "I is excluded"
        );
    }

    #[test]
    fn the_digest_hides_the_code_and_depends_on_the_secret() {
        let hash = hash_code("secret-a", "4F2K9QXM");
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("4F2K9QXM"));
        assert_ne!(hash, hash_code("secret-a", "4F2K9QXZ"));
        assert_ne!(hash, hash_code("secret-b", "4F2K9QXM"));
        assert_eq!(hash, hash_code("secret-a", "4F2K9QXM"));
    }

    #[test]
    fn the_limiter_allows_ten_attempts_per_minute() {
        let limiter = ClaimLimiter::new();
        for attempt in 1..=CLAIM_ATTEMPTS_PER_MINUTE {
            assert!(limiter.allows("10.0.0.1", 1_000), "attempt {attempt}");
        }
        assert!(!limiter.allows("10.0.0.1", 1_000), "eleventh attempt");
        // A different source has its own budget.
        assert!(limiter.allows("10.0.0.2", 1_000));
    }

    #[test]
    fn the_window_slides_instead_of_resetting_on_the_minute() {
        let limiter = ClaimLimiter::new();
        for _ in 0..CLAIM_ATTEMPTS_PER_MINUTE {
            assert!(limiter.allows("10.0.0.3", 1_000));
        }
        // Still inside the minute: the budget stays spent, and a refused attempt
        // is not pushed, so it cannot extend the block either.
        assert!(!limiter.allows("10.0.0.3", 1_059));
        // At exactly one minute the earlier attempts fall out of the window, so
        // the whole budget is back without any wall-clock minute boundary.
        for attempt in 1..=CLAIM_ATTEMPTS_PER_MINUTE {
            assert!(limiter.allows("10.0.0.3", 1_060), "attempt {attempt}");
        }
        assert!(!limiter.allows("10.0.0.3", 1_061));
    }
}
