//! What this device can serve right now, probed once at startup.
//!
//! The hub forwards these entries verbatim as `connector.runtimes` and the UI
//! renders them, so every entry must be an observation rather than a guess:
//!
//! * `codex` is a PATH lookup — spawning the CLI would start a process the user
//!   never asked for;
//! * `dsh` is a real `initialize` against the bridge endpoint, because an
//!   endpoint file on disk does not prove that anything is listening;
//! * `echo` is this process itself.
//!
//! Only the runtimes this launch mode can actually serve are reported, plus the
//! `dsh` probe (the UI guides the user through the bridge with it). Announcing
//! `codex` from an `--runtime echo` process would be a claim it cannot honour.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tokio::time::timeout;

use crate::cli::RuntimeKind;
use crate::dsh::client::DshClient;
use crate::dsh::handshake::InitializeParams;
use crate::dsh::{self, discovery, rpc, Endpoint};

pub const CODEX: &str = "codex";
pub const DSH: &str = "dsh";
pub const ECHO: &str = "echo";

/// The whole bridge-handshake budget. A bridge that accepts the socket and then
/// says nothing must not hold the first `hello` open.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// One entry of `hello.agent.supportedRuntimes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeReport {
    pub name: String,
    pub available: bool,
    /// The device's own words when `available` is false. Serialized as `null`
    /// rather than omitted: the contract spells the field `string | null`.
    #[serde(default)]
    pub reason: Option<String>,
}

impl RuntimeReport {
    pub fn available(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            available: true,
            reason: None,
        }
    }

    pub fn unavailable(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            available: false,
            reason: Some(reason.into()),
        }
    }
}

/// The entries to report for a launch mode, given what each probe observed.
///
/// `None` means "not probed": the launch mode cannot serve that runtime, and a
/// missing observation must not turn into an availability claim.
pub fn reports(
    runtime: RuntimeKind,
    codex: Option<Result<(), String>>,
    dsh: Option<Result<(), String>>,
) -> Vec<RuntimeReport> {
    match runtime {
        RuntimeKind::Codex => vec![from_probe(CODEX, codex), from_probe(DSH, dsh)],
        RuntimeKind::Echo => vec![RuntimeReport::available(ECHO), from_probe(DSH, dsh)],
        // Here the dsh probe *is* this launch mode's own entry: nothing else runs.
        RuntimeKind::Dsh => vec![from_probe(DSH, dsh)],
    }
}

fn from_probe(name: &str, probe: Option<Result<(), String>>) -> RuntimeReport {
    match probe {
        Some(Ok(())) => RuntimeReport::available(name),
        Some(Err(reason)) => RuntimeReport::unavailable(name, reason),
        None => RuntimeReport::unavailable(name, "本次启动模式没有探测这一项"),
    }
}

/// Everything the probe needs, resolved once by `main` so the probe itself never
/// reads the CLI or the environment.
#[derive(Debug, Clone)]
pub struct ProbeRequest {
    pub runtime: RuntimeKind,
    /// `FARWORK_CODEX_BIN`, or the default `codex`.
    pub codex_bin: String,
    /// `--runtime dsh` resolves the endpoint before the probe runs, so the report
    /// describes the bridge that this launch would actually use.
    pub dsh_endpoint: Option<Endpoint>,
    /// `--dsh-endpoint`. Honoured by every launch mode: an operator who pinned a
    /// file must not be told about some other bridge.
    pub dsh_endpoint_override: Option<PathBuf>,
    pub agent_id: String,
    pub client_version: String,
    /// Bridge-handshake budget; injectable so a test does not have to wait out
    /// the production timeout.
    pub timeout: Duration,
}

impl ProbeRequest {
    pub async fn run(&self) -> Vec<RuntimeReport> {
        let codex = match self.runtime {
            RuntimeKind::Codex => Some(self.probe_codex()),
            // Never claim codex from a process that only serves echo or dsh.
            _ => None,
        };
        let dsh = Some(self.probe_dsh().await);
        reports(self.runtime, codex, dsh)
    }

    fn probe_codex(&self) -> Result<(), String> {
        match find_executable(&self.codex_bin) {
            Some(path) => {
                tracing::debug!(codex_bin = %path.display(), "codex CLI found");
                Ok(())
            }
            None => Err(format!(
                "{} CLI 不在 PATH 上（spawn {} ENOENT）",
                self.codex_bin, self.codex_bin
            )),
        }
    }

    async fn probe_dsh(&self) -> Result<(), String> {
        let endpoint = match &self.dsh_endpoint {
            Some(endpoint) => endpoint.clone(),
            None => {
                discovery::resolve(self.dsh_endpoint_override.as_deref(), &|key| {
                    std::env::var(key).ok()
                })
                .map_err(|error| error.to_string())?
                .endpoint
            }
        };
        let address = endpoint.address();
        let namespace = dsh::session_namespace(&self.agent_id);
        let params = InitializeParams {
            auth_token: &endpoint.token,
            connector_id: &self.agent_id,
            session_namespace: &namespace,
            client_version: &self.client_version,
        };
        let attempt = async {
            let connection = rpc::dial(&address)
                .await
                .map_err(|error| format!("连接 DSH 桥 {address} 失败：{error}"))?;
            let client = DshClient::new(connection.client.clone());
            let outcome = client
                .initialize(&address, &params)
                .await
                .map(|_handshake| ())
                .map_err(|error| error.to_string());
            // The reader task holds its own handle, so the socket is closed here
            // explicitly instead of being leaked for the life of the process.
            client.rpc().close().await;
            outcome
        };
        match timeout(self.timeout, attempt).await {
            Ok(outcome) => outcome,
            Err(_) => Err(format!(
                "DSH 桥 {address} 在 {}s 内没有完成握手",
                self.timeout.as_secs()
            )),
        }
    }
}

/// PATH lookup for a bare name, direct check for anything with a separator.
/// Existence alone is not enough: a non-executable file fails at spawn time, and
/// reporting it as available would send the user into a broken session.
fn find_executable(bin: &str) -> Option<PathBuf> {
    let trimmed = bin.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(std::path::MAIN_SEPARATOR) || trimmed.contains('/') {
        let path = PathBuf::from(trimmed);
        return is_executable(&path).then_some(path);
    }
    let names: Vec<String> = if cfg!(windows) {
        vec![trimmed.to_string(), format!("{trimmed}.exe")]
    } else {
        vec![trimmed.to_string()]
    };
    let search = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&search) {
        for name in &names {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// One startup probe, shared by every `hello` this process sends.
///
/// `warm` starts it before the first websocket dial and the hello awaits the same
/// cell, so connecting is never delayed by the probe; a reconnect reuses the
/// answer instead of re-dialling the bridge, because the report describes this
/// launch rather than the moment of the connection.
#[derive(Debug, Clone)]
pub struct RuntimeProbe {
    cell: Arc<OnceCell<Vec<RuntimeReport>>>,
    request: Option<Arc<ProbeRequest>>,
}

impl RuntimeProbe {
    pub fn new(request: ProbeRequest) -> Self {
        Self {
            cell: Arc::new(OnceCell::new()),
            request: Some(Arc::new(request)),
        }
    }

    /// Nothing to report. For callers that have no device to describe.
    pub fn none() -> Self {
        let cell = Arc::new(OnceCell::new());
        // A pre-filled cell means `reports` answers immediately.
        let _ = cell.set(Vec::new());
        Self {
            cell,
            request: None,
        }
    }

    /// Start the probe without waiting for it.
    pub fn warm(&self) {
        if self.request.is_none() {
            return;
        }
        let probe = self.clone();
        tokio::spawn(async move {
            let _ = probe.reports().await;
        });
    }

    /// The report to put in `hello`. Bounded even if a probe misbehaves: an
    /// unreported runtime list is honest, a hung handshake is not.
    pub async fn reports(&self) -> Vec<RuntimeReport> {
        let Some(request) = self.request.clone() else {
            return Vec::new();
        };
        let filled = self.cell.get_or_init(|| async {
            let reports = request.run().await;
            tracing::info!(runtimes = %summarize(&reports), "runtime probe finished");
            reports
        });
        match timeout(request.timeout + Duration::from_secs(1), filled).await {
            Ok(reports) => reports.clone(),
            Err(_) => {
                tracing::warn!("runtime probe did not finish in time; reporting nothing");
                Vec::new()
            }
        }
    }
}

fn summarize(reports: &[RuntimeReport]) -> String {
    reports
        .iter()
        .map(|report| format!("{}={}", report.name, report.available))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn request(runtime: RuntimeKind, timeout: Duration) -> ProbeRequest {
        ProbeRequest {
            runtime,
            codex_bin: String::from("codex"),
            dsh_endpoint: None,
            dsh_endpoint_override: None,
            agent_id: String::from("agt_test"),
            client_version: String::from("0.1.0"),
            timeout,
        }
    }

    #[test]
    fn a_codex_launch_reports_codex_and_the_dsh_probe() {
        let reports = reports(RuntimeKind::Codex, Some(Ok(())), Some(Ok(())));
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "codex");
        assert!(reports[0].available);
        assert_eq!(reports[0].reason, None);
        assert_eq!(reports[1].name, "dsh");
        assert!(reports[1].available);
    }

    #[test]
    fn a_failed_codex_lookup_keeps_its_reason_and_does_not_fail_the_dsh_entry() {
        let reports = reports(
            RuntimeKind::Codex,
            Some(Err(String::from(
                "codex CLI 不在 PATH 上（spawn codex ENOENT）",
            ))),
            Some(Err(String::from("no DSH bridge endpoint found"))),
        );
        assert!(!reports[0].available);
        assert_eq!(
            reports[0].reason.as_deref(),
            Some("codex CLI 不在 PATH 上（spawn codex ENOENT）")
        );
        assert!(!reports[1].available);
        // The bridge's own words survive verbatim: the UI shows this to the user.
        assert_eq!(
            reports[1].reason.as_deref(),
            Some("no DSH bridge endpoint found")
        );
    }

    #[test]
    fn an_echo_launch_never_claims_codex_or_dsh_it_cannot_serve() {
        let reports = reports(RuntimeKind::Echo, None, Some(Ok(())));
        assert_eq!(
            reports
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["echo", "dsh"]
        );
        assert!(reports[0].available, "echo is this process");
        assert!(reports[1].available, "the dsh entry comes from the probe");
    }

    #[test]
    fn a_dsh_launch_reports_one_entry_built_from_the_real_handshake() {
        let reports = reports(
            RuntimeKind::Dsh,
            None,
            Some(Err(String::from(
                "initialize was rejected: bridge error -32600: nope",
            ))),
        );
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].name, "dsh");
        assert!(!reports[0].available);
        assert_eq!(
            reports[0].reason.as_deref(),
            Some("initialize was rejected: bridge error -32600: nope")
        );
    }

    #[test]
    fn an_unprobed_runtime_is_reported_unavailable_with_a_reason_not_by_panic() {
        let reports = reports(RuntimeKind::Codex, None, None);
        for row in &reports {
            assert!(!row.available);
            assert!(row.reason.is_some(), "a false must always carry a reason");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_codex_probe_requires_an_executable_file() {
        let dir = std::env::temp_dir().join(format!("farwork-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        use std::io::Write;

        let plain = dir.join("codex-not-executable");
        let mut file = std::fs::File::create(&plain).expect("create");
        file.write_all(b"#!/bin/sh\n").expect("write");
        assert!(
            !is_executable(&plain),
            "a file nobody may execute is not a codex install"
        );

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&plain));
        assert!(find_executable(&plain.to_string_lossy()).is_some());

        let missing = dir.join("codex-absent");
        assert!(!is_executable(&missing));
        assert!(find_executable(&missing.to_string_lossy()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_bridge_endpoint_is_a_reason_not_a_dial() {
        // An explicit file that does not exist: discovery refuses instead of
        // searching elsewhere, so no socket is ever opened.
        let mut request = request(RuntimeKind::Dsh, Duration::from_millis(200));
        request.dsh_endpoint_override = Some(std::env::temp_dir().join(format!(
            "farwork-no-such-endpoint-{}.json",
            std::process::id()
        )));
        let reason = request
            .probe_dsh()
            .await
            .expect_err("a missing endpoint is not an available runtime");
        assert!(
            reason.contains("no DSH bridge endpoint"),
            "discovery's own message is what the UI shows: {reason}"
        );
    }

    #[tokio::test]
    async fn a_bridge_that_never_answers_times_out_instead_of_hanging() {
        // Accepts the socket and then says nothing: exactly the failure the
        // timeout exists for.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let mut request = request(RuntimeKind::Dsh, Duration::from_millis(150));
        request.dsh_endpoint = Some(Endpoint {
            host: String::from("127.0.0.1"),
            port,
            token: String::from("test-token"),
            pid: None,
        });
        let started = std::time::Instant::now();
        let outcome = request.probe_dsh().await;
        assert!(outcome.is_err());
        assert!(
            started.elapsed() < PROBE_TIMEOUT,
            "the probe must give up on its own budget, took {:?}",
            started.elapsed()
        );
        assert!(outcome.unwrap_err().contains("握手"));
    }

    #[tokio::test]
    async fn the_probe_answers_once_and_reuses_it_for_later_hellos() {
        let probe = RuntimeProbe::new(request(RuntimeKind::Echo, Duration::from_millis(150)));
        let first = probe.reports().await;
        let second = probe.reports().await;
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0].name, "echo");
        assert_eq!(probe.reports().await[0].name, "echo");
    }

    #[tokio::test]
    async fn a_probe_with_nothing_to_say_reports_nothing() {
        assert!(RuntimeProbe::none().reports().await.is_empty());
        RuntimeProbe::none().warm();
    }
}
