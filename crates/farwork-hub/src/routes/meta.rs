//! `GET /api/v2/meta`: the address this hub is actually reachable at.
//!
//! Production serves the web build from the hub, so the page origin *is* the hub
//! and nothing goes wrong. Under `vite dev` (5173 proxying to 7300) or behind a
//! reverse proxy it is not, and every command built from `location.origin` is
//! wrong — `farwork-agent pair --hub http://localhost:5173` gets stored in
//! `agent.json` and the agent never connects again, while the only symptom is
//! "the agent is offline". So the hub states its own base address and the web
//! copies it instead of guessing.
//!
//! Two rules follow from that failure:
//!
//! * The port comes from the **listening configuration**, never from `Host`.
//!   Behind a proxy the header carries the proxy's address, which is precisely
//!   the number that must not be copied.
//! * `FARWORK_HUB_PUBLIC_URL` wins outright: only the operator knows the public
//!   name, port and TLS terminator in front of the hub.
//!
//! `X-Forwarded-Proto` is read for the **scheme only**. It is caller-controlled
//! unless a trusted proxy rewrites it, so it must not influence anything beyond
//! the cosmetic scheme of a URL we hand back — in particular it says nothing
//! about who the caller is.
//!
//! Unauthenticated, like `/healthz`: it reveals the hub's own address and the
//! build version, nothing about an account.

use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::config::Config;
use crate::state::AppState;
use crate::timeutil;
use crate::VERSION;

pub fn router() -> Router<AppState> {
    Router::new().route("/meta", get(get_meta))
}

async fn get_meta(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let base = base_url(state.config(), &headers);
    Json(json!({
        "hubUrl": base.hub_url,
        "version": VERSION,
        "serverTime": timeutil::utc_now(),
        // Reported alongside so a mismatched agent credential can be diagnosed
        // without guessing which half of the URL the hub believed.
        "scheme": base.scheme,
        "port": base.port,
    }))
}

/// Everything needed to build `hubUrl`, plus the two parts kept separately
/// because a wrong port is the interesting half of a wrong address.
struct BaseUrl {
    hub_url: String,
    scheme: &'static str,
    port: u16,
}

fn base_url(config: &Config, headers: &HeaderMap) -> BaseUrl {
    if let Some(public_url) = config.public_url.as_deref() {
        // Normalized at startup, so the scheme/port parse cannot fail here; the
        // fallback only keeps the response total if a Config was built by hand.
        let (scheme, port) = scheme_and_port(public_url).unwrap_or(("http", 80));
        return BaseUrl {
            hub_url: public_url.to_string(),
            scheme,
            port,
        };
    }
    let scheme = forwarded_scheme(headers).unwrap_or("http");
    let host = host_header(headers).unwrap_or_else(|| String::from("127.0.0.1"));
    let port = config.port;
    BaseUrl {
        hub_url: format!("{scheme}://{host}{}", port_suffix(scheme, port)),
        scheme,
        port,
    }
}

/// Splits a normalized `scheme://host[:port][/path]` into the scheme and the
/// port a client would dial (the scheme default when none is written).
fn scheme_and_port(url: &str) -> Option<(&'static str, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = match scheme {
        "https" => "https",
        "http" => "http",
        _ => return None,
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    // `[::1]:8080` splits into its port; a bare IPv6 literal has none to find and
    // falls through to the scheme default.
    let explicit = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok());
    let default = if scheme == "https" { 443 } else { 80 };
    Some((scheme, explicit.unwrap_or(default)))
}

/// The `Host` header with its port chopped off — the port is the proxy's, not
/// ours, and copying it is the bug this module documents.
fn host_header(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::HOST)?.to_str().ok()?.trim();
    let host = host_without_port(raw);
    (!host.is_empty()).then(|| host.to_string())
}

fn host_without_port(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('[') {
        // IPv6 literal: `[::1]:5173` -> `[::1]`.
        return match rest.find(']') {
            Some(end) => &value[..end + 2],
            None => value,
        };
    }
    match value.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => value,
    }
}

/// First entry of `X-Forwarded-Proto`, and only when it names a scheme we serve.
fn forwarded_scheme(headers: &HeaderMap) -> Option<&'static str> {
    let raw = headers.get("x-forwarded-proto")?.to_str().ok()?;
    match raw.split(',').next()?.trim().to_ascii_lowercase().as_str() {
        "https" => Some("https"),
        "http" => Some("http"),
        _ => None,
    }
}

/// Drops the port a client assumes anyway (`:80` on http, `:443` on https).
fn port_suffix(scheme: &str, port: u16) -> String {
    if (scheme == "http" && port == 80) || (scheme == "https" && port == 443) {
        String::new()
    } else {
        format!(":{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_port_keeps_the_host_and_drops_every_port_form() {
        assert_eq!(host_without_port("127.0.0.1:5173"), "127.0.0.1");
        assert_eq!(host_without_port("localhost"), "localhost");
        assert_eq!(
            host_without_port("farwork.example.com:9"),
            "farwork.example.com"
        );
        assert_eq!(host_without_port("[::1]:5173"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
    }

    #[test]
    fn scheme_and_port_defaults_per_scheme() {
        assert_eq!(
            scheme_and_port("https://hub.example.com"),
            Some(("https", 443))
        );
        assert_eq!(
            scheme_and_port("http://hub.example.com"),
            Some(("http", 80))
        );
        assert_eq!(
            scheme_and_port("http://127.0.0.1:8080"),
            Some(("http", 8080))
        );
        assert_eq!(
            scheme_and_port("https://farwork.example.com/base/"),
            Some(("https", 443))
        );
        assert_eq!(scheme_and_port("gopher://hub.example.com"), None);
    }

    #[test]
    fn forwarded_proto_takes_the_first_entry_and_ignores_junk() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().expect("value"));
        assert_eq!(forwarded_scheme(&headers), Some("https"));

        headers.insert("x-forwarded-proto", "HTTPS, http".parse().expect("value"));
        assert_eq!(forwarded_scheme(&headers), Some("https"));

        headers.insert("x-forwarded-proto", "gopher".parse().expect("value"));
        assert_eq!(forwarded_scheme(&headers), None);
    }

    #[test]
    fn port_suffix_omits_only_the_scheme_default() {
        assert_eq!(port_suffix("http", 80), "");
        assert_eq!(port_suffix("https", 443), "");
        assert_eq!(port_suffix("http", 443), ":443");
        assert_eq!(port_suffix("https", 7300), ":7300");
    }
}
