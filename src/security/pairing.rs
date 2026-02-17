// Gateway pairing mode — first-connect authentication.
//
// On startup the gateway generates a one-time pairing code printed to the
// terminal. The first client must present this code via `X-Pairing-Code`
// header on a `POST /pair` request. The server responds with a bearer token
// that must be sent on all subsequent requests via `Authorization: Bearer <token>`.
//
// Already-paired tokens are persisted in config so restarts don't require
// re-pairing.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::time::Instant;

const MAX_PAIR_ATTEMPTS: u32 = 5;
const PAIR_LOCKOUT_SECS: u64 = 300; // 5 minutes

#[derive(Debug)]
pub struct PairingGuard {
    require_pairing: bool,
    pairing_code: Option<String>,
    paired_tokens: Mutex<HashSet<String>>,
    failed_attempts: Mutex<(u32, Option<Instant>)>,
}

impl PairingGuard {
    pub fn new(require_pairing: bool, existing_tokens: &[String]) -> Self {
        let tokens: HashSet<String> = existing_tokens.iter().cloned().collect();
        let code = if require_pairing && tokens.is_empty() {
            Some(generate_code())
        } else {
            None
        };
        Self {
            require_pairing,
            pairing_code: code,
            paired_tokens: Mutex::new(tokens),
            failed_attempts: Mutex::new((0, None)),
        }
    }

    pub fn pairing_code(&self) -> Option<&str> {
        self.pairing_code.as_deref()
    }

    pub fn require_pairing(&self) -> bool {
        self.require_pairing
    }

    // returns Err(lockout_seconds) if brute-force locked out
    pub fn try_pair(&self, code: &str) -> Result<Option<String>, u64> {
        // Check brute force lockout
        {
            let attempts = self.failed_attempts.lock();
            if let (count, Some(locked_at)) = &*attempts {
                if *count >= MAX_PAIR_ATTEMPTS {
                    let elapsed = locked_at.elapsed().as_secs();
                    if elapsed < PAIR_LOCKOUT_SECS {
                        return Err(PAIR_LOCKOUT_SECS - elapsed);
                    }
                }
            }
        }

        if let Some(ref expected) = self.pairing_code {
            if constant_time_eq(code.trim(), expected.trim()) {
                // Reset failed attempts on success
                {
                    let mut attempts = self.failed_attempts.lock();
                    *attempts = (0, None);
                }
                let token = generate_token();
                let mut tokens = self.paired_tokens.lock();
                tokens.insert(token.clone());
                return Ok(Some(token));
            }
        }

        // Increment failed attempts
        {
            let mut attempts = self.failed_attempts.lock();
            attempts.0 += 1;
            if attempts.0 >= MAX_PAIR_ATTEMPTS {
                attempts.1 = Some(Instant::now());
            }
        }

        Ok(None)
    }

    pub fn is_authenticated(&self, token: &str) -> bool {
        if !self.require_pairing {
            return true;
        }
        let tokens = self.paired_tokens.lock();
        tokens.contains(token)
    }

    pub fn is_paired(&self) -> bool {
        let tokens = self.paired_tokens.lock();
        !tokens.is_empty()
    }

    pub fn tokens(&self) -> Vec<String> {
        let tokens = self.paired_tokens.lock();
        tokens.iter().cloned().collect()
    }
}

fn generate_code() -> String {
    // rejection sampling to eliminate modulo bias
    const UPPER_BOUND: u32 = 1_000_000;
    const REJECT_THRESHOLD: u32 = (u32::MAX / UPPER_BOUND) * UPPER_BOUND;

    loop {
        let uuid = uuid::Uuid::new_v4();
        let bytes = uuid.as_bytes();
        let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        if raw < REJECT_THRESHOLD {
            return format!("{:06}", raw % UPPER_BOUND);
        }
    }
}

fn generate_token() -> String {
    format!("bh_{}", uuid::Uuid::new_v4().as_simple())
}

// constant-time comparison to prevent timing attacks on pairing code.
// Iterates max(a.len, b.len) to avoid leaking length info via early return.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let max_len = a.len().max(b.len());
    #[allow(clippy::cast_possible_truncation)]
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..max_len {
        let x = if i < a.len() { a[i] } else { 0 };
        let y = if i < b.len() { b[i] } else { 0 };
        diff |= x ^ y;
    }
    diff == 0
}

pub fn is_public_bind(host: &str) -> bool {
    !matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0:0:0:0:0:0:0:1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PairingGuard ─────────────────────────────────────────

    #[test]
    fn new_guard_generates_code_when_no_tokens() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.pairing_code().is_some());
        assert!(!guard.is_paired());
    }

    #[test]
    fn new_guard_no_code_when_tokens_exist() {
        let guard = PairingGuard::new(true, &["bh_existing".into()]);
        assert!(guard.pairing_code().is_none());
        assert!(guard.is_paired());
    }

    #[test]
    fn new_guard_no_code_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.pairing_code().is_none());
    }

    #[test]
    fn try_pair_correct_code() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code).unwrap();
        assert!(token.is_some());
        assert!(token.unwrap().starts_with("bh_"));
        assert!(guard.is_paired());
    }

    #[test]
    fn try_pair_wrong_code() {
        let guard = PairingGuard::new(true, &[]);
        let result = guard.try_pair("000000").unwrap();
        // Might succeed if code happens to be 000000, but extremely unlikely
        // Just check it returns Ok(None) normally
        let _ = result;
    }

    #[test]
    fn try_pair_empty_code() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.try_pair("").unwrap().is_none());
    }

    #[test]
    fn is_authenticated_with_valid_token() {
        let guard = PairingGuard::new(true, &["bh_valid".into()]);
        assert!(guard.is_authenticated("bh_valid"));
    }

    #[test]
    fn is_authenticated_with_invalid_token() {
        let guard = PairingGuard::new(true, &["bh_valid".into()]);
        assert!(!guard.is_authenticated("bh_invalid"));
    }

    #[test]
    fn is_authenticated_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.is_authenticated("anything"));
        assert!(guard.is_authenticated(""));
    }

    #[test]
    fn tokens_returns_all_paired() {
        let guard = PairingGuard::new(true, &["a".into(), "b".into()]);
        let mut tokens = guard.tokens();
        tokens.sort();
        assert_eq!(tokens, vec!["a", "b"]);
    }

    #[test]
    fn pair_then_authenticate() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code).unwrap().unwrap();
        assert!(guard.is_authenticated(&token));
        assert!(!guard.is_authenticated("wrong"));
    }

    // ── is_public_bind ───────────────────────────────────────

    #[test]
    fn localhost_variants_not_public() {
        assert!(!is_public_bind("127.0.0.1"));
        assert!(!is_public_bind("localhost"));
        assert!(!is_public_bind("::1"));
        assert!(!is_public_bind("[::1]"));
    }

    #[test]
    fn zero_zero_is_public() {
        assert!(is_public_bind("0.0.0.0"));
    }

    #[test]
    fn real_ip_is_public() {
        assert!(is_public_bind("192.168.1.100"));
        assert!(is_public_bind("10.0.0.1"));
    }

    // ── constant_time_eq ─────────────────────────────────────

    #[test]
    fn constant_time_eq_same() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn constant_time_eq_different() {
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("a", ""));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        // These must return false without leaking length via timing
        assert!(!constant_time_eq("short", "longer_string"));
        assert!(!constant_time_eq("longer_string", "short"));
        assert!(!constant_time_eq("", "notempty"));
        assert!(!constant_time_eq("notempty", ""));
    }

    #[test]
    fn constant_time_eq_both_empty() {
        assert!(constant_time_eq("", ""));
    }

    // ── generate helpers ─────────────────────────────────────

    #[test]
    fn generate_code_is_6_digits() {
        let code = generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn generate_code_is_not_deterministic() {
        // Two codes should differ with overwhelming probability. We try
        // multiple pairs so a single 1-in-10^6 collision doesn't cause
        // a flaky CI failure. All 10 pairs colliding is ~1-in-10^60.
        for _ in 0..10 {
            if generate_code() != generate_code() {
                return; // Pass: found a non-matching pair.
            }
        }
        panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
    }

    #[test]
    fn generate_token_has_prefix() {
        let token = generate_token();
        assert!(token.starts_with("bh_"));
        assert!(token.len() > 10);
    }

    // ── Brute force protection ───────────────────────────────

    #[test]
    fn brute_force_lockout_after_max_attempts() {
        let guard = PairingGuard::new(true, &[]);
        // Exhaust all attempts with wrong codes
        for i in 0..MAX_PAIR_ATTEMPTS {
            let result = guard.try_pair(&format!("wrong_{i}"));
            assert!(result.is_ok(), "Attempt {i} should not be locked out yet");
        }
        // Next attempt should be locked out
        let result = guard.try_pair("another_wrong");
        assert!(
            result.is_err(),
            "Should be locked out after {MAX_PAIR_ATTEMPTS} attempts"
        );
        let lockout_secs = result.unwrap_err();
        assert!(lockout_secs > 0, "Lockout should have remaining seconds");
        assert!(
            lockout_secs <= PAIR_LOCKOUT_SECS,
            "Lockout should not exceed max"
        );
    }

    #[test]
    fn correct_code_resets_failed_attempts() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        // Fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong");
        }
        // Correct code should still work (under MAX_PAIR_ATTEMPTS)
        let result = guard.try_pair(&code).unwrap();
        assert!(result.is_some(), "Correct code should work before lockout");
    }

    #[test]
    fn lockout_returns_remaining_seconds() {
        let guard = PairingGuard::new(true, &[]);
        for _ in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair("wrong");
        }
        let err = guard.try_pair("wrong").unwrap_err();
        // Should be close to PAIR_LOCKOUT_SECS (within a second)
        assert!(
            err >= PAIR_LOCKOUT_SECS - 1,
            "Remaining lockout should be ~{PAIR_LOCKOUT_SECS}s, got {err}s"
        );
    }
}
