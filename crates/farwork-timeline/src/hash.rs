//! Canonical JSON and the content hash that makes repeated writes free.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::item::{ItemStatus, Role, TimelineItemKind};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Canonicalise a JSON value into a byte-stable string.
///
/// Object keys are sorted, separators are minimal, numbers keep their JSON
/// spelling, and non-ASCII text stays unescaped. That combination matches what
/// upstream gets from `json.dumps(value, ensure_ascii=False, sort_keys=True,
/// separators=(",", ":"))`, so a hash computed here equals a hash computed by the
/// Python connector for the same payload — which is what lets both sides agree on
/// "unchanged".
///
/// Array order is preserved on purpose: `[1,2]` and `[2,1]` are different content
/// and must hash differently, because timeline payloads use arrays for ordered
/// things (tool call arguments, message parts).
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical_json(&mut out, value);
    out
}

/// SHA-256 over the canonical JSON of one item's runtime-owned state.
///
/// Upstream computes `contentHash` over `{type, status, role, content}`
/// (`connector/runtime_protocol/timeline.py:583`), not over the payload alone, and
/// that choice is load-bearing: a run that only flips `running` to `done` reports
/// the same payload, so a payload-only hash would classify a real status transition
/// as "unchanged" and silently drop it. This function keeps the upstream field set.
/// The returned digest is the full 64-character lowercase hex string.
pub fn state_hash(
    kind: TimelineItemKind,
    status: ItemStatus,
    role: Option<Role>,
    payload: &Value,
) -> String {
    let identity = json!({
        "type": kind.as_str(),
        "status": status.as_str(),
        "role": role.map(Role::as_str),
        "content": payload,
    });
    sha256_hex(canonical_json(&identity).as_bytes())
}

/// Lowercase hex SHA-256 of raw bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn write_canonical_json(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_json_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_json(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_string(out, key);
                out.push(':');
                if let Some(item) = map.get(key) {
                    write_canonical_json(out, item);
                }
            }
            out.push('}');
        }
    }
}

/// Escape exactly the characters JSON requires, in the same style as `serde_json`
/// and `json.dumps(ensure_ascii=False)`, so the canonical form is byte-identical
/// across implementations rather than merely self-consistent.
fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{9}' => out.push_str("\\t"),
            '\u{a}' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\u{d}' => out.push_str("\\r"),
            control if (control as u32) < 0x20 => {
                let code = control as u32;
                out.push_str("\\u00");
                out.push(HEX_DIGITS[(code >> 4) as usize] as char);
                out.push(HEX_DIGITS[(code & 0xf) as usize] as char);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
