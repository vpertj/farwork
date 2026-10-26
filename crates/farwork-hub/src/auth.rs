//! Password verifiers and bearer tokens.
//!
//! Wire-compatible with upstream `server/agent_server/core/auth.py` and
//! `web-next/src/features/auth/password-verifier.ts`:
//!
//! * verifier = base64url-no-pad(PBKDF2-HMAC-SHA256(password, salt_utf8, 120000, 32 bytes))
//! * stored   = pbkdf2_sha256<dollar><salt><dollar><verifier>
//! * token    = HS256 JWT `{"alg":"HS256","typ":"JWT"}` / `{"exp":..,"sub":..,"kind":..}`,
//!   base64url without padding, 7 day lifetime for user access tokens.
//!
//! The `kind` claim is what keeps the three token families apart: an access
//! token, a mobile QR token and a mobile refresh token are signed with the same
//! secret, so without it any of them would authenticate any endpoint that takes
//! a bearer token. Tokens minted before `kind` existed decode to no kind claim
//! and are treated as access tokens — dropping them would log out every live
//! session on deploy.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{ApiError, ApiResult};
use crate::timeutil;

pub const PBKDF2_ITERATIONS: u32 = 120_000;
pub const VERIFIER_BYTES: usize = 32;
pub const USER_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 7;
/// QR login token: short enough that a photographed code is not a durable
/// credential, and single-use on top of that (see `mobile_login_tokens`).
pub const MOBILE_LOGIN_TTL_SECONDS: i64 = 120;
/// Mobile refresh token: accepted as the analogue of a 30-day session.
pub const MOBILE_REFRESH_TTL_SECONDS: i64 = 60 * 60 * 24 * 30;

/// `kind` claim values. Each verification point accepts exactly its own kind.
pub const TOKEN_KIND_ACCESS: &str = "access";
pub const TOKEN_KIND_MOBILE_LOGIN: &str = "mobile_login";
pub const TOKEN_KIND_MOBILE_REFRESH: &str = "mobile_refresh";
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

/// Sign an arbitrary claim set. Production code goes through the
/// `create_*_token` helpers below; the claim-level entry point exists so a test
/// can mint a token in the pre-`kind` shape that live sessions still hold.
pub fn create_jwt(secret: &str, claims: &serde_json::Value) -> String {
    // serde_json object maps are sorted, matching upstream's sort_keys=True.
    let header = serde_json::json!({"alg": "HS256", "typ": "JWT"}).to_string();
    let payload = claims.to_string();
    let signing_input = format!(
        "{}.{}",
        b64url_encode(header.as_bytes()),
        b64url_encode(payload.as_bytes())
    );
    let signature = sign(secret, &signing_input);
    format!("{signing_input}.{signature}")
}

pub fn create_signed_token(secret: &str, kind: &str, user_id: &str, ttl_seconds: i64) -> String {
    create_jwt(
        secret,
        &serde_json::json!({
            "sub": user_id,
            "exp": timeutil::unix_now() + ttl_seconds,
            "kind": kind,
        }),
    )
}

/// Verify signature and expiry, then return the `sub` claim — but only if the
/// token was minted for `kind`. A token for another kind is indistinguishable
/// from a forged one here: same code path, same `None`, so no caller can leak
/// "that was a refresh token" through a different rejection.
pub fn verify_signed_token(secret: &str, kind: &str, token: &str) -> Option<String> {
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
    let accepted = match data.get("kind").and_then(|value| value.as_str()) {
        Some(actual) => actual == kind,
        // No kind claim: a token from before this feature, which can only have
        // been an access token. Every other kind must say so explicitly.
        None => kind == TOKEN_KIND_ACCESS,
    };
    if !accepted {
        return None;
    }
    data.get("sub")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

pub fn create_user_access_token(secret: &str, user_id: &str) -> String {
    create_user_access_token_with_ttl(secret, user_id, USER_TOKEN_TTL_SECONDS)
}

pub fn create_user_access_token_with_ttl(secret: &str, user_id: &str, ttl_seconds: i64) -> String {
    create_signed_token(secret, TOKEN_KIND_ACCESS, user_id, ttl_seconds)
}

pub fn verify_user_access_token(secret: &str, token: &str) -> Option<String> {
    verify_signed_token(secret, TOKEN_KIND_ACCESS, token)
}

/// A per-issuance unique claim. Without it, signing is deterministic (`sub` +
/// `exp` + `kind`, one-second granularity), so two tokens for the same user in
/// the same second are byte-identical. Harmless for a bearer token; fatal for
/// the QR token, whose sha256 is the primary key of `mobile_login_tokens` — the
/// second "generate a code" click in that second would violate it. Upstream
/// carries the same idea as `nonce: secrets.token_urlsafe(12)`.
fn nonce() -> String {
    crate::ids::token_urlsafe(16)
}

/// Returns the token together with the epoch it expires at, so the stored row
/// and the signed claim cannot disagree by a second.
pub fn create_mobile_login_token(secret: &str, user_id: &str) -> (String, i64) {
    let expires_epoch = timeutil::unix_now() + MOBILE_LOGIN_TTL_SECONDS;
    let token = create_jwt(
        secret,
        &serde_json::json!({
            "sub": user_id,
            "exp": expires_epoch,
            "kind": TOKEN_KIND_MOBILE_LOGIN,
            "nonce": nonce(),
        }),
    );
    (token, expires_epoch)
}

pub fn create_mobile_refresh_token(secret: &str, user_id: &str) -> String {
    create_jwt(
        secret,
        &serde_json::json!({
            "sub": user_id,
            "exp": timeutil::unix_now() + MOBILE_REFRESH_TTL_SECONDS,
            "kind": TOKEN_KIND_MOBILE_REFRESH,
            "nonce": nonce(),
        }),
    )
}

pub fn verify_mobile_refresh_token(secret: &str, token: &str) -> Option<String> {
    verify_signed_token(secret, TOKEN_KIND_MOBILE_REFRESH, token)
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

    fn claims(token: &str) -> serde_json::Value {
        let payload = token.split('.').nth(1).expect("payload");
        serde_json::from_slice(&b64url_decode(payload).expect("decode")).expect("json")
    }

    fn exp_delta(token: &str) -> i64 {
        claims(token)["exp"].as_i64().expect("exp") - timeutil::unix_now()
    }

    #[test]
    fn token_families_do_not_interchange() {
        let access = create_user_access_token("secret", "usr_abc");
        let (login, _) = create_mobile_login_token("secret", "usr_abc");
        let refresh = create_mobile_refresh_token("secret", "usr_abc");

        assert_eq!(
            verify_user_access_token("secret", &access).as_deref(),
            Some("usr_abc")
        );
        assert_eq!(
            verify_mobile_refresh_token("secret", &refresh).as_deref(),
            Some("usr_abc")
        );

        // Cross-kind use rejects exactly like a forged token.
        assert!(verify_user_access_token("secret", &refresh).is_none());
        assert!(verify_user_access_token("secret", &login).is_none());
        assert!(verify_mobile_refresh_token("secret", &access).is_none());
        assert!(verify_mobile_refresh_token("secret", &login).is_none());
        assert!(verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &access).is_none());
        assert!(verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &refresh).is_none());
        assert_eq!(
            verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &login).as_deref(),
            Some("usr_abc")
        );
    }

    #[test]
    fn a_token_without_a_kind_claim_is_still_an_access_token() {
        // The shape every session minted before kinds existed.
        let legacy = create_jwt(
            "secret",
            &serde_json::json!({"sub": "usr_abc", "exp": timeutil::unix_now() + 60}),
        );
        assert!(claims(&legacy).get("kind").is_none());
        assert_eq!(
            verify_user_access_token("secret", &legacy).as_deref(),
            Some("usr_abc")
        );
        // Legacy leniency is scoped to access tokens only.
        assert!(verify_mobile_refresh_token("secret", &legacy).is_none());
        assert!(verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &legacy).is_none());
    }

    #[test]
    fn mobile_token_lifetimes_are_120_seconds_and_30_days() {
        let (login, expires_epoch) = create_mobile_login_token("secret", "usr_abc");
        assert_eq!(claims(&login)["kind"], TOKEN_KIND_MOBILE_LOGIN);
        assert!((exp_delta(&login) - MOBILE_LOGIN_TTL_SECONDS).abs() <= 2);
        assert!((expires_epoch - timeutil::unix_now() - MOBILE_LOGIN_TTL_SECONDS).abs() <= 2);

        let refresh = create_mobile_refresh_token("secret", "usr_abc");
        assert_eq!(claims(&refresh)["kind"], TOKEN_KIND_MOBILE_REFRESH);
        assert!((exp_delta(&refresh) - MOBILE_REFRESH_TTL_SECONDS).abs() <= 2);

        assert_eq!(
            claims(&create_user_access_token("secret", "usr_abc"))["kind"],
            TOKEN_KIND_ACCESS
        );
    }

    #[test]
    fn each_issuance_is_unique_even_within_one_second() {
        // Signing without a nonce is deterministic: same sub, same exp (one
        // second granularity), same kind => same bytes. Mint until the two share
        // an `exp`, which is exactly the same-second case, and require the
        // tokens to differ anyway.
        let (first, first_exp) = create_mobile_login_token("secret", "usr_abc");
        let mut second = create_mobile_login_token("secret", "usr_abc");
        let mut attempts = 0;
        while second.1 != first_exp {
            attempts += 1;
            assert!(attempts < 5, "the clock never repeated a second?!");
            second = create_mobile_login_token("secret", "usr_abc");
        }
        assert_eq!(claims(&first)["exp"], claims(&second.0)["exp"]);
        assert_eq!(claims(&first)["sub"], claims(&second.0)["sub"]);
        assert_ne!(
            first, second.0,
            "same-second issuance must not repeat: the QR token's sha256 is a primary key"
        );
        assert_ne!(
            claims(&first)["nonce"],
            claims(&second.0)["nonce"],
            "the nonce is what makes two issuances distinct"
        );
        assert!(!claims(&first)["nonce"]
            .as_str()
            .unwrap_or_default()
            .is_empty());
        // Both verify: the uniqueness must not have broken the signature.
        assert_eq!(
            verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &first).as_deref(),
            Some("usr_abc")
        );
        assert_eq!(
            verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &second.0).as_deref(),
            Some("usr_abc")
        );

        // Same reasoning for the refresh token, which is also re-issued on every
        // exchange of a different QR.
        let a = create_mobile_refresh_token("secret", "usr_abc");
        let b = create_mobile_refresh_token("secret", "usr_abc");
        assert_ne!(a, b);
        assert_eq!(
            verify_mobile_refresh_token("secret", &a).as_deref(),
            Some("usr_abc")
        );
    }

    #[test]
    fn an_expired_mobile_login_token_is_rejected() {
        let expired = create_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, "usr_abc", -10);
        assert!(verify_signed_token("secret", TOKEN_KIND_MOBILE_LOGIN, &expired).is_none());
    }
}
