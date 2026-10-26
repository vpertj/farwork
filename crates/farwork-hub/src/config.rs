//! Process configuration. Everything is env-driven so the same binary runs in
//! tests, locally, and in a container.

use std::env;

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
        Ok(Self {
            database_url,
            port,
            secret,
            setup_token,
            database_max_connections,
            public_url,
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

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid port {0:?}")]
    InvalidPort(String),
    #[error(
        "invalid FARWORK_HUB_PUBLIC_URL {0:?}: expected http://host[:port] or https://host[:port]"
    )]
    InvalidPublicUrl(String),
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
    use super::parse_public_url;

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
}
