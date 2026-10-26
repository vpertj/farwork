//! Locating the DSH bridge endpoint.
//!
//! The bridge publishes one small file with a loopback host, a port and a bearer
//! token. Every field is validated: a file naming another host, a port outside
//! 1..=65535, an empty token or an unknown version is refused rather than dialled
//! "best effort". A missing endpoint is a startup error, never a silent
//! "runtime unavailable" that would look like a working DSH session that never
//! answers.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `$HOME/.agents-anywhere/dsh-bridge/endpoint.json`.
pub const PRIMARY_SUFFIX: &str = ".agents-anywhere/dsh-bridge/endpoint.json";
/// `$DSH_HOME/agents-anywhere/bridge/endpoint.json`.
pub const FALLBACK_SUFFIX: &str = "agents-anywhere/bridge/endpoint.json";
pub const DSH_HOME_ENV: &str = "DSH_HOME";
/// The only layout this agent understands. A newer version means a different
/// file shape, so guessing at it is how a stale reader silently mis-dials.
pub const ENDPOINT_VERSION: u32 = 1;
pub const LOOPBACK: &str = "127.0.0.1";

#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub pid: Option<u64>,
}

impl Endpoint {
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// The token is a credential: never let it reach a log line through `{:?}`.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("pid", &self.pid)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("no DSH bridge endpoint found (looked for {tried}); start DSH with the agents-anywhere bridge enabled, or pass --dsh-endpoint <path>")]
    NotFound { tried: String },
    #[error("cannot read DSH bridge endpoint {path}: {message}")]
    Unreadable { path: String, message: String },
    #[error("invalid DSH bridge endpoint {path}: {reason}")]
    Invalid { path: String, reason: String },
}

#[derive(Debug, Deserialize)]
struct RawEndpoint {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u32>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    pid: Option<u64>,
}

/// Both published locations, in preference order. Parameterized so tests never
/// have to touch the process environment or the real home directory.
pub fn candidate_paths(home: Option<&Path>, dsh_home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home {
        paths.push(home.join(PRIMARY_SUFFIX));
    }
    if let Some(dsh_home) = dsh_home {
        paths.push(dsh_home.join(FALLBACK_SUFFIX));
    }
    paths
}

/// Locate and validate the endpoint. `explicit` (from `--dsh-endpoint`) wins and
/// is used alone: an operator who names a file must not silently get a different
/// bridge from the default search path.
pub fn resolve(
    explicit: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Endpoint, DiscoveryError> {
    if let Some(path) = explicit {
        return read(path);
    }
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from);
    let dsh_home = env(DSH_HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".dsh")));
    let candidates = candidate_paths(home.as_deref(), dsh_home.as_deref());
    for path in &candidates {
        if path.is_file() {
            return read(path);
        }
    }
    Err(DiscoveryError::NotFound {
        tried: candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

fn read(path: &Path) -> Result<Endpoint, DiscoveryError> {
    let shown = path.display().to_string();
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(DiscoveryError::NotFound { tried: shown })
        }
        Err(error) => {
            return Err(DiscoveryError::Unreadable {
                path: shown,
                message: error.to_string(),
            })
        }
    };
    parse(&raw, &shown)
}

/// Parse and validate one endpoint document. Kept separate from IO so the
/// refusal rules are directly testable.
pub fn parse(raw: &str, shown: &str) -> Result<Endpoint, DiscoveryError> {
    let invalid = |reason: String| DiscoveryError::Invalid {
        path: shown.to_string(),
        reason,
    };
    let parsed: RawEndpoint = serde_json::from_str(raw)
        .map_err(|error| invalid(format!("not a JSON object: {error}")))?;
    match parsed.version {
        Some(version) if version == ENDPOINT_VERSION => {}
        Some(version) => {
            return Err(invalid(format!(
                "unsupported version {version} (expected {ENDPOINT_VERSION})"
            )))
        }
        None => return Err(invalid(String::from("missing version"))),
    }
    let host = parsed
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| invalid(String::from("missing host")))?;
    if host != LOOPBACK {
        return Err(invalid(format!(
            "host must be {LOOPBACK}, got {host:?}: the bridge is a loopback endpoint and this agent will not dial anything else"
        )));
    }
    let port = parsed
        .port
        .ok_or_else(|| invalid(String::from("missing port")))?;
    if port == 0 || port > u16::MAX as u32 {
        return Err(invalid(format!("port must be 1..=65535, got {port}")));
    }
    let token = parsed
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| invalid(String::from("missing token")))?;
    Ok(Endpoint {
        host: String::from(host),
        port: port as u16,
        token: token.to_string(),
        pid: parsed.pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint_json(host: &str, port: u32, token: &str) -> String {
        format!(r#"{{"version":1,"host":{host:?},"port":{port},"token":{token:?},"pid":42}}"#)
    }

    #[test]
    fn a_valid_endpoint_parses_and_its_token_never_reaches_debug_output() {
        let endpoint =
            parse(&endpoint_json("127.0.0.1", 62082, "secret-token"), "ep").expect("parse");
        assert_eq!(endpoint.address(), "127.0.0.1:62082");
        assert_eq!(endpoint.token, "secret-token");
        assert_eq!(endpoint.pid, Some(42));
        let shown = format!("{endpoint:?}");
        assert!(
            !shown.contains("secret-token"),
            "debug leaked the token: {shown}"
        );
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn a_non_loopback_host_is_refused_instead_of_dialled() {
        let error = parse(&endpoint_json("10.0.0.5", 62082, "t"), "ep").unwrap_err();
        assert!(matches!(error, DiscoveryError::Invalid { .. }));
        assert!(error.to_string().contains("host must be 127.0.0.1"));
    }

    #[test]
    fn port_and_token_and_version_are_validated() {
        for (raw, needle) in [
            (endpoint_json("127.0.0.1", 0, "t"), "port must be"),
            (endpoint_json("127.0.0.1", 70000, "t"), "port must be"),
            (endpoint_json("127.0.0.1", 62082, ""), "missing token"),
            (endpoint_json("127.0.0.1", 62082, "   "), "missing token"),
            (
                r#"{"host":"127.0.0.1","port":1,"token":"t"}"#.to_string(),
                "missing version",
            ),
            (
                r#"{"version":2,"host":"127.0.0.1","port":1,"token":"t"}"#.to_string(),
                "unsupported version",
            ),
            (String::from("not json"), "not a JSON object"),
        ] {
            let error = parse(&raw, "ep").unwrap_err();
            assert!(
                error.to_string().contains(needle),
                "expected {needle:?} in {error}"
            );
        }
    }

    #[test]
    fn an_explicit_path_is_used_alone() {
        let root =
            std::env::temp_dir().join(format!("farwork-dsh-ep-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let explicit = root.join("elsewhere.json");
        std::fs::write(&explicit, endpoint_json("127.0.0.1", 6111, "explicit")).expect("write");
        let env = |key: &str| (key == "HOME").then(|| root.display().to_string());
        let endpoint = resolve(Some(&explicit), &env).expect("explicit");
        assert_eq!(endpoint.port, 6111);
        // A missing explicit path is reported, never back-filled from the defaults.
        let missing = root.join("nope.json");
        let error = resolve(Some(&missing), &env).unwrap_err();
        assert!(matches!(error, DiscoveryError::NotFound { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_documented_locations_are_searched_in_order_and_the_fallback_works() {
        let root =
            std::env::temp_dir().join(format!("farwork-dsh-fb-{}", uuid::Uuid::new_v4().simple()));
        let home = root.join("home");
        let dsh_home = root.join("dsh");
        std::fs::create_dir_all(dsh_home.join("agents-anywhere/bridge")).expect("mkdir");
        std::fs::write(
            dsh_home.join("agents-anywhere/bridge/endpoint.json"),
            endpoint_json("127.0.0.1", 6222, "fallback"),
        )
        .expect("write");
        let env = |key: &str| match key {
            "HOME" => Some(home.display().to_string()),
            "DSH_HOME" => Some(dsh_home.display().to_string()),
            _ => None,
        };
        let endpoint = resolve(None, &env).expect("fallback");
        assert_eq!(endpoint.port, 6222);

        // The primary location wins when both exist.
        std::fs::create_dir_all(home.join(PRIMARY_SUFFIX).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(
            home.join(PRIMARY_SUFFIX),
            endpoint_json("127.0.0.1", 6333, "primary"),
        )
        .expect("write");
        let endpoint = resolve(None, &env).expect("primary");
        assert_eq!(endpoint.port, 6333);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_endpoint_anywhere_is_a_loud_error_listing_what_was_tried() {
        let root = std::env::temp_dir().join(format!(
            "farwork-dsh-none-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let env = |key: &str| (key == "HOME").then(|| root.join("home").display().to_string());
        let error = resolve(None, &env).unwrap_err();
        assert!(matches!(error, DiscoveryError::NotFound { .. }));
        assert!(error.to_string().contains("dsh-bridge"));
        assert!(error.to_string().contains("--dsh-endpoint"));
    }
}
