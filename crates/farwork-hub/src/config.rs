//! Process configuration. Everything is env-driven so the same binary runs in
//! tests, locally, and in a container.

use std::env;
use std::path::PathBuf;

use crate::static_site::{INDEX_FILE, STATIC_DIR_ENV};

pub const DEFAULT_DATABASE_URL: &str = "postgres://tianjun@127.0.0.1:5432/farwork";
pub const DEFAULT_PORT: u16 = 7300;
/// Upstream default; overridable so a deployment does not silently share a key.
pub const DEFAULT_SECRET: &str = "agent-server-dev-secret";
pub const SETUP_TOKEN_TTL_SECONDS: i64 = 30 * 60;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub secret: String,
    /// Bootstrap token. When set (`FARWORK_HUB_SETUP_TOKEN`) the first-run
    /// admin registration accepts it verbatim; otherwise one is generated and
    /// logged, mirroring upstream's behaviour.
    pub setup_token: Option<String>,
    pub database_max_connections: u32,
    /// `FARWORK_HUB_PUBLIC_URL`: the address clients should dial, normalized to
    /// `scheme://authority` with no trailing slash. Only the operator knows the
    /// public name, port and TLS terminator in front of the hub, so this wins
    /// over anything derived from a request (see `routes::meta`).
    pub public_url: Option<String>,
    /// `FARWORK_HUB_STATIC_DIR`: the built SPA (`apps/web/build`) the hub serves
    /// itself, so one container can hold both the front end and the API. Unset
    /// means the static surface is not mounted at all — the pre-existing
    /// behaviour every test and `pnpm dev` depends on.
    pub static_dir: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
        let port = match env::var("FARWORK_HUB_PORT").or_else(|_| env::var("PORT")) {
            Ok(value) => value
                .parse::<u16>()
                .map_err(|_| ConfigError::InvalidPort(value))?,
            Err(_) => DEFAULT_PORT,
        };
        let secret = env::var("FARWORK_HUB_SECRET").unwrap_or_else(|_| {
            tracing::warn!("FARWORK_HUB_SECRET is not set; falling back to the development secret");
            DEFAULT_SECRET.to_string()
        });
        let setup_token = env::var("FARWORK_HUB_SETUP_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let database_max_connections = env::var("FARWORK_HUB_DB_POOL")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(10);
        let public_url = env::var("FARWORK_HUB_PUBLIC_URL")
            .ok()
            .map(|value| parse_public_url(&value))
            .transpose()?
            .flatten();
        let static_dir = match env::var(STATIC_DIR_ENV) {
            Ok(value) => parse_static_dir(&value)?,
            Err(_) => None,
        };
        Ok(Self {
            database_url,
            port,
            secret,
            setup_token,
            database_max_connections,
            public_url,
            static_dir,
        })
    }

    pub fn with_port_override(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Normalizes the advertised public URL: lowercase `http`/`https` scheme,
/// no trailing slash, empty value means "not set".
///
/// A malformed value fails startup on purpose. This string is copied verbatim
/// into every pairing command, so a typo must break loudly here instead of
/// handing users an address no agent can reach — which is the exact shape of
/// the bug `routes::meta` exists to prevent.
fn parse_public_url(raw: &str) -> Result<Option<String>, ConfigError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(None);
    }
    let invalid = || ConfigError::InvalidPublicUrl(raw.to_string());
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(invalid)?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https")
        || rest.is_empty()
        || rest.contains(char::is_whitespace)
    {
        return Err(invalid());
    }
    Ok(Some(format!("{scheme}://{rest}")))
}

/// Resolves `FARWORK_HUB_STATIC_DIR` to a directory that can actually serve the
/// SPA. A configured-but-broken value fails startup on purpose: a hub without a
/// front end answers `/` with `404`, which from the outside looks like a routing
/// bug while the deploy log happily says "listening". Empty value = "not set",
/// like every other variable here.
fn parse_static_dir(raw: &str) -> Result<Option<PathBuf>, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(trimmed);
    if !path.is_dir() {
        return Err(ConfigError::InvalidStaticDir(format!(
            "{trimmed:?} is not a directory"
        )));
    }
    // Without `index.html` the SPA fallback would answer every route with a 404:
    // a directory that looks configured but serves nothing.
    if !path.join(INDEX_FILE).is_file() {
        return Err(ConfigError::InvalidStaticDir(format!(
            "{trimmed:?} has no {INDEX_FILE} (build the SPA: cd apps/web && pnpm run build)"
        )));
    }
    Ok(Some(path))
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid port {0:?}")]
    InvalidPort(String),
    #[error(
        "invalid FARWORK_HUB_PUBLIC_URL {0:?}: expected http://host[:port] or https://host[:port]"
    )]
    InvalidPublicUrl(String),
    #[error("invalid FARWORK_HUB_STATIC_DIR: {0}")]
    InvalidStaticDir(String),
}

/// `--port <n>` or `--port=<n>`. `Ok(None)` means "no override given".
pub fn parse_port_arg(args: &[String]) -> Result<Option<u16>, ConfigError> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--port" {
            let value = iter
                .next()
                .ok_or_else(|| ConfigError::InvalidPort(String::from("<missing>")))?;
            return value
                .parse::<u16>()
                .map(Some)
                .map_err(|_| ConfigError::InvalidPort(value.clone()));
        }
        if let Some(value) = arg.strip_prefix("--port=") {
            return value
                .parse::<u16>()
                .map(Some)
                .map_err(|_| ConfigError::InvalidPort(value.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{parse_public_url, parse_static_dir};

    #[test]
    fn public_url_is_normalized_or_rejected() {
        assert_eq!(
            parse_public_url("https://hub.example.com/"),
            Ok(Some(String::from("https://hub.example.com")))
        );
        assert_eq!(
            parse_public_url("  HTTP://Farwork.example.com:8080//  "),
            Ok(Some(String::from("http://Farwork.example.com:8080")))
        );
        assert_eq!(parse_public_url("   "), Ok(None));
        assert!(parse_public_url("hub.example.com").is_err());
        assert!(parse_public_url("ftp://hub.example.com").is_err());
        assert!(parse_public_url("https://").is_err());
        assert!(parse_public_url("https://hub example.com").is_err());
    }

    /// An empty value means "not set"; anything else must name a directory that
    /// can actually answer `/`, because a start-up that "succeeds" without a
    /// front end is the failure mode this validation exists to prevent.
    #[test]
    fn static_dir_is_optional_but_validated() {
        assert_eq!(parse_static_dir(""), Ok(None));
        assert_eq!(parse_static_dir("   "), Ok(None));

        let root = std::env::temp_dir().join(format!("farwork-static-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture directory");

        // A directory without index.html cannot serve the SPA shell.
        let missing_index = parse_static_dir(&root.to_string_lossy()).expect_err("no index.html");
        assert!(
            missing_index.to_string().contains("has no index.html"),
            "unexpected message: {missing_index}"
        );

        std::fs::write(root.join("index.html"), "<!doctype html>").expect("write index.html");
        assert_eq!(
            parse_static_dir(&format!("  {}  ", root.to_string_lossy())),
            Ok(Some(root.clone())),
            "surrounding whitespace is trimmed, like every other variable here"
        );

        let missing_dir = parse_static_dir("/nonexistent/farwork-static-dir");
        assert!(missing_dir
            .expect_err("a missing directory must fail startup")
            .to_string()
            .contains("is not a directory"),);

        std::fs::remove_dir_all(&root).ok();
    }
}
