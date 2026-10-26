//! Command line surface. Hand-rolled on purpose: six flags do not justify
//! pulling in an argument parser, and the workspace has no CLI dependency.

use std::path::PathBuf;

pub const DEFAULT_HUB: &str = "ws://127.0.0.1:7300";
pub const CONNECTOR_PATH: &str = "/api/v2/connector/ws";
pub const TOKEN_ENV: &str = "FARWORK_CONNECTOR_TOKEN";
pub const CODEX_BIN_ENV: &str = "FARWORK_CODEX_BIN";
pub const DEFAULT_CODEX_BIN: &str = "codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Codex,
    Echo,
}

impl RuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Echo => "echo",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "echo" => Some(Self::Echo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub hub: String,
    pub token: Option<String>,
    pub name: Option<String>,
    pub workdir: Option<PathBuf>,
    pub runtime: RuntimeKind,
    pub once: bool,
    pub codex_bin: String,
    /// Shell override for device terminals, instead of $SHELL.
    pub shell: Option<String>,
    pub show_help: bool,
    pub show_version: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            hub: String::from(DEFAULT_HUB),
            token: None,
            name: None,
            workdir: None,
            runtime: RuntimeKind::Codex,
            once: false,
            codex_bin: String::from(DEFAULT_CODEX_BIN),
            shell: None,
            show_help: false,
            show_version: false,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CliError {
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    #[error("missing value for {0}")]
    MissingValue(String),
    #[error("unknown runtime {0:?} (expected codex or echo)")]
    UnknownRuntime(String),
    #[error("{0} is set but empty")]
    EmptyEnv(&'static str),
}

pub const HELP: &str = "\
farwork-agent — outbound Farwork connector

USAGE:
    farwork-agent [OPTIONS]

OPTIONS:
    --hub <ws url>       Hub base URL (default: ws://127.0.0.1:7300)
    --token <token>      Connector token from POST /api/v2/connectors
                         (or env FARWORK_CONNECTOR_TOKEN)
    --name <name>        Device display name (default: host name)
    --workdir <path>     Default working directory for sessions
    --runtime <kind>     codex | echo (default: codex)
    --shell <path>       Shell for device terminals (default: $SHELL)
    --once               Connect once, then exit instead of reconnecting
    -h, --help           Print this help
    -V, --version        Print the version
";

/// Parse argv (excluding argv[0]) with an injected environment lookup so tests
/// do not depend on or mutate the process environment.
pub fn parse(
    args: impl IntoIterator<Item = String>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Cli, CliError> {
    let mut cli = Cli::default();
    let mut token_from_flag: Option<String> = None;
    let mut codex_bin: Option<String> = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| -> Result<String, CliError> {
            iter.next()
                .ok_or_else(|| CliError::MissingValue(flag.to_owned()))
        };
        match arg.as_str() {
            "--hub" => cli.hub = value("--hub")?,
            "--token" => token_from_flag = Some(value("--token")?),
            "--name" => cli.name = Some(value("--name")?),
            "--workdir" => cli.workdir = Some(PathBuf::from(value("--workdir")?)),
            "--runtime" => {
                let raw = value("--runtime")?;
                cli.runtime = RuntimeKind::parse(&raw).ok_or(CliError::UnknownRuntime(raw))?;
            }
            "--codex-bin" => codex_bin = Some(value("--codex-bin")?),
            "--shell" => cli.shell = Some(value("--shell")?),
            "--once" => cli.once = true,
            "-h" | "--help" => cli.show_help = true,
            "-V" | "--version" => cli.show_version = true,
            other => return Err(CliError::UnknownArgument(other.to_owned())),
        }
    }

    cli.token = match token_from_flag {
        Some(token) => Some(token),
        None => match env_lookup(TOKEN_ENV) {
            Some(raw) if raw.trim().is_empty() => return Err(CliError::EmptyEnv(TOKEN_ENV)),
            Some(raw) => Some(raw.trim().to_owned()),
            None => None,
        },
    };
    cli.codex_bin = match codex_bin {
        Some(bin) => bin,
        None => env_lookup(CODEX_BIN_ENV)
            .map(|raw| raw.trim().to_owned())
            .filter(|raw| !raw.is_empty())
            .unwrap_or_else(|| String::from(DEFAULT_CODEX_BIN)),
    };
    Ok(cli)
}

/// Device name fallback chain, no external crate required.
pub fn default_device_name(env_lookup: &dyn Fn(&str) -> Option<String>) -> String {
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Some(value) = env_lookup(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    if let Ok(raw) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    if let Ok(output) = std::process::Command::new("uname").arg("-n").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    String::from("farwork-agent")
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Build the connector ingress URL, percent-encoding the token.
pub fn connector_ws_url(hub: &str, token: &str) -> String {
    let base = hub.trim_end_matches('/');
    let mut url = String::from(base);
    if !url.contains(CONNECTOR_PATH) {
        url.push_str(CONNECTOR_PATH);
    }
    url.push_str("?token=");
    url.push_str(&percent_encode(token));
    url
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn defaults_match_the_documented_cli() {
        let cli = parse(Vec::new(), &no_env).unwrap();
        assert_eq!(cli.hub, "ws://127.0.0.1:7300");
        assert_eq!(cli.runtime, RuntimeKind::Codex);
        assert_eq!(cli.codex_bin, "codex");
        assert_eq!(cli.shell, None, "an absent --shell falls back to SHELL");
        assert!(!cli.once);
        assert_eq!(cli.token, None);
    }

    #[test]
    fn every_flag_is_recognised() {
        let cli = parse(
            args(&[
                "--hub",
                "ws://10.0.0.2:7300",
                "--token",
                "cxt_abc",
                "--name",
                "mbp",
                "--workdir",
                "/work",
                "--runtime",
                "echo",
                "--once",
                "--codex-bin",
                "/opt/codex",
                "--shell",
                "/bin/bash",
            ]),
            &no_env,
        )
        .unwrap();
        assert_eq!(cli.hub, "ws://10.0.0.2:7300");
        assert_eq!(cli.token.as_deref(), Some("cxt_abc"));
        assert_eq!(cli.name.as_deref(), Some("mbp"));
        assert_eq!(cli.workdir, Some(PathBuf::from("/work")));
        assert_eq!(cli.runtime, RuntimeKind::Echo);
        assert!(cli.once);
        assert_eq!(cli.codex_bin, "/opt/codex");
        assert_eq!(cli.shell.as_deref(), Some("/bin/bash"));
    }

    #[test]
    fn token_falls_back_to_environment() {
        let lookup = |key: &str| (key == TOKEN_ENV).then(|| String::from("  cxt_env  "));
        let cli = parse(Vec::new(), &lookup).unwrap();
        assert_eq!(cli.token.as_deref(), Some("cxt_env"));
    }

    #[test]
    fn flag_token_beats_environment() {
        let lookup = |key: &str| (key == TOKEN_ENV).then(|| String::from("cxt_env"));
        let cli = parse(args(&["--token", "cxt_flag"]), &lookup).unwrap();
        assert_eq!(cli.token.as_deref(), Some("cxt_flag"));
    }

    #[test]
    fn empty_token_env_is_a_loud_error() {
        let lookup = |key: &str| (key == TOKEN_ENV).then(String::new);
        assert_eq!(
            parse(Vec::new(), &lookup),
            Err(CliError::EmptyEnv(TOKEN_ENV))
        );
    }

    #[test]
    fn unknown_flag_and_missing_value_are_errors() {
        assert_eq!(
            parse(args(&["--nope"]), &no_env),
            Err(CliError::UnknownArgument(String::from("--nope")))
        );
        assert_eq!(
            parse(args(&["--hub"]), &no_env),
            Err(CliError::MissingValue(String::from("--hub")))
        );
    }

    #[test]
    fn unknown_runtime_is_rejected_instead_of_silently_defaulting() {
        assert_eq!(
            parse(args(&["--runtime", "python"]), &no_env),
            Err(CliError::UnknownRuntime(String::from("python")))
        );
    }

    #[test]
    fn help_and_version_are_flags_not_errors() {
        assert!(parse(args(&["--help"]), &no_env).unwrap().show_help);
        assert!(parse(args(&["-V"]), &no_env).unwrap().show_version);
    }

    #[test]
    fn connector_url_appends_path_and_query() {
        assert_eq!(
            connector_ws_url("ws://127.0.0.1:7300", "cxt_abc"),
            "ws://127.0.0.1:7300/api/v2/connector/ws?token=cxt_abc"
        );
        assert_eq!(
            connector_ws_url("ws://127.0.0.1:7300/", "cxt_abc"),
            "ws://127.0.0.1:7300/api/v2/connector/ws?token=cxt_abc"
        );
        assert_eq!(
            connector_ws_url("ws://h:1/api/v2/connector/ws", "cxt_abc"),
            "ws://h:1/api/v2/connector/ws?token=cxt_abc"
        );
    }

    #[test]
    fn connector_url_percent_encodes_reserved_token_characters() {
        assert_eq!(
            connector_ws_url("ws://h:1", "cxt_a+b/c=d"),
            "ws://h:1/api/v2/connector/ws?token=cxt_a%2Bb%2Fc%3Dd"
        );
        // cxt_ prefix + base64url never needs escaping, but '-' and '_' must survive.
        assert_eq!(
            connector_ws_url("ws://h:1", "cxt_a-b_c"),
            "ws://h:1/api/v2/connector/ws?token=cxt_a-b_c"
        );
    }

    #[test]
    fn device_name_prefers_hostname_env_then_falls_back() {
        let lookup = |key: &str| (key == "HOSTNAME").then(|| String::from("  mbp  "));
        assert_eq!(default_device_name(&lookup), "mbp");
        let empty = default_device_name(&no_env);
        assert!(!empty.trim().is_empty());
    }

    #[test]
    fn platform_is_one_of_the_contract_values() {
        assert!(["macos", "linux", "windows"].contains(&platform()));
    }
}
