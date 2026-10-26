//! Command line surface. Hand-rolled on purpose: a dozen flags do not justify
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
    /// The DeepSeek Harness bridge: a loopback JSON-RPC endpoint this agent
    /// attaches to instead of spawning a child process.
    Dsh,
}

impl RuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Echo => "echo",
            Self::Dsh => "dsh",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "echo" => Some(Self::Echo),
            "dsh" => Some(Self::Dsh),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub hub: String,
    /// Whether --hub was actually passed. The default below is a dev fallback,
    /// so "not given" must stay distinguishable from "given as the default"
    /// once agent.json can supply a hub.
    pub hub_explicit: bool,
    /// The `pair` subcommand: redeem a pairing code into agent.json.
    pub pair: bool,
    /// --code, the one-shot pairing code; only meaningful with `pair`.
    pub code: Option<String>,
    pub token: Option<String>,
    pub name: Option<String>,
    pub workdir: Option<PathBuf>,
    pub runtime: RuntimeKind,
    pub once: bool,
    pub codex_bin: String,
    /// `--dsh-endpoint`: an explicit bridge endpoint file, instead of the one the
    /// bridge publishes.
    pub dsh_endpoint: Option<PathBuf>,
    /// `--dsh-permission`: a permission selection ID or a short name resolved
    /// through `catalog.listPermissions`, applied to the first `createAndStart`.
    pub dsh_permission: Option<String>,
    /// `--dsh-model`: the same, resolved through `catalog.listModels`.
    pub dsh_model: Option<String>,
    /// `--dsh-preset`: the Agent mode, resolved through
    /// `catalog.listAgentPresets` and sent as `createAndStart.agentPreset`.
    pub dsh_preset: Option<String>,
    /// Shell override for device terminals, instead of $SHELL.
    pub shell: Option<String>,
    pub show_help: bool,
    pub show_version: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            hub: String::from(DEFAULT_HUB),
            hub_explicit: false,
            pair: false,
            code: None,
            token: None,
            name: None,
            workdir: None,
            runtime: RuntimeKind::Codex,
            once: false,
            codex_bin: String::from(DEFAULT_CODEX_BIN),
            dsh_endpoint: None,
            dsh_permission: None,
            dsh_model: None,
            dsh_preset: None,
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
    #[error("unknown runtime {0:?} (expected codex, echo or dsh)")]
    UnknownRuntime(String),
    #[error("{0} is set but empty")]
    EmptyEnv(&'static str),
    #[error("{flag} only applies to --runtime dsh, but the runtime is {runtime}")]
    SelectionWithoutDsh {
        flag: &'static str,
        runtime: &'static str,
    },
    #[error("--code only applies to the \"pair\" subcommand: farwork-agent pair --hub <url> --code <code>")]
    CodeOutsidePair,
    #[error("pair needs the hub to redeem against: farwork-agent pair --hub <url> --code <code>")]
    PairMissingHub,
    #[error("pair needs --code <code> (the pairing code shown in the web UI)")]
    PairMissingCode,
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
    --runtime <kind>     codex | echo | dsh (default: codex)
    --dsh-endpoint <p>   DSH bridge endpoint.json (default: the published one)
    --dsh-permission <v> DSH permission for new sessions: a selection id or a
                         catalog short name, e.g. read-only (dsh only)
    --dsh-model <v>      DSH model for new sessions: a selection id or a
                         catalog short name (dsh only)
    --dsh-preset <v>     DSH Agent mode for new sessions: a preset id or a
                         catalog name, e.g. ptc (dsh only)
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
    let mut iter = args.into_iter().peekable();

    // A leading bare word is the subcommand; anything else stays a flag, so the
    // existing no-subcommand invocation keeps working verbatim.
    if iter.peek().map(String::as_str) == Some("pair") {
        cli.pair = true;
        iter.next();
    }

    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| -> Result<String, CliError> {
            iter.next()
                .ok_or_else(|| CliError::MissingValue(flag.to_owned()))
        };
        match arg.as_str() {
            "--hub" => {
                cli.hub = value("--hub")?;
                cli.hub_explicit = true;
            }
            "--code" => cli.code = Some(value("--code")?),
            "--token" => token_from_flag = Some(value("--token")?),
            "--name" => cli.name = Some(value("--name")?),
            "--workdir" => cli.workdir = Some(PathBuf::from(value("--workdir")?)),
            "--runtime" => {
                let raw = value("--runtime")?;
                cli.runtime = RuntimeKind::parse(&raw).ok_or(CliError::UnknownRuntime(raw))?;
            }
            "--codex-bin" => codex_bin = Some(value("--codex-bin")?),
            "--dsh-endpoint" => cli.dsh_endpoint = Some(PathBuf::from(value("--dsh-endpoint")?)),
            "--dsh-permission" => cli.dsh_permission = Some(value("--dsh-permission")?),
            "--dsh-model" => cli.dsh_model = Some(value("--dsh-model")?),
            "--dsh-preset" => cli.dsh_preset = Some(value("--dsh-preset")?),
            "--shell" => cli.shell = Some(value("--shell")?),
            "--once" => cli.once = true,
            "-h" | "--help" => cli.show_help = true,
            "-V" | "--version" => cli.show_version = true,
            other => return Err(CliError::UnknownArgument(other.to_owned())),
        }
    }

    // A selection only ever reaches a DSH session. Accepting it under codex or
    // echo would look like it was applied while changing nothing.
    let selection_flag = [
        ("--dsh-permission", cli.dsh_permission.is_some()),
        ("--dsh-model", cli.dsh_model.is_some()),
        ("--dsh-preset", cli.dsh_preset.is_some()),
    ]
    .into_iter()
    .find_map(|(flag, set)| set.then_some(flag));
    if let (Some(flag), false) = (selection_flag, cli.runtime == RuntimeKind::Dsh) {
        return Err(CliError::SelectionWithoutDsh {
            flag,
            runtime: cli.runtime.as_str(),
        });
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

    match (cli.pair, cli.code.take()) {
        (true, Some(code)) => {
            // Crockford's alphabet is case-insensitive by design, so a pasted
            // lower-case code is the same code rather than a typo.
            let code = code.trim().to_ascii_uppercase();
            if code.is_empty() {
                return Err(CliError::PairMissingCode);
            }
            cli.code = Some(code);
        }
        (true, None) => return Err(CliError::PairMissingCode),
        (false, Some(_)) => return Err(CliError::CodeOutsidePair),
        (false, None) => {}
    }
    // The default hub is a localhost dev fallback: redeeming a code against it
    // would fail in a way that looks like a bad code. Demand an explicit hub.
    if cli.pair && !cli.hub_explicit {
        return Err(CliError::PairMissingHub);
    }
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
///
/// The hub may be given as the http(s) URL that pairing and the web UI use;
/// the connector always dials the ws(s) form of the same origin.
pub fn connector_ws_url(hub: &str, token: &str) -> String {
    let base = hub.trim().trim_end_matches('/');
    let base = match base.strip_prefix("https://") {
        Some(rest) => format!("wss://{rest}"),
        None => match base.strip_prefix("http://") {
            Some(rest) => format!("ws://{rest}"),
            None => String::from(base),
        },
    };
    let base = base.as_str();
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
    fn the_dsh_runtime_and_its_endpoint_flag_are_recognised() {
        let cli = parse(
            args(&["--runtime", "dsh", "--dsh-endpoint", "/tmp/endpoint.json"]),
            &no_env,
        )
        .unwrap();
        assert_eq!(cli.runtime, RuntimeKind::Dsh);
        assert_eq!(cli.runtime.as_str(), "dsh");
        assert_eq!(cli.dsh_endpoint, Some(PathBuf::from("/tmp/endpoint.json")));
        // codex stays the default: dsh is opt-in.
        assert_eq!(
            parse(Vec::new(), &no_env).unwrap().runtime,
            RuntimeKind::Codex
        );
    }

    #[test]
    fn dsh_selection_flags_parse_for_the_dsh_runtime() {
        let cli = parse(
            args(&[
                "--runtime",
                "dsh",
                "--dsh-permission",
                "read-only",
                "--dsh-model",
                "DeepSeek-V4-Pro",
                "--dsh-preset",
                "ptc",
            ]),
            &no_env,
        )
        .unwrap();
        assert_eq!(cli.dsh_permission.as_deref(), Some("read-only"));
        assert_eq!(cli.dsh_model.as_deref(), Some("DeepSeek-V4-Pro"));
        assert_eq!(cli.dsh_preset.as_deref(), Some("ptc"));
        // Absent by default: nothing is overridden unless the operator asks.
        let bare = parse(args(&["--runtime", "dsh"]), &no_env).unwrap();
        assert_eq!(bare.dsh_permission, None);
        assert_eq!(bare.dsh_model, None);
        assert_eq!(bare.dsh_preset, None);
    }

    #[test]
    fn a_dsh_selection_outside_the_dsh_runtime_is_a_loud_error() {
        for (runtime, flag) in [
            ("codex", "--dsh-permission"),
            ("echo", "--dsh-model"),
            ("codex", "--dsh-preset"),
        ] {
            let error =
                parse(args(&["--runtime", runtime, flag, "read-only"]), &no_env).unwrap_err();
            assert_eq!(
                error,
                CliError::SelectionWithoutDsh {
                    flag,
                    runtime: if runtime == "codex" { "codex" } else { "echo" },
                }
            );
            let shown = error.to_string();
            assert!(shown.contains(flag), "{shown}");
            assert!(shown.contains(runtime), "{shown}");
            assert!(shown.contains("--runtime dsh"), "{shown}");
        }
        // The default runtime is codex, so the flag alone must fail too.
        assert!(matches!(
            parse(args(&["--dsh-permission", "read-only"]), &no_env),
            Err(CliError::SelectionWithoutDsh { .. })
        ));
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
    fn the_pair_subcommand_takes_a_hub_and_a_code() {
        let cli = parse(
            args(&[
                "pair",
                "--hub",
                "https://hub.example.com",
                "--code",
                "4f2k-9qxm",
            ]),
            &no_env,
        )
        .unwrap();
        assert!(cli.pair);
        assert!(cli.hub_explicit);
        assert_eq!(cli.hub, "https://hub.example.com");
        // Crockford codes are case-insensitive: a pasted lower-case code is the
        // same code, and the hub is not asked to guess.
        assert_eq!(cli.code.as_deref(), Some("4F2K-9QXM"));
        // Every other flag keeps working in pair mode.
        let full = parse(
            args(&[
                "pair",
                "--hub",
                "https://h",
                "--code",
                "4F2K-9QXM",
                "--name",
                "我的 Mac",
                "--runtime",
                "echo",
                "--once",
            ]),
            &no_env,
        )
        .unwrap();
        assert!(full.pair && full.once);
        assert_eq!(full.name.as_deref(), Some("我的 Mac"));
        assert_eq!(full.runtime, RuntimeKind::Echo);
    }

    #[test]
    fn pairing_errors_name_the_missing_piece() {
        assert_eq!(
            parse(args(&["pair", "--code", "4F2K-9QXM"]), &no_env),
            Err(CliError::PairMissingHub)
        );
        assert_eq!(
            parse(args(&["pair", "--hub", "https://h"]), &no_env),
            Err(CliError::PairMissingCode)
        );
        // --code outside `pair` would look accepted while doing nothing.
        assert_eq!(
            parse(args(&["--code", "4F2K-9QXM"]), &no_env),
            Err(CliError::CodeOutsidePair)
        );
        assert_eq!(
            parse(
                args(&["pair", "--hub", "https://h", "--code", "   "]),
                &no_env
            ),
            Err(CliError::PairMissingCode)
        );
        for error in [
            CliError::PairMissingHub,
            CliError::PairMissingCode,
            CliError::CodeOutsidePair,
        ] {
            assert!(error.to_string().contains("pair"), "{error}");
        }
    }

    #[test]
    fn an_unpaired_run_does_not_claim_the_default_hub_was_given() {
        let cli = parse(Vec::new(), &no_env).unwrap();
        assert!(
            !cli.hub_explicit,
            "the default hub is not an explicit choice"
        );
        assert!(!cli.pair);
        assert_eq!(cli.code, None);
        assert!(
            parse(args(&["--hub", "wss://h"]), &no_env)
                .unwrap()
                .hub_explicit
        );
    }

    #[test]
    fn the_hub_may_be_given_as_http_and_is_dialled_over_ws() {
        assert_eq!(
            connector_ws_url("https://hub.example.com", "cxt_abc"),
            "wss://hub.example.com/api/v2/connector/ws?token=cxt_abc"
        );
        assert_eq!(
            connector_ws_url("http://127.0.0.1:7400/", "cxt_abc"),
            "ws://127.0.0.1:7400/api/v2/connector/ws?token=cxt_abc"
        );
        // The ws(s) forms are already what the connector speaks.
        assert_eq!(
            connector_ws_url("wss://h:1", "cxt_abc"),
            "wss://h:1/api/v2/connector/ws?token=cxt_abc"
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
