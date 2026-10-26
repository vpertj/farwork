//! Locating the DSH bridge endpoint.
//!
//! Two bridges can be installed at once and each publishes its own small file
//! with a loopback host, a port and a bearer token:
//!
//! * `~/.farwork/dsh-bridge/endpoint.json` — our own
//!   `@farwork/dsh-bridge` plugin, searched **first** so `--runtime dsh`
//!   needs no `--dsh-endpoint` flag once the plugin is installed;
//! * `~/.agents-anywhere/dsh-bridge/endpoint.json` — the upstream
//!   `@agents-anywhere/dsh-bridge`, kept as a compatibility fallback;
//! * `$DSH_HOME/agents-anywhere/bridge/endpoint.json` — the upstream
//!   fallback location.
//!
//! The two plugins never share a directory (they would evict each other's endpoint
//! and lock), so the search has to know both. Every field is validated: a file
//! naming another host, a port outside 1..=65535, an empty token or an unknown
//! version is refused rather than dialled "best effort". A missing endpoint is a
//! startup error, never a silent "runtime unavailable" that would look like a
//! working DSH session that never answers.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `$HOME/.farwork/dsh-bridge/endpoint.json` — our own plugin.
pub const FARWORK_SUFFIX: &str = ".farwork/dsh-bridge/endpoint.json";
/// `$HOME/.agents-anywhere/dsh-bridge/endpoint.json` — upstream plugin.
pub const UPSTREAM_SUFFIX: &str = ".agents-anywhere/dsh-bridge/endpoint.json";
/// `$DSH_HOME/agents-anywhere/bridge/endpoint.json` — upstream fallback.
pub const DSH_HOME_SUFFIX: &str = "agents-anywhere/bridge/endpoint.json";
pub const DSH_HOME_ENV: &str = "DSH_HOME";
/// Where upstream looks for `DSH_HOME` when the variable is unset.
pub const DEFAULT_DSH_HOME: &str = ".dsh";
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

/// Which candidate produced the endpoint. A value rather than a log line so the
/// startup message can name it: with two plugins installable side by side,
/// "connected to some bridge" is not enough — an operator who installed ours has
/// to be able to see that the farwork one is (or is not) the one in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSource {
    Explicit,
    Farwork,
    Upstream,
    UpstreamDshHome,
}

impl EndpointSource {
    /// The `dsh_endpoint_source` value; `farwork` vs `upstream` is the
    /// distinction that matters, the rest only says which of their files.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Farwork => "farwork",
            Self::Upstream => "upstream",
            Self::UpstreamDshHome => "upstream-dsh-home",
        }
    }
}

/// A validated endpoint plus where it came from — both belong in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub endpoint: Endpoint,
    pub path: PathBuf,
    pub source: EndpointSource,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("{message}")]
    NotFound { message: String },
    #[error("cannot read DSH bridge endpoint {path}: {message}")]
    Unreadable { path: String, message: String },
    #[error("invalid DSH bridge endpoint {path}: {reason}")]
    Invalid { path: String, reason: String },
}

impl DiscoveryError {
    /// An explicit `--dsh-endpoint` that does not exist is an error, not a
    /// hint to search the defaults: the operator named a file, so a different
    /// bridge answering would hide the typo behind a session that "works".
    fn missing_explicit(path: &Path) -> Self {
        Self::NotFound {
            message: format!(
                "no DSH bridge endpoint at {}: --dsh-endpoint names the file to use and is never \
                 back-filled from the default locations (drop the flag to search {FARWORK_SUFFIX}, \
                 {UPSTREAM_SUFFIX} and $DSH_HOME/{DSH_HOME_SUFFIX})",
                path.display()
            ),
        }
    }

    /// All four candidates by name, plus what to check. The four move in and out of
    /// existence depending on which plugin is installed and whether DSH was
    /// restarted after installing it, so a vague "not found" is unactionable.
    fn missing_defaults(home: Option<&Path>, dsh_home: Option<&Path>) -> Self {
        let farwork = under_home(home, FARWORK_SUFFIX);
        let upstream = under_home(home, UPSTREAM_SUFFIX);
        let dsh_home_path = match dsh_home {
            Some(dsh_home) => dsh_home.join(DSH_HOME_SUFFIX).display().to_string(),
            None => format!("$DSH_HOME/{DSH_HOME_SUFFIX}"),
        };
        Self::NotFound {
            message: format!(
                "no DSH bridge endpoint found. Looked for, in this order: \
                 1) --dsh-endpoint <path> (not given); \
                 2) {farwork} (farwork DSH bridge plugin); \
                 3) {upstream} (upstream agents-anywhere plugin); \
                 4) {dsh_home_path} ($DSH_HOME fallback). \
                 Installed the farwork DSH bridge plugin? Make sure DSH was restarted so the \
                 plugin republished its endpoint. Using the upstream agents-anywhere plugin? \
                 Make sure it is enabled in DSH. Or pass --dsh-endpoint <path> to name the file \
                 yourself."
            ),
        }
    }
}

fn under_home(home: Option<&Path>, suffix: &str) -> String {
    match home {
        Some(home) => home.join(suffix).display().to_string(),
        None => format!("~/{suffix}"),
    }
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

/// The default locations in preference order: our plugin, then upstream's two.
/// Parameterized so tests never have to touch the process environment or the
/// real home directory.
pub fn candidate_paths(
    home: Option<&Path>,
    dsh_home: Option<&Path>,
) -> Vec<(PathBuf, EndpointSource)> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        candidates.push((home.join(FARWORK_SUFFIX), EndpointSource::Farwork));
        candidates.push((home.join(UPSTREAM_SUFFIX), EndpointSource::Upstream));
    }
    if let Some(dsh_home) = dsh_home {
        candidates.push((
            dsh_home.join(DSH_HOME_SUFFIX),
            EndpointSource::UpstreamDshHome,
        ));
    }
    candidates
}

/// Locate and validate the endpoint. `explicit` (from `--dsh-endpoint`) wins and
/// is used alone: an operator who names a file must not silently get a different
/// bridge from the default search path.
pub fn resolve(
    explicit: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Located, DiscoveryError> {
    if let Some(path) = explicit {
        let endpoint = read(path).map_err(|error| match error {
            DiscoveryError::NotFound { .. } => DiscoveryError::missing_explicit(path),
            other => other,
        })?;
        return Ok(Located {
            endpoint,
            path: path.to_path_buf(),
            source: EndpointSource::Explicit,
        });
    }
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from);
    let dsh_home = env(DSH_HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(DEFAULT_DSH_HOME)));
    for (path, source) in candidate_paths(home.as_deref(), dsh_home.as_deref()) {
        if path.is_file() {
            let endpoint = read(&path)?;
            return Ok(Located {
                endpoint,
                path,
                source,
            });
        }
    }
    Err(DiscoveryError::missing_defaults(
        home.as_deref(),
        dsh_home.as_deref(),
    ))
}

fn read(path: &Path) -> Result<Endpoint, DiscoveryError> {
    let shown = path.display().to_string();
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(DiscoveryError::NotFound {
                message: format!("no DSH bridge endpoint found (looked for {shown})"),
            })
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

    /// A throwaway home/dsh-home pair under the temp dir, removed on drop.
    struct Root(PathBuf);

    impl Root {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "farwork-dsh-{tag}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("mkdir");
            Self(root)
        }

        fn home(&self) -> PathBuf {
            self.0.join("home")
        }

        fn dsh_home(&self) -> PathBuf {
            self.0.join("dsh")
        }

        fn write(&self, base: &Path, suffix: &str, port: u32, token: &str) -> PathBuf {
            let path = base.join(suffix);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, endpoint_json("127.0.0.1", port, token)).expect("write");
            path
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
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
    fn the_four_sources_are_tried_in_precedence_order() {
        let root = Root::new("order");
        let home = root.home();
        let dsh_home = root.dsh_home();
        let farwork = root.write(&home, FARWORK_SUFFIX, 7501, "farwork");
        let upstream = root.write(&home, UPSTREAM_SUFFIX, 7502, "upstream");
        let fallback = root.write(&dsh_home, DSH_HOME_SUFFIX, 7503, "dsh-home");
        let home_s = home.display().to_string();
        let dsh_s = dsh_home.display().to_string();
        let env = move |key: &str| match key {
            "HOME" => Some(home_s.clone()),
            "DSH_HOME" => Some(dsh_s.clone()),
            _ => None,
        };

        let located = resolve(None, &env).expect("farwork plugin first");
        assert_eq!(
            (located.endpoint.port, located.source),
            (7501, EndpointSource::Farwork)
        );
        assert_eq!(located.path, farwork);

        std::fs::remove_file(&farwork).expect("rm farwork");
        let located = resolve(None, &env).expect("upstream second");
        assert_eq!(
            (located.endpoint.port, located.source),
            (7502, EndpointSource::Upstream)
        );
        assert_eq!(located.path, upstream);

        std::fs::remove_file(&upstream).expect("rm upstream");
        let located = resolve(None, &env).expect("dsh home third");
        assert_eq!(
            (located.endpoint.port, located.source),
            (7503, EndpointSource::UpstreamDshHome)
        );
        assert_eq!(located.path, fallback);

        std::fs::remove_file(&fallback).expect("rm fallback");
        let error = resolve(None, &env).unwrap_err();
        assert!(matches!(error, DiscoveryError::NotFound { .. }), "{error}");
    }

    #[test]
    fn an_explicit_path_is_used_alone_and_a_missing_one_never_falls_back() {
        let root = Root::new("explicit");
        let home = root.home();
        // A farwork endpoint is present: it must still not answer for the flag.
        root.write(&home, FARWORK_SUFFIX, 7501, "farwork");
        let explicit = root.0.join("elsewhere.json");
        std::fs::write(&explicit, endpoint_json("127.0.0.1", 7511, "explicit")).expect("write");
        let home_s = home.display().to_string();
        let env = move |key: &str| (key == "HOME").then(|| home_s.clone());

        let located = resolve(Some(&explicit), &env).expect("explicit");
        assert_eq!(
            (located.endpoint.port, located.source),
            (7511, EndpointSource::Explicit)
        );
        assert_eq!(located.path, explicit);

        let missing = root.0.join("nope.json");
        let error = resolve(Some(&missing), &env).unwrap_err();
        assert!(matches!(error, DiscoveryError::NotFound { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("nope.json"), "{message}");
        assert!(message.contains("--dsh-endpoint"), "{message}");
        assert!(
            message.contains("never back-filled"),
            "the refusal must say the defaults are not consulted: {message}"
        );
    }

    #[test]
    fn home_and_dsh_home_are_combined_for_the_last_candidate() {
        let root = Root::new("dshhome");
        let home = root.home();
        // $HOME/.dsh stands in when DSH_HOME is unset.
        root.write(
            &home.join(DEFAULT_DSH_HOME),
            DSH_HOME_SUFFIX,
            7521,
            "implicit",
        );
        let home_s = home.display().to_string();
        let only_home = move |key: &str| (key == "HOME").then(|| home_s.clone());
        let located = resolve(None, &only_home).expect("implicit dsh home");
        assert_eq!(located.endpoint.port, 7521);
        assert_eq!(
            located.path,
            home.join(DEFAULT_DSH_HOME).join(DSH_HOME_SUFFIX)
        );

        // DSH_HOME replaces that default entirely.
        let dsh_home = root.dsh_home();
        root.write(&dsh_home, DSH_HOME_SUFFIX, 7522, "explicit-dsh-home");
        let home_s = home.display().to_string();
        let dsh_s = dsh_home.display().to_string();
        let both = move |key: &str| match key {
            "HOME" => Some(home_s.clone()),
            "DSH_HOME" => Some(dsh_s.clone()),
            _ => None,
        };
        let located = resolve(None, &both).expect("explicit dsh home");
        assert_eq!(located.endpoint.port, 7522);
        assert_eq!(located.path, dsh_home.join(DSH_HOME_SUFFIX));
    }

    #[test]
    fn every_source_has_a_distinct_startup_log_tag() {
        let tags = [
            EndpointSource::Explicit,
            EndpointSource::Farwork,
            EndpointSource::Upstream,
            EndpointSource::UpstreamDshHome,
        ]
        .map(EndpointSource::tag);
        assert_eq!(
            tags,
            ["explicit", "farwork", "upstream", "upstream-dsh-home"]
        );
        // The two families an operator has to tell apart share no prefix.
        let mut unique = tags.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), tags.len());
    }

    #[test]
    fn no_endpoint_anywhere_lists_every_candidate_and_what_to_check() {
        let root = Root::new("none");
        let home = root.home();
        let home_s = home.display().to_string();
        let env = move |key: &str| (key == "HOME").then(|| home_s.clone());

        let error = resolve(None, &env).unwrap_err();
        assert!(matches!(error, DiscoveryError::NotFound { .. }));
        let message = error.to_string();
        for needle in [
            home.join(FARWORK_SUFFIX).display().to_string(),
            home.join(UPSTREAM_SUFFIX).display().to_string(),
            home.join(DEFAULT_DSH_HOME)
                .join(DSH_HOME_SUFFIX)
                .display()
                .to_string(),
            String::from("--dsh-endpoint"),
            String::from("restarted"),
            String::from("enabled in DSH"),
        ] {
            assert!(message.contains(&needle), "missing {needle:?} in {message}");
        }
    }
}
