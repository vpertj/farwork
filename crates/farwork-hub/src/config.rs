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
        Ok(Self {
            database_url,
            port,
            secret,
            setup_token,
            database_max_connections,
        })
    }

    pub fn with_port_override(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid port {0:?}")]
    InvalidPort(String),
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
