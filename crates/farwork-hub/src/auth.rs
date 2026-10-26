//! Password verifiers and bearer tokens.
//!
//! Wire-compatible with upstream `server/agent_server/core/auth.py` and
//! `web-next/src/features/auth/password-verifier.ts`:
//!
//! * verifier = base64url-no-pad(PBKDF2-HMAC-SHA256(password, salt_utf8, 120000, 32 bytes))
//! * stored   = pbkdf2_sha256<dollar><salt><dollar><verifier>
//! * token    = HS256 JWT `{"alg":"HS256","typ":"JWT"}` / `{"exp":..,"sub":..}`,
//!   base64url without padding, 7 day lifetime for user access tokens.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{ApiError, ApiResult};
use crate::timeutil;

pub const PBKDF2_ITERATIONS: u32 = 120_000;
pub const VERIFIER_BYTES: usize = 32;
pub const USER_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 7;
pub const HASH_ALGORITHM: &str = "pbkdf2_sha256";
const SEPARATOR: char = '$';

type HmacSha256 = Hmac<Sha256>;

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.trim_end_matches('=')).ok()
}

/// Derive the client-side verifier from a plaintext password. The browser
/// derives this locally; the server only needs it for the plaintext fallback.
pub fn derive_password_verifier(password: &str, salt: &str) -> String {
    let mut out = [0u8; VERIFIER_BYTES];
    pbkdf2::pbkdf2_hmac::<Sha256>(
        password.as_bytes(),
        salt.as_bytes(),
        PBKDF2_ITERATIONS,
        &mut out,
    );
    b64url_encode(&out)
}

pub fn hash_password_verifier(verifier: &str, salt: &str) -> ApiResult<String> {
    if verifier.is_empty() {
        return Err(ApiError::unprocessable("password verifier is required"));
    }
    Ok(format!(
        "{HASH_ALGORITHM}{SEPARATOR}{salt}{SEPARATOR}{verifier}"
    ))
}

pub fn password_salt_from_hash(stored: &str) -> Option<&str> {
    let mut parts = stored.splitn(3, SEPARATOR);
    let algorithm = parts.next()?;
    let salt = parts.next()?;
    if algorithm != HASH_ALGORITHM || salt.is_empty() {
        return None;
    }
    Some(salt)
}

fn expected_verifier(stored: &str) -> Option<&str> {
    let mut parts = stored.splitn(3, SEPARATOR);
    let algorithm = parts.next()?;
    let _salt = parts.next()?;
    let expected = parts.next()?;
    if algorithm != HASH_ALGORITHM {
        return None;
    }
    Some(expected)
}

/// Constant-time comparison; only the length check can short-circuit.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn verify_password_verifier(stored: &str, verifier: &str) -> bool {
    match expected_verifier(stored) {
        Some(expected) => constant_time_eq(expected.as_bytes(), verifier.as_bytes()),
        None => false,
    }
}

/// Plaintext fallback: derive with the stored salt. Equivalent to the browser
/// derivation, so both login paths agree on the stored verifier.
pub fn verify_password(stored: &str, password: &str) -> bool {
    match password_salt_from_hash(stored) {
        Some(salt) => {
            let derived = derive_password_verifier(password, salt);
            verify_password_verifier(stored, &derived)
        }
        None => false,
    }
}

fn sign(secret: &str, signing_input: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(signing_input.as_bytes());
    b64url_encode(&mac.finalize().into_bytes())
}

pub fn create_user_access_token(secret: &str, user_id: &str) -> String {
    create_user_access_token_with_ttl(secret, user_id, USER_TOKEN_TTL_SECONDS)
}

pub fn create_user_access_token_with_ttl(secret: &str, user_id: &str, ttl_seconds: i64) -> String {
    // serde_json object maps are sorted, matching upstream's sort_keys=True.
    let header = serde_json::json!({"alg": "HS256", "typ": "JWT"}).to_string();
    let payload = serde_json::json!({
        "sub": user_id,
        "exp": timeutil::unix_now() + ttl_seconds,
    })
    .to_string();
    let signing_input = format!(
        "{}.{}",
        b64url_encode(header.as_bytes()),
        b64url_encode(payload.as_bytes())
    );
    let signature = sign(secret, &signing_input);
    format!("{signing_input}.{signature}")
}

pub fn verify_user_access_token(secret: &str, token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let expected = sign(secret, &format!("{header}.{payload}"));
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return None;
    }
    let raw = b64url_decode(payload)?;
    let data: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let exp = data.get("exp").and_then(|value| value.as_i64())?;
    if exp < timeutil::unix_now() {
        return None;
    }
    data.get("sub")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

/// Resolve the caller from an `Authorization` header value.
pub fn user_id_from_authorization(secret: &str, authorization: Option<&str>) -> ApiResult<String> {
    const PREFIX: &str = "Bearer ";
    let header =
        authorization.ok_or_else(|| ApiError::unauthorized("missing user access token"))?;
    let token = header
        .strip_prefix(PREFIX)
        .ok_or_else(|| ApiError::unauthorized("missing user access token"))?;
    verify_user_access_token(secret, token)
        .ok_or_else(|| ApiError::unauthorized("invalid user access token"))
}

/// Random salt in the shape upstream emits (`secrets.token_urlsafe(16)`).
pub fn random_salt() -> String {
    crate::ids::token_urlsafe(16)
}

pub fn sha256_hex(value: &[u8]) -> String {
    use sha2::Digest as _;
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_round_trip_matches_plaintext_derivation() {
        let salt = "c2FsdHktc2FsdA";
        let verifier = derive_password_verifier("correct horse", salt);
        let stored = hash_password_verifier(&verifier, salt).expect("hash");
        assert_eq!(password_salt_from_hash(&stored), Some(salt));
        assert!(verify_password_verifier(&stored, &verifier));
        assert!(!verify_password_verifier(&stored, "nope"));
        assert!(verify_password(&stored, "correct horse"));
        assert!(!verify_password(&stored, "wrong horse"));
    }

    #[test]
    fn verifier_is_base64url_without_padding() {
        let verifier = derive_password_verifier("pw", "salt");
        assert!(!verifier.contains('='));
        assert!(!verifier.contains('+'));
        assert!(!verifier.contains('/'));
        assert_eq!(verifier.len(), 43);
    }

    #[test]
    fn token_round_trip_and_expiry() {
        let token = create_user_access_token("secret", "usr_abc");
        assert_eq!(
            verify_user_access_token("secret", &token).as_deref(),
            Some("usr_abc")
        );
        assert!(verify_user_access_token("other", &token).is_none());
        let expired = create_user_access_token_with_ttl("secret", "usr_abc", -10);
        assert!(verify_user_access_token("secret", &expired).is_none());
    }

    #[test]
    fn token_lifetime_is_seven_days() {
        let token = create_user_access_token("secret", "usr_abc");
        let payload = token.split('.').nth(1).expect("payload");
        let raw = b64url_decode(payload).expect("decode");
        let data: serde_json::Value = serde_json::from_slice(&raw).expect("json");
        let exp = data["exp"].as_i64().expect("exp");
        let delta = exp - timeutil::unix_now();
        assert!((USER_TOKEN_TTL_SECONDS - 5..=USER_TOKEN_TTL_SECONDS).contains(&delta));
    }
}
