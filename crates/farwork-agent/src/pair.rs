//! Redeeming a pairing code for a connector token.
//!
//! The web UI mints a short one-shot code (`POST /api/v2/pairing/start`) and the
//! agent trades it for a connector token (`POST /api/v2/pairing/claim`, no
//! auth). This module owns the agent side of that exchange: turning a hub URL
//! into a REST URL, and turning every failure into something the operator can
//! act on -- "it failed" is useless when the fix differs per status code.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cli::CONNECTOR_PATH;

pub const CLAIM_PATH: &str = "/api/v2/pairing/claim";
pub const CLAIM_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    pub connector_id: String,
    pub token: String,
    /// The name the hub echoed back, i.e. the connector it created.
    pub name: String,
    /// `true` when this device already had a connector and the hub rotated its
    /// token instead of registering a second one. Only the hub can tell the two
    /// apart, so the operator has to be told which happened.
    pub reused: bool,
}

/// Every variant names the next action a human should take.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    #[error("hub URL {0:?} is neither http(s):// nor ws(s)://: pass e.g. --hub https://farwork.example.com")]
    UnsupportedHub(String),
    #[error("pairing code {code} is not valid for this hub (404 invalid_code): open the web UI and generate a new code")]
    InvalidCode { code: String },
    #[error("pairing code {code} was already claimed (409 code_already_claimed): another device may have claimed it, so open the web UI and generate a new code")]
    AlreadyClaimed { code: String },
    #[error(
        "pairing code {code} expired (410 code_expired): open the web UI and generate a new code"
    )]
    Expired { code: String },
    #[error("too many pairing attempts from this address (429 too_many_attempts, at most 10 per minute): wait a minute and try again")]
    TooManyAttempts,
    #[error("hub at {url} has no pairing API (404 without invalid_code): that hub predates pairing -- upgrade it, or pass --token instead")]
    EndpointMissing { url: String },
    #[error("hub answered {status} from {url}{detail}")]
    UnexpectedStatus {
        url: String,
        status: u16,
        detail: String,
    },
    #[error("hub at {url} answered 200 without a connectorToken: that endpoint is not the pairing API this agent expects")]
    MalformedClaim { url: String },
    #[error("cannot reach the hub at {url}: {reason}")]
    Unreachable { url: String, reason: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaimRequest<'a> {
    code: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<&'a str>,
    /// Always sent: it is the only thing that lets the hub recognise "this machine
    /// again", and a claim without it creates a duplicate row. A hub that predates
    /// the key ignores it, so sending it unconditionally stays compatible.
    agent_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    #[serde(default)]
    connector: Option<ClaimedConnector>,
    #[serde(default, rename = "connectorToken")]
    connector_token: Option<String>,
    /// Absent from a hub that cannot reuse, which is "not reported": the agent
    /// then says "paired" instead of claiming something it does not know.
    #[serde(default)]
    reused: bool,
}

#[derive(Debug, Deserialize)]
struct ClaimedConnector {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// The REST base for a hub given in any of the four accepted forms.
///
/// A hub URL may already carry the connector ingress path (that is what the docs
/// hand out for `--hub`), so it is stripped before the REST path is added.
pub fn rest_base(hub: &str) -> Result<String, PairError> {
    let trimmed = hub.trim().trim_end_matches('/');
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| PairError::UnsupportedHub(hub.to_owned()))?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => "http",
        "https" | "wss" => "https",
        _ => return Err(PairError::UnsupportedHub(hub.to_owned())),
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(CONNECTOR_PATH).unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err(PairError::UnsupportedHub(hub.to_owned()));
    }
    Ok(format!("{scheme}://{rest}"))
}

pub fn claim_url(hub: &str) -> Result<String, PairError> {
    Ok(format!("{}{CLAIM_PATH}", rest_base(hub)?))
}

/// Status + hub detail code -> the action it implies.
///
/// The status alone is not enough: a 404 is `invalid_code` from the pairing
/// route but plain "no such route" from a hub that never had pairing, and those
/// two need opposite advice.
pub fn map_failure(status: u16, detail_code: Option<&str>, code: &str, url: &str) -> PairError {
    let unexpected = |detail: String| PairError::UnexpectedStatus {
        url: url.to_owned(),
        status,
        detail,
    };
    match (status, detail_code) {
        (404, Some("invalid_code")) => PairError::InvalidCode {
            code: code.to_owned(),
        },
        (404, None) => PairError::EndpointMissing {
            url: url.to_owned(),
        },
        (404, Some(other)) => unexpected(format!(" (detail: {other})")),
        (409, _) => PairError::AlreadyClaimed {
            code: code.to_owned(),
        },
        (410, _) => PairError::Expired {
            code: code.to_owned(),
        },
        (429, _) => PairError::TooManyAttempts,
        _ => unexpected(String::new()),
    }
}

/// Trade a pairing code for a connector token.
///
/// `agent_id` is this device's stable identity (`agent.json`): the hub uses it to
/// hand back the connector the same machine already has instead of creating a
/// second one.
pub async fn claim(
    hub: &str,
    code: &str,
    name: &str,
    platform: &str,
    agent_id: &str,
) -> Result<Claimed, PairError> {
    let url = claim_url(hub)?;
    let client = reqwest::Client::builder()
        .timeout(CLAIM_TIMEOUT)
        .build()
        .map_err(|error| PairError::Unreachable {
            url: url.clone(),
            reason: error.to_string(),
        })?;
    let body = ClaimRequest {
        code,
        name,
        platform: Some(platform),
        agent_id,
    };
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|error| PairError::Unreachable {
            url: url.clone(),
            reason: describe_transport(&error),
        })?;

    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if status >= 400 {
        return Err(map_failure(
            status,
            detail_code(&text).as_deref(),
            code,
            &url,
        ));
    }

    let ClaimResponse {
        connector,
        connector_token,
        reused,
    } = serde_json::from_str(&text).map_err(|_| PairError::MalformedClaim { url: url.clone() })?;
    let token = connector_token
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PairError::MalformedClaim { url: url.clone() })?;
    let connector_id = connector
        .as_ref()
        .and_then(|connector| connector.id.clone())
        .unwrap_or_default();
    let name = connector
        .and_then(|connector| connector.name)
        .unwrap_or_default();
    Ok(Claimed {
        connector_id,
        token,
        name,
        reused,
    })
}

/// reqwest hides the useful part (refused / timed out / DNS) below a generic
/// wrapper; without it "cannot reach the hub" would not say why.
fn describe_transport(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return format!("timed out after {}s", CLAIM_TIMEOUT.as_secs());
    }
    if error.is_connect() {
        return String::from("connection refused or DNS lookup failed");
    }
    error.to_string()
}

/// The hub reports failures as {"detail":{"code":...,"message":...}}.
fn detail_code(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        detail: Option<Detail>,
    }
    #[derive(Deserialize)]
    struct Detail {
        code: Option<String>,
    }
    serde_json::from_str::<Envelope>(body).ok()?.detail?.code
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-shot HTTP stub: answers the first request with `status` + `body` and
    /// hands back the raw request text so the test can inspect it.
    async fn spawn_stub(status: u16, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_owned();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let declared = text[..head_end]
                        .to_ascii_lowercase()
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if request.len() >= head_end + 4 + declared {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            String::from_utf8_lossy(&request).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn rest_urls_accept_http_ws_https_wss_and_a_connector_path() {
        for (hub, expected) in [
            (
                "http://127.0.0.1:7300",
                "http://127.0.0.1:7300/api/v2/pairing/claim",
            ),
            (
                "http://127.0.0.1:7300/",
                "http://127.0.0.1:7300/api/v2/pairing/claim",
            ),
            (
                "ws://127.0.0.1:7300",
                "http://127.0.0.1:7300/api/v2/pairing/claim",
            ),
            (
                "wss://farwork.example.com",
                "https://farwork.example.com/api/v2/pairing/claim",
            ),
            (
                "https://farwork.example.com/",
                "https://farwork.example.com/api/v2/pairing/claim",
            ),
            (
                "wss://farwork.example.com/api/v2/connector/ws",
                "https://farwork.example.com/api/v2/pairing/claim",
            ),
            (
                "https://farwork.example.com/fw",
                "https://farwork.example.com/fw/api/v2/pairing/claim",
            ),
        ] {
            assert_eq!(claim_url(hub).unwrap(), expected, "hub {hub}");
        }
    }

    #[test]
    fn a_hub_without_a_scheme_is_rejected_with_a_shaped_example() {
        for hub in ["farwork.example.com", "ftp://h:1", "https://", "://h:1"] {
            let error = claim_url(hub).unwrap_err();
            assert_eq!(error, PairError::UnsupportedHub(hub.to_owned()));
            assert!(error.to_string().contains("--hub https://"), "{error}");
        }
    }

    #[test]
    fn claim_statuses_map_to_the_action_they_imply() {
        let url = "https://h:1/api/v2/pairing/claim";
        assert_eq!(
            map_failure(404, Some("invalid_code"), "4F2K-9QXM", url),
            PairError::InvalidCode {
                code: String::from("4F2K-9QXM")
            }
        );
        assert_eq!(
            map_failure(409, Some("code_already_claimed"), "4F2K-9QXM", url),
            PairError::AlreadyClaimed {
                code: String::from("4F2K-9QXM")
            }
        );
        assert_eq!(
            map_failure(410, Some("code_expired"), "4F2K-9QXM", url),
            PairError::Expired {
                code: String::from("4F2K-9QXM")
            }
        );
        assert_eq!(
            map_failure(429, Some("too_many_attempts"), "4F2K-9QXM", url),
            PairError::TooManyAttempts
        );
        // A bare 404 is a missing route, not a bad code, and the advice differs.
        assert_eq!(
            map_failure(404, None, "4F2K-9QXM", url),
            PairError::EndpointMissing {
                url: String::from(url)
            }
        );
        assert!(matches!(
            map_failure(500, None, "4F2K-9QXM", url),
            PairError::UnexpectedStatus { status: 500, .. }
        ));
    }

    #[test]
    fn every_failure_message_names_the_next_step() {
        let expired = PairError::Expired {
            code: String::from("4F2K-9QXM"),
        }
        .to_string();
        assert!(expired.contains("expired"), "{expired}");
        assert!(expired.contains("web UI"), "{expired}");
        assert!(expired.contains("new code"), "{expired}");

        let claimed = PairError::AlreadyClaimed {
            code: String::from("4F2K-9QXM"),
        }
        .to_string();
        assert!(claimed.contains("already claimed"), "{claimed}");
        assert!(claimed.contains("another device"), "{claimed}");

        let invalid = PairError::InvalidCode {
            code: String::from("4F2K-9QXM"),
        }
        .to_string();
        assert!(invalid.contains("not valid"), "{invalid}");
        assert!(invalid.contains("web UI"), "{invalid}");

        let throttled = PairError::TooManyAttempts.to_string();
        assert!(throttled.contains("wait a minute"), "{throttled}");
    }

    #[tokio::test]
    async fn a_claim_returns_the_connector_token_and_sends_the_contract_body() {
        let body = r#"{"connector":{"id":"cn_1","name":"\u6211\u7684 Mac","deviceOs":"macos","status":"online"},"connectorToken":"cxt_stub","serverTime":"2026-01-01T00:00:00Z"}"#;
        let (hub, handle) = spawn_stub(200, body).await;
        let claimed = claim(&hub, "4F2K-9QXM", "我的 Mac", "macos", "agt_stub")
            .await
            .unwrap();
        assert_eq!(claimed.connector_id, "cn_1");
        assert_eq!(claimed.token, "cxt_stub");
        assert_eq!(claimed.name, "我的 Mac");
        // A hub that does not report the key has not reused anything.
        assert!(!claimed.reused);

        let request = handle.await.unwrap();
        assert!(
            request.starts_with("POST /api/v2/pairing/claim "),
            "{request}"
        );
        assert!(request.contains("\"code\":\"4F2K-9QXM\""), "{request}");
        assert!(request.contains("\"platform\":\"macos\""), "{request}");
        // The device identity is what stops a second pair from creating a second
        // connector, so it is not optional on this side.
        assert!(request.contains("\"agentId\":\"agt_stub\""), "{request}");
        assert!(
            request.contains("connectorToken") || request.contains("name"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_reused_claim_is_reported_as_a_reuse() {
        let body = r#"{"connector":{"id":"cn_9","name":"mbp","deviceOs":"macos","status":"offline"},"connectorToken":"cxt_rotated","reused":true,"serverTime":"2026-01-01T00:00:00Z"}"#;
        let (hub, _handle) = spawn_stub(200, body).await;
        let claimed = claim(&hub, "4F2K-9QXM", "mbp", "macos", "agt_stub")
            .await
            .unwrap();
        assert_eq!(claimed.connector_id, "cn_9");
        assert_eq!(claimed.token, "cxt_rotated");
        assert!(
            claimed.reused,
            "the operator must be able to tell a reuse from a fresh pairing"
        );
    }

    #[tokio::test]
    async fn claim_failures_reach_the_caller_as_their_own_variant() {
        for (status, detail, expected) in [
            (404u16, "invalid_code", "InvalidCode"),
            (409, "code_already_claimed", "AlreadyClaimed"),
            (410, "code_expired", "Expired"),
            (429, "too_many_attempts", "TooManyAttempts"),
        ] {
            let body = format!(r#"{{"detail":{{"code":"{detail}","message":"x"}}}}"#);
            let (hub, _handle) = spawn_stub(status, &body).await;
            let error = claim(&hub, "4F2K-9QXM", "mbp", "macos", "agt_stub")
                .await
                .unwrap_err();
            let shown = format!("{error:?}");
            assert!(shown.contains(expected), "{status}: {shown}");
            // A rejection never carries the token: the hub did not send one.
            assert!(!error.to_string().contains("cxt_"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_200_without_a_connector_token_is_refused() {
        let (hub, _handle) = spawn_stub(200, r#"{"connector":{"id":"cn_1"}}"#).await;
        let error = claim(&hub, "4F2K-9QXM", "mbp", "macos", "agt_stub")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            PairError::MalformedClaim {
                url: claim_url(&hub).unwrap()
            }
        );
    }

    #[tokio::test]
    async fn an_unreachable_hub_reports_the_url_it_tried() {
        // Bind then drop, so the port is almost certainly free and refuses.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let hub = format!("http://{addr}");
        let error = claim(&hub, "4F2K-9QXM", "mbp", "macos", "agt_stub")
            .await
            .unwrap_err();
        let shown = error.to_string();
        assert!(
            shown.contains(&format!("{addr}/api/v2/pairing/claim")),
            "{shown}"
        );
        assert!(shown.contains("cannot reach the hub"), "{shown}");
    }
}
