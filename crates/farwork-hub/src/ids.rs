//! Identifier shapes copied from upstream so clients never learn a second
//! convention for the same object kind.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;

/// `secrets.token_urlsafe(n)` equivalent: n random bytes, base64url, no padding.
pub fn token_urlsafe(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(&buf)
}

pub fn random_hex(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn user_id() -> String {
    // upstream: f"usr_{secrets.token_hex(12)}"
    format!("usr_{}", random_hex(12))
}

pub fn connector_id() -> String {
    format!("conn_{}", token_urlsafe(10))
}

pub fn project_id() -> String {
    format!("proj_{}", token_urlsafe(10))
}

pub fn session_id() -> String {
    format!("sess_{}", token_urlsafe(10))
}

pub fn timeline_item_id() -> String {
    format!("itm_{}", token_urlsafe(12))
}

/// Connector stream id. The ingress contract spells this "<uuid>", so it is a
/// UUID rather than one of the prefixed ids above.
pub fn stream_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `fs.request` correlation id. The contract spells it "<uuid>", like streamId.
pub fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Device terminal id. The contract spells this "<uuid>" and the hub is the only
/// side that mints it, so it is a UUID rather than one of the prefixed ids.
pub fn terminal_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Share id. Upstream `infra/repositories/shares.py` mints
/// `shr_<secrets.token_urlsafe(24)>`; the id is the only thing a public link
/// carries, so it stays opaque and unguessable.
pub fn share_id() -> String {
    format!("shr_{}", token_urlsafe(24))
}

/// Pairing id. The code a user types is not this id: the id is what the browser
/// polls (`GET /pairing/{id}`) and never leaves the account that minted it.
pub fn pairing_id() -> String {
    format!("pair_{}", token_urlsafe(10))
}

/// Connector device credential (`cxt_` prefix, 32 random bytes).
pub fn connector_token() -> String {
    format!("cxt_{}", token_urlsafe(32))
}
