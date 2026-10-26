//! The DSH runtime: one bridge session per hub stream.
//!
//! The shape of one session:
//!
//! 1. dial the loopback bridge and `initialize`, keeping the feature flags as a
//!    gate for what this session may claim;
//! 2. `session.createAndStart` for the first turn, `session.startTurn` after it;
//! 3. `runtime.sync.subscribe` and then, forever, batch → frames → ack.
//!
//! The bridge owns items, state and notices. This module only translates them into
//! Ingress Protocol frames, which is why it never invents an item, a status or a
//! button the runtime did not send.

pub mod client;
pub mod discovery;
pub mod handshake;
pub mod interaction;
pub mod projection;
pub mod rpc;
pub mod selections;
pub mod sync;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;

use crate::codex::session::{ExitReport, SessionCmd};
use crate::protocol::{CatalogSet, ClientFrame, Decision, SessionState};
use crate::timeutil;

use self::client::{CapabilitySet, DshClient, MessageIds, SessionPair};
pub use self::discovery::Endpoint;
use self::handshake::{Features, InitializeParams};
use self::interaction::NoticeQueue;
use self::rpc::{Inbound, RpcError};
use self::sync::{
    Batch, Checkpoints, SnapshotStage, SyncError, SyncStream, CHECKPOINT_VERSION,
    UPLOAD_PROGRESS_VERSION,
};

pub use self::projection::capabilities_frame;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many times one connection may answer a dead sync feed with a resubscribe.
///
/// Each attempt persists the checkpoints of every session the bridge managed to
/// stream, so the next attempt resumes further along; more than a couple of them
/// is a bridge that will not converge, and pretending otherwise just spins.
const MAX_SYNC_RECOVERIES: u32 = 3;

#[derive(Debug, Clone)]
pub struct DshOptions {
    pub endpoint: Endpoint,
    pub connector_id: String,
    /// The bridge session namespace this connector writes into.
    pub session_namespace: String,
    /// Hub-minted stream id.
    pub stream_id: String,
    /// The hub's session id, reused verbatim as DSH's `sessionId` so a retried
    /// `createAndStart` addresses the same session instead of creating a second.
    pub hub_session_id: String,
    pub cwd: String,
    pub prompt: String,
    pub client_version: String,
    /// `selections` for `createAndStart`, already resolved from the operator's
    /// `--dsh-permission` / `--dsh-model` at startup. `None` keeps DSH's defaults.
    pub selections: Option<Value>,
    /// `agentPreset` for `createAndStart`, resolved from `--dsh-preset`. `None`
    /// keeps whichever Agent mode the bridge considers default.
    pub agent_preset: Option<String>,
    /// Where durable checkpoints live (`$FARWORK_HOME/dsh/sync/checkpoints`).
    /// `None` keeps them in memory for one process: still correct, but every
    /// reconnect replays the whole inventory, which is what overflows the bridge's
    /// own event queue on an instance with dozens of sessions.
    pub checkpoint_dir: Option<PathBuf>,
}

/// Side effects that need a real RPC and therefore cannot happen while the
/// operation list is being walked.
#[derive(Debug, Default)]
struct Refresh {
    capabilities: bool,
    catalogs: bool,
}

pub struct DshSession {
    endpoint_address: String,
    auth_token: String,
    connector_id: String,
    session_namespace: String,
    client_version: String,
    stream_id: String,
    hub_session_id: String,
    cwd: String,
    prompt: String,
    selections: Option<Value>,
    agent_preset: Option<String>,
    out: UnboundedSender<ClientFrame>,
    cmd: Option<UnboundedReceiver<SessionCmd>>,
    features: Features,
    session: SessionPair,
    state: SessionState,
    sync: Option<SyncStream>,
    catalogs: CatalogSet,
    capabilities: CapabilitySet,
    snapshots: SnapshotStage,
    checkpoints: Checkpoints,
    notices: NoticeQueue,
    messages: MessageIds,
    /// How many times the bridge's sync feed has died on this connection's watch;
    /// it bounds the retries (see `on_runtime_error`).
    sync_failures: u32,
}

impl DshSession {
    pub fn new(
        options: DshOptions,
        out: UnboundedSender<ClientFrame>,
        cmd: UnboundedReceiver<SessionCmd>,
    ) -> Self {
        Self {
            endpoint_address: options.endpoint.address(),
            auth_token: options.endpoint.token,
            connector_id: options.connector_id,
            session_namespace: options.session_namespace,
            client_version: options.client_version,
            stream_id: options.stream_id,
            hub_session_id: options.hub_session_id.clone(),
            cwd: options.cwd,
            prompt: options.prompt,
            selections: options.selections,
            agent_preset: options.agent_preset,
            out,
            cmd: Some(cmd),
            features: Features::default(),
            session: SessionPair::new(options.hub_session_id, String::new()),
            state: SessionState::Idle,
            sync: None,
            catalogs: CatalogSet::default(),
            capabilities: CapabilitySet::default(),
            snapshots: SnapshotStage::new(),
            checkpoints: match &options.checkpoint_dir {
                Some(dir) => Checkpoints::open(dir),
                None => Checkpoints::in_memory(),
            },
            notices: NoticeQueue::new(),
            messages: MessageIds::new(),
            sync_failures: 0,
        }
    }

    pub async fn run(self) -> ExitReport {
        let connection = match timeout(CONNECT_TIMEOUT, rpc::dial(&self.endpoint_address)).await {
            Err(_) => {
                return self.fail(
                    1,
                    format!(
                        "connecting to the DSH bridge at {} timed out",
                        self.endpoint_address
                    ),
                )
            }
            Ok(Err(error)) => {
                return self.fail(
                    1,
                    format!(
                        "cannot connect to the DSH bridge at {}: {error}",
                        self.endpoint_address
                    ),
                )
            }
            Ok(Ok(connection)) => connection,
        };
        // The reader task keeps its own handle, so the socket has to be closed
        // explicitly once this session is done with it.
        let client = DshClient::new(connection.client.clone());
        let report = self.serve(connection, &client).await;
        client.rpc().close().await;
        report
    }

    async fn serve(mut self, connection: rpc::Connection, client: &DshClient) -> ExitReport {
        let mut notifications = connection.notifications;
        let params = InitializeParams {
            auth_token: &self.auth_token,
            connector_id: &self.connector_id,
            session_namespace: &self.session_namespace,
            client_version: &self.client_version,
        };
        let handshake = match client.initialize(&self.endpoint_address, &params).await {
            Ok(handshake) => handshake,
            Err(error) => return self.fail(1, format!("DSH initialize failed: {error}")),
        };
        tracing::info!(
            stream_id = %self.stream_id,
            runtime = %handshake.identity.runtime_version,
            bridge = %handshake.identity.bridge_version,
            protocol = %handshake.identity.protocol_version,
            approval = handshake.features.approval,
            user_questions = handshake.features.user_questions,
            "dsh bridge initialised"
        );
        self.features = handshake.features;
        if !self.features.events() {
            // The only sync mode this agent implements is the ordered event stream.
            // Polling would leave the timeline silently stale.
            return self.fail(
                2,
                format!(
                    "the DSH bridge offers syncMode {:?}; this agent only serves \"events\"",
                    self.features.sync_mode
                ),
            );
        }

        let client_message_id = self.messages.resolve(None, &self.prompt);
        let created = client
            .create_and_start(
                &self.hub_session_id,
                &self.prompt,
                &client_message_id,
                &self.cwd,
                self.selections.as_ref(),
                self.agent_preset.as_deref(),
            )
            .await;
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                return self.fail(
                    1,
                    format!("session.createAndStart failed: {}", error.message()),
                )
            }
        };
        if let Some(false) = created.get("ok").and_then(Value::as_bool) {
            let message = created
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the bridge refused to start the session");
            return self.fail(1, format!("session.createAndStart refused: {message}"));
        }
        let external = external_session_id(&created).unwrap_or_else(|| self.hub_session_id.clone());
        self.session = SessionPair::new(self.hub_session_id.clone(), external);
        self.send(ClientFrame::SessionStarted {
            stream_id: self.stream_id.clone(),
            external_session_id: self.session.external_session_id.clone(),
        });

        self.refresh_catalogs(client).await;
        self.refresh_capabilities(client).await;
        self.send_capabilities();

        if let Err(report) = self.subscribe(client).await {
            return report;
        }
        self.set_state(SessionState::Running);

        let Some(mut cmd) = self.cmd.take() else {
            return self.finish(0, "session inputs closed");
        };
        loop {
            let signal = tokio::select! {
                maybe = notifications.recv() => Signal::Notification(maybe),
                maybe = cmd.recv() => Signal::Command(maybe),
            };
            let outcome = match signal {
                Signal::Notification(Some(inbound)) => self.on_notification(client, inbound).await,
                Signal::Notification(None) => {
                    return self.fail(1, "the DSH bridge closed the event stream")
                }
                Signal::Command(Some(command)) => self.on_command(client, command).await,
                Signal::Command(None) => return self.finish(0, "hub command channel closed"),
            };
            if let Err(report) = outcome {
                return report;
            }
        }
    }

    // ------------------------------------------------------------ notifications

    async fn on_notification(
        &mut self,
        client: &DshClient,
        inbound: Inbound,
    ) -> Result<(), ExitReport> {
        if inbound.method == "runtime.sync.batch" {
            return self.on_batch(client, &inbound.params).await;
        }
        if inbound.method == "runtime.error" {
            return self.on_runtime_error(client, &inbound.params).await;
        }

        let mut frames = Vec::new();
        let mut refresh = Refresh::default();
        self.apply_notification(
            inbound.method.as_str(),
            &inbound.params,
            &mut frames,
            &mut refresh,
        );
        self.flush(frames);
        if refresh.catalogs {
            self.refresh_catalogs(client).await;
        }
        if refresh.capabilities {
            self.refresh_capabilities(client).await;
        }
        if refresh.catalogs || refresh.capabilities {
            self.send_capabilities();
        }
        Ok(())
    }

    /// The bridge reports a dead sync feed as `runtime.error` with
    /// `data.scope == "sync"` — on a big DSH instance that is its own event queue
    /// (10000 events) overflowing while it replays the whole inventory.
    ///
    /// Retrying that as an unknown error is useless: the replay starts over and the
    /// queue fills again. A retry only earns its place when the checkpoints we
    /// persist let the bridge *resume* instead of replaying, and it is bounded.
    /// Past that the operator gets the cause and the one action that fixes it,
    /// because this agent cannot shrink somebody's DSH instance for them.
    async fn on_runtime_error(
        &mut self,
        client: &DshClient,
        params: &Value,
    ) -> Result<(), ExitReport> {
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the DSH bridge reported an error");
        let data = params.get("data");
        let code = data
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("INTERNAL_ERROR");
        let scope = data
            .and_then(|data| data.get("scope"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if scope != "sync" {
            tracing::warn!(%message, "dsh runtime error");
            self.send(ClientFrame::Error {
                message: message.to_string(),
                stream_id: Some(self.stream_id.clone()),
            });
            return Ok(());
        }

        self.sync_failures += 1;
        if self.sync_failures <= MAX_SYNC_RECOVERIES && !self.checkpoints.is_empty() {
            tracing::warn!(
                %code,
                %message,
                attempt = self.sync_failures,
                checkpoints = self.checkpoints.len(),
                "dsh sync feed failed; resubscribing, the persisted checkpoints let the bridge resume"
            );
            return self.resubscribe(client).await;
        }
        Err(self.fail(
            1,
            format!(
                "the DSH bridge dropped its sync stream ({code}: {message}). The bridge replays the \
                 whole session inventory on subscribe and its own event queue (10000 events) \
                 overflows before that replay finishes; retrying cannot fix it. Archive old \
                 sessions in DSH (or run the bridge against fewer sessions) and reconnect. This \
                 agent has persisted {} checkpoint(s) at {} — every session covered by one will \
                 resume on the next connection instead of replaying.",
                self.checkpoints.len(),
                match self.checkpoints.dir() {
                    Some(dir) => dir.display().to_string(),
                    None => String::from("<in memory only>"),
                },
            ),
        ))
    }

    async fn on_batch(&mut self, client: &DshClient, params: &Value) -> Result<(), ExitReport> {
        let batch = match Batch::parse(params) {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!(%error, "malformed dsh batch; resubscribing");
                return self.resubscribe(client).await;
            }
        };
        let accepted = match self.sync.as_mut() {
            Some(stream) => stream.accept(&batch),
            None => Err(SyncError::WrongStream {
                found: batch.stream_id.clone(),
                expected: String::from("<no subscription>"),
            }),
        };
        if let Err(error) = accepted {
            // A gap means events were lost, so nothing staged can be trusted and a
            // fresh subscription is the only recovery that leaves no hole.
            tracing::warn!(%error, "dsh batch rejected; resubscribing");
            return self.resubscribe(client).await;
        }

        let now = timeutil::utc_now();
        let mut frames = Vec::new();
        let mut refresh = Refresh::default();
        let mut ack_checkpoint: Option<Value> = None;
        for operation in &batch.operations {
            self.apply_operation(
                operation,
                &now,
                &mut frames,
                &mut refresh,
                &mut ack_checkpoint,
            );
        }
        self.flush(frames);
        if refresh.catalogs {
            self.refresh_catalogs(client).await;
        }
        if refresh.capabilities {
            self.refresh_capabilities(client).await;
        }
        if refresh.catalogs || refresh.capabilities {
            self.send_capabilities();
        }
        self.ack(client, batch.batch_seq, ack_checkpoint).await
    }

    /// Apply one `operations[]` entry. Anything naming another session is skipped:
    /// one stream carries the whole visible inventory, and only one of those
    /// sessions is this stream.
    fn apply_operation(
        &mut self,
        operation: &Value,
        now: &str,
        frames: &mut Vec<ClientFrame>,
        refresh: &mut Refresh,
        ack_checkpoint: &mut Option<Value>,
    ) {
        let Some(kind) = operation.get("kind").and_then(Value::as_str) else {
            tracing::warn!("dsh operation without a kind");
            return;
        };
        match kind {
            "notifications" => {
                let notifications = operation
                    .get("notifications")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for notification in &notifications {
                    let method = notification
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let params = notification.get("params").cloned().unwrap_or(Value::Null);
                    self.apply_notification(method, &params, frames, refresh);
                }
            }
            "snapshot.begin" => {
                if !self.is_ours(operation) {
                    return;
                }
                let snapshot_id = operation
                    .get("snapshotId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let session_id = operation
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let through_seq = operation
                    .get("throughSeq")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1);
                let external = operation
                    .get("meta")
                    .and_then(|meta| meta.get("externalSessionId"))
                    .and_then(Value::as_str);
                self.snapshots
                    .begin(session_id, snapshot_id, through_seq, external);
            }
            "snapshot.items" => {
                let Some(snapshot_id) = operation.get("snapshotId").and_then(Value::as_str) else {
                    return;
                };
                if !self.is_ours(operation) || !self.snapshot_matches(snapshot_id) {
                    return;
                }
                let items = operation
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Err(error) = self.snapshots.push(&items) {
                    // The capture cannot be trusted: dropping it keeps what the
                    // timeline had instead of losing history to a broken page set.
                    tracing::warn!(%error, "dsh snapshot page rejected; discarding the capture");
                    self.snapshots.abort(snapshot_id);
                }
            }
            "snapshot.commit" => {
                let Some(snapshot_id) = operation.get("snapshotId").and_then(Value::as_str) else {
                    return;
                };
                if !self.is_ours(operation) || !self.snapshot_matches(snapshot_id) {
                    return;
                }
                let total_items = operation
                    .get("totalItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let through_seq = operation
                    .get("throughSeq")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1);
                match self.snapshots.commit(snapshot_id, total_items, through_seq) {
                    Ok(committed) => {
                        for item in &committed.items {
                            if let Some(envelope) = projection::item(item, now) {
                                frames.push(ClientFrame::ItemUpsert {
                                    stream_id: self.stream_id.clone(),
                                    item: envelope,
                                });
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "dsh snapshot commit rejected; nothing forwarded")
                    }
                }
            }
            "snapshot.abort" => {
                if let Some(snapshot_id) = operation.get("snapshotId").and_then(Value::as_str) {
                    self.snapshots.abort(snapshot_id);
                }
            }
            "checkpoint.load" => {
                let external = operation
                    .get("externalSessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                // The answer rides on the ack, `null` when we have none: an omitted
                // key would read as "this extension is not implemented". One ack can
                // carry one checkpoint, so a batch asking for two would silently
                // drop an answer — the bridge sends one per batch, say so if not.
                if ack_checkpoint.is_some() {
                    tracing::warn!(
                        external_session_id = external,
                        "a dsh batch asked for more than one checkpoint; answering the last"
                    );
                }
                *ack_checkpoint = Some(self.checkpoints.load(external).unwrap_or(Value::Null));
            }
            "checkpoint.save" => {
                let external = operation
                    .get("externalSessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(checkpoint) = operation.get("checkpoint") {
                    // Applied before the ack goes out: acknowledging durable state we
                    // have not written is how a reconnect turns into a full replay.
                    self.checkpoints.save(external, checkpoint.clone());
                }
            }
            "checkpoint.delete" => {
                let external = operation
                    .get("externalSessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.checkpoints.delete(external);
            }
            "workspace.inventory" => {
                // Ignored by contract: projects are derived from cwd.
            }
            other => tracing::debug!(kind = %other, "ignoring an unhandled dsh operation"),
        }
    }

    fn apply_notification(
        &mut self,
        method: &str,
        params: &Value,
        frames: &mut Vec<ClientFrame>,
        refresh: &mut Refresh,
    ) {
        match method {
            "timeline.itemUpsert" => self.apply_item(params, frames),
            "notice.upsert" => self.apply_notice(params, frames),
            "session.state.updated" => {
                if let Some(state) = self.own_state(params) {
                    if let Some(frame) = self.transition(state) {
                        frames.push(frame);
                    }
                }
            }
            "session.turnEnded" => {
                if self.is_ours(params) {
                    if let Some(frame) = self.transition(SessionState::Idle) {
                        frames.push(frame);
                    }
                }
            }
            "session.capability.updated" | "runtime.capability.updated" => {
                refresh.capabilities = true;
            }
            "catalog.model.update" | "catalog.permission.update" => {
                refresh.catalogs = true;
                refresh.capabilities = true;
            }
            "session.meta.upsert"
            | "session.source.updated"
            | "session.inventory.begin"
            | "session.inventory.complete" => {}
            other => tracing::debug!(method = %other, "ignoring an unhandled dsh notification"),
        }
    }

    fn apply_item(&mut self, params: &Value, frames: &mut Vec<ClientFrame>) {
        let item = params.get("item").unwrap_or(params);
        if !self.is_ours(item) {
            return;
        }
        let now = timeutil::utc_now();
        if let Some(envelope) = projection::item(item, &now) {
            frames.push(ClientFrame::ItemUpsert {
                stream_id: self.stream_id.clone(),
                item: envelope,
            });
        }
    }

    fn apply_notice(&mut self, params: &Value, frames: &mut Vec<ClientFrame>) {
        let notice = params.get("notice").unwrap_or(params);
        if !self.is_ours(notice) {
            return;
        }
        let status = projection::notice_status(notice).to_string();
        let notice_id = notice
            .get("noticeId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if notice_id.is_empty() {
            tracing::warn!("dsh notice without a noticeId");
            return;
        }
        match status.as_str() {
            "open" | "responding" => {
                let Some(envelope) =
                    projection::notice(notice, &self.features, &self.session.session_id)
                else {
                    return;
                };
                self.notices.track(notice);
                if notice
                    .get("responseRequired")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    if let Some(frame) = self.transition(SessionState::WaitingApproval) {
                        frames.push(frame);
                    }
                }
                frames.push(ClientFrame::NoticeUpsert {
                    stream_id: self.stream_id.clone(),
                    notice: envelope,
                });
            }
            _ => {
                // Closed on the bridge: it may be our own answer (already reported,
                // and therefore no longer queued) or somebody else's. An unknown
                // transition is reported without inventing a decision.
                if !self.notices.contains(&notice_id) {
                    return;
                }
                self.notices.pop(&notice_id);
                frames.push(ClientFrame::NoticeResolved {
                    notice_id,
                    decision: None,
                    action_id: None,
                });
            }
        }
    }

    // --------------------------------------------------------------- commands

    async fn on_command(
        &mut self,
        client: &DshClient,
        command: SessionCmd,
    ) -> Result<(), ExitReport> {
        match command {
            SessionCmd::Message {
                text,
                client_message_id,
            } => {
                let client_message_id = self.messages.resolve(Some(&client_message_id), &text);
                let result = if self.state == SessionState::Running {
                    client.steer(&self.session, &text, &client_message_id).await
                } else {
                    client
                        .start_turn(&self.session, &text, &client_message_id)
                        .await
                };
                match result {
                    Ok(_) => {
                        self.set_state(SessionState::Running);
                        Ok(())
                    }
                    Err(error) => self.report("session message", error),
                }
            }
            SessionCmd::Interrupt => match client.interrupt(&self.session).await {
                Ok(_) => Ok(()),
                Err(error) => self.report("session.interrupt", error),
            },
            SessionCmd::Steer { text } => {
                let client_message_id = self.messages.resolve(None, &text);
                match client.steer(&self.session, &text, &client_message_id).await {
                    Ok(_) => Ok(()),
                    Err(error) => self.report("session.steer", error),
                }
            }
            SessionCmd::Selections {
                model,
                permission_mode,
            } => {
                if !projection::capability_flags(&self.capabilities, &self.catalogs).selections {
                    self.send(ClientFrame::Error {
                        message: String::from(
                            "this DSH session has no model or permission catalog; the selection was not applied",
                        ),
                        stream_id: Some(self.stream_id.clone()),
                    });
                    return Ok(());
                }
                let mut selections = serde_json::Map::new();
                if let Some(model) = model {
                    selections.insert(String::from("model"), json!(model));
                }
                if let Some(permission) = permission_mode {
                    selections.insert(String::from("permission"), json!(permission));
                }
                match client
                    .update_selections(&self.session, Value::Object(selections))
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(error) => self.report("session.updateSelections", error),
                }
            }
            SessionCmd::NoticeDecision {
                notice_id,
                decision,
                action_id,
                input,
            } => {
                self.respond(client, &notice_id, decision, action_id, input)
                    .await
            }
        }
    }

    async fn respond(
        &mut self,
        client: &DshClient,
        notice_id: &str,
        decision: Option<Decision>,
        action_id: Option<String>,
        input: Option<Value>,
    ) -> Result<(), ExitReport> {
        let Some(interaction) = self.notices.pop(notice_id) else {
            // The hub broadcasts notice.respond to every session of a connector,
            // because the frame carries no streamId, so another session's answer
            // arrives here routinely. The durable notice row (and its 409/422) is
            // the hub's; an error frame here would be a false alarm on a healthy
            // multi-session device.
            tracing::debug!(%notice_id, "notice.respond for a notice this session does not hold");
            return Ok(());
        };
        let action = match interaction::action_for(&interaction, decision, action_id.as_deref()) {
            Ok(action) => action,
            Err(error) => {
                self.notices.restore(interaction);
                self.send(ClientFrame::Error {
                    message: error.to_string(),
                    stream_id: Some(self.stream_id.clone()),
                });
                return Ok(());
            }
        };
        match client
            .respond_interaction(&self.session, notice_id, &action, input)
            .await
        {
            Ok(result) if result.get("ok").and_then(Value::as_bool) == Some(false) => {
                // A stale answer comes back as a business result, not an RPC error.
                // Reporting success would grey out a button whose decision nobody
                // applied.
                let message = result
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the bridge refused the answer");
                tracing::warn!(notice_id, %message, "dsh interaction refused");
                self.notices.restore(interaction);
                self.send(ClientFrame::Error {
                    message: format!("DSH refused the answer: {message}"),
                    stream_id: Some(self.stream_id.clone()),
                });
                Ok(())
            }
            Ok(_) => {
                self.send(ClientFrame::NoticeResolved {
                    notice_id: notice_id.to_string(),
                    decision: projection::decision_for_action(&action),
                    action_id: Some(action),
                });
                Ok(())
            }
            Err(error) => {
                self.notices.restore(interaction);
                self.send(ClientFrame::Error {
                    message: format!("session.respondInteraction failed: {}", error.message()),
                    stream_id: Some(self.stream_id.clone()),
                });
                Ok(())
            }
        }
    }

    // ----------------------------------------------------------------- plumbing

    async fn subscribe(&mut self, client: &DshClient) -> Result<(), ExitReport> {
        match client
            .subscribe(CHECKPOINT_VERSION, UPLOAD_PROGRESS_VERSION)
            .await
        {
            Ok(subscription) => {
                tracing::info!(
                    stream_id = %self.stream_id,
                    sync_stream = %subscription.stream_id,
                    projection = subscription.projection_version,
                    checkpoint = ?subscription.checkpoint_version,
                    "subscribed to the dsh event stream"
                );
                self.sync = Some(SyncStream::new(
                    subscription.stream_id,
                    subscription.projection_version,
                ));
                Ok(())
            }
            Err(error) => Err(self.fail(
                1,
                format!("runtime.sync.subscribe failed: {}", error.message()),
            )),
        }
    }

    async fn resubscribe(&mut self, client: &DshClient) -> Result<(), ExitReport> {
        self.snapshots = SnapshotStage::new();
        self.sync = None;
        self.subscribe(client).await
    }

    async fn ack(
        &mut self,
        client: &DshClient,
        batch_seq: u64,
        checkpoint: Option<Value>,
    ) -> Result<(), ExitReport> {
        let Some(stream_id) = self
            .sync
            .as_ref()
            .map(|stream| stream.stream_id().to_string())
        else {
            return Ok(());
        };
        if let Err(error) = client.ack(&stream_id, batch_seq, checkpoint).await {
            tracing::warn!(%error, "dsh ack failed; resubscribing");
            return self.resubscribe(client).await;
        }
        Ok(())
    }

    async fn refresh_catalogs(&mut self, client: &DshClient) {
        let models = client.list_models().await.unwrap_or_else(|error| {
            // A failed catalog read must not disable messaging: the picker offers
            // nothing, which is what we actually know.
            tracing::warn!(%error, "catalog.listModels failed");
            Vec::new()
        });
        let permissions = client.list_permissions().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "catalog.listPermissions failed");
            Vec::new()
        });
        self.catalogs = projection::catalogs(&models, &permissions);
    }

    async fn refresh_capabilities(&mut self, client: &DshClient) {
        match client.get_capabilities(&self.session).await {
            Ok(capabilities) => self.capabilities = capabilities,
            Err(error) => {
                tracing::warn!(%error, "session.getCapabilities failed; claiming nothing");
                self.capabilities = CapabilitySet::default();
            }
        }
    }

    fn send_capabilities(&self) {
        self.send(capabilities_frame(
            &self.stream_id,
            &self.capabilities,
            &self.catalogs,
        ));
    }

    fn transition(&mut self, state: SessionState) -> Option<ClientFrame> {
        if self.state == state {
            return None;
        }
        self.state = state;
        Some(ClientFrame::SessionStateFrame {
            stream_id: self.stream_id.clone(),
            state,
        })
    }

    fn set_state(&mut self, state: SessionState) {
        if let Some(frame) = self.transition(state) {
            self.send(frame);
        }
    }

    fn flush(&self, frames: Vec<ClientFrame>) {
        for frame in frames {
            self.send(frame);
        }
    }

    fn send(&self, frame: ClientFrame) {
        if self.out.send(frame).is_err() {
            tracing::debug!(stream_id = %self.stream_id, "hub channel closed; dropping frame");
        }
    }

    /// One error frame, then either keep serving (a retryable failure) or end the
    /// stream (a rejection this session cannot recover from).
    fn report(&self, what: &str, error: RpcError) -> Result<(), ExitReport> {
        let message = format!("{what} failed: {}", error.message());
        if error.is_retryable() {
            tracing::warn!(%message, "dsh request failed; the session continues");
            self.send(ClientFrame::Error {
                message,
                stream_id: Some(self.stream_id.clone()),
            });
            return Ok(());
        }
        Err(self.fail(1, message))
    }

    fn fail(&self, code: i32, message: impl Into<String>) -> ExitReport {
        let message = message.into();
        tracing::error!(stream_id = %self.stream_id, %message, "dsh session failed");
        self.send(ClientFrame::Error {
            message: message.clone(),
            stream_id: Some(self.stream_id.clone()),
        });
        self.finish(code, message)
    }

    fn finish(&self, code: i32, reason: impl Into<String>) -> ExitReport {
        let reason = reason.into();
        self.send(ClientFrame::SessionExit {
            stream_id: self.stream_id.clone(),
            code,
        });
        ExitReport { code, reason }
    }

    fn is_ours(&self, value: &Value) -> bool {
        match value.get("sessionId").and_then(Value::as_str) {
            Some(session_id) => session_id == self.session.session_id,
            None => true,
        }
    }

    fn snapshot_matches(&self, snapshot_id: &str) -> bool {
        matches!(self.snapshots.open_snapshot_id(), Some(open) if open == snapshot_id)
    }

    fn own_state(&self, params: &Value) -> Option<SessionState> {
        if !self.is_ours(params) {
            return None;
        }
        let status = params.get("status").and_then(Value::as_str)?;
        Some(projection::session_state(status))
    }
}

enum Signal {
    Notification(Option<Inbound>),
    Command(Option<SessionCmd>),
}

fn external_session_id(result: &Value) -> Option<String> {
    let direct = result.get("externalSessionId").and_then(Value::as_str);
    let nested = result
        .get("session")
        .and_then(|session| session.get("externalSessionId"))
        .and_then(Value::as_str);
    direct
        .or(nested)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// `sessionNamespace` is the prefix the bridge uses for native session ids, so it
/// has to be stable per connector rather than random per process.
pub fn session_namespace(connector_id: &str) -> String {
    format!("farwork-{connector_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::sync::oneshot;

    type Reader = Lines<BufReader<OwnedReadHalf>>;

    fn options(port: u16) -> DshOptions {
        DshOptions {
            endpoint: Endpoint {
                host: String::from("127.0.0.1"),
                port,
                token: String::from("test-token"),
                pid: Some(1),
            },
            connector_id: String::from("conn_1"),
            session_namespace: String::from("farwork-conn_1"),
            stream_id: String::from("str_1"),
            hub_session_id: String::from("sess_hub_1"),
            cwd: String::from("/tmp"),
            prompt: String::from("hello dsh"),
            client_version: String::from("0.1.0"),
            selections: None,
            agent_preset: None,
            // A test must never write into the developer's real state directory.
            checkpoint_dir: None,
        }
    }

    async fn next_frame(lines: &mut Reader) -> Option<Value> {
        loop {
            let line = lines.next_line().await.ok()??;
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(&line).ok();
        }
    }

    async fn reply(writer: &mut OwnedWriteHalf, id: Value, result: Value) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write");
    }

    async fn notify(writer: &mut OwnedWriteHalf, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write");
    }

    fn initialize_result() -> Value {
        json!({
            "identity": {
                "runtime": "dsh", "runtimeVersion": "0.2.1-rc.2",
                "bridgeVersion": "2.1.0", "protocolVersion": "1.0",
                "displayName": "DeepSeek Harness",
            },
            "storage": { "mode": "dsh-native", "sameSessionWriterLimit": 1, "crossProcessWriterExclusion": false },
            "features": {
                "attachments": true, "sessionDiscovery": true, "timelineSuffixRead": false,
                "approval": true, "userQuestions": true, "readOnly": false,
                "snapshotPagination": true, "syncMode": "events", "projectionVersion": 3,
            },
        })
    }

    fn approval_notice() -> Value {
        json!({
            "noticeId": "dsh_notice_1",
            "sessionId": "sess_hub_1",
            "runtime": "dsh",
            "type": "interaction",
            "interactionType": "approval",
            "title": "请求批准：shell",
            "message": "此操作需要你的批准。",
            "severity": "warning",
            "status": "open",
            "revision": 1,
            "responseRequired": true,
            "context": { "toolName": "shell" },
            "actions": [
                { "actionId": "allow-once", "label": "允许一次", "style": "primary" },
                { "actionId": "reject", "label": "拒绝", "style": "secondary" },
            ],
        })
    }

    /// The second batch skips a sequence number: the driver must throw its
    /// subscription away and ask for a fresh one instead of carrying on.
    #[tokio::test]
    async fn an_out_of_order_batch_forces_a_fresh_subscription() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let bridge = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut subscribes = 0u32;
            let mut acks: Vec<u64> = Vec::new();
            while let Some(frame) = next_frame(&mut lines).await {
                let id = frame["id"].clone();
                match frame["method"].as_str().unwrap_or_default() {
                    "initialize" => reply(&mut write, id, initialize_result()).await,
                    "session.createAndStart" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "sessionId": "sess_hub_1", "externalSessionId": "ext-1" }),
                        )
                        .await
                    }
                    "catalog.listModels" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "models": [] }),
                        )
                        .await
                    }
                    "catalog.listPermissions" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "permissions": [] }),
                        )
                        .await
                    }
                    "session.getCapabilities" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "capabilities": [] }),
                        )
                        .await
                    }
                    "runtime.sync.subscribe" => {
                        subscribes += 1;
                        reply(
                            &mut write,
                            id,
                            json!({ "streamId": format!("sync_{subscribes}"), "projectionVersion": 3 }),
                        )
                        .await;
                        notify(
                            &mut write,
                            "runtime.sync.batch",
                            json!({ "streamId": format!("sync_{subscribes}"), "batchSeq": 1,
                                "projectionVersion": 3,
                                "operations": [{ "kind": "notifications", "notifications": [] }] }),
                        )
                        .await;
                        notify(
                            &mut write,
                            "runtime.sync.batch",
                            json!({ "streamId": format!("sync_{subscribes}"), "batchSeq": 3,
                                "projectionVersion": 3,
                                "operations": [{ "kind": "notifications", "notifications": [] }] }),
                        )
                        .await;
                    }
                    "runtime.sync.ack" => {
                        acks.push(frame["params"]["batchSeq"].as_u64().unwrap_or(0));
                        reply(&mut write, id, json!({ "ok": true })).await;
                    }
                    _ => {}
                }
                if subscribes >= 2 {
                    break;
                }
            }
            (subscribes, acks)
        });

        let (out_tx, mut out_rx) = unbounded_channel();
        let (_cmd_tx, cmd_rx) = unbounded_channel();
        let session = DshSession::new(options(port), out_tx, cmd_rx);
        let run = tokio::spawn(async move { session.run().await });

        let (subscribes, acks) = tokio::time::timeout(Duration::from_secs(20), bridge)
            .await
            .expect("bridge timed out")
            .expect("bridge task");
        assert_eq!(subscribes, 2, "a gap must be answered with a resubscribe");
        assert_eq!(acks, vec![1], "the rejected batch must never be acked");

        // Drain what the driver told the hub: started, capabilities, state, no exit.
        let mut kinds = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_millis(200), out_rx.recv()).await
        {
            kinds.push(frame_type(&frame));
        }
        assert!(kinds.contains(&String::from("session.started")));
        assert!(kinds.contains(&String::from("session.capabilities")));
        run.abort();
    }

    /// The whole approval path: a notice with the runtime's own action ids goes
    /// out, the browser's action id comes back, and the answer is the id the
    /// bridge was asked to apply.
    #[tokio::test]
    async fn a_notice_round_trip_uses_the_runtimes_own_action_ids() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (responded_tx, responded_rx) = oneshot::channel();
        let bridge = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut responded = Some(responded_tx);
            while let Some(frame) = next_frame(&mut lines).await {
                let id = frame["id"].clone();
                match frame["method"].as_str().unwrap_or_default() {
                    "initialize" => reply(&mut write, id, initialize_result()).await,
                    "session.createAndStart" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "sessionId": "sess_hub_1", "externalSessionId": "ext-1" }),
                        )
                        .await
                    }
                    "catalog.listModels" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1,
                                "models": [{ "id": "dsh:model:a", "title": "DeepSeek-V4-Pro",
                                    "selectionId": "dsh:model:a", "enabled": true, "reasoningItems": [] }] }),
                        )
                        .await
                    }
                    "catalog.listPermissions" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1,
                                "permissions": [{ "id": "dsh:permission:y", "title": "workspace-write",
                                    "selectionId": "dsh:permission:y", "enabled": true }] }),
                        )
                        .await
                    }
                    "session.getCapabilities" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "capabilities": [
                                { "capabilityId": "session.interrupt", "scope": "session", "runtime": "dsh",
                                  "supported": true, "available": true, "allowed": true },
                                { "capabilityId": "catalog.model", "scope": "runtime", "runtime": "dsh",
                                  "supported": true, "available": true, "allowed": true },
                            ] }),
                        )
                        .await
                    }
                    "runtime.sync.subscribe" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "streamId": "sync_1", "projectionVersion": 3, "checkpointVersion": 1 }),
                        )
                        .await;
                        notify(
                            &mut write,
                            "runtime.sync.batch",
                            json!({ "streamId": "sync_1", "batchSeq": 1, "projectionVersion": 3,
                                "operations": [{ "kind": "notifications", "notifications": [
                                    { "method": "timeline.itemUpsert", "params": { "item": {
                                        "id": "dsh_item_1", "sessionId": "sess_hub_1", "type": "message",
                                        "status": "done", "role": "assistant",
                                        "content": { "text": "hello from dsh" } } } },
                                    { "method": "notice.upsert", "params": { "notice": approval_notice() } },
                                ] }] }),
                        )
                        .await;
                    }
                    "runtime.sync.ack" => {
                        reply(&mut write, id, json!({ "ok": true })).await;
                    }
                    "session.respondInteraction" => {
                        if let Some(sender) = responded.take() {
                            let _ = sender.send(frame["params"].clone());
                        }
                        reply(&mut write, id, json!({ "ok": true, "result": { "resolved": true } })).await;
                    }
                    _ => {}
                }
            }
        });

        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = DshSession::new(options(port), out_tx, cmd_rx);
        let run = tokio::spawn(async move { session.run().await });

        // Collect until the notice lands; the item may arrive before or after it.
        let mut notice_id = None;
        let mut options_seen: Vec<crate::protocol::NoticeOption> = Vec::new();
        let mut saw_item = false;
        for _ in 0..12 {
            let frame = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
                .await
                .expect("frame timed out")
                .expect("channel closed");
            match &frame {
                ClientFrame::NoticeUpsert { notice, .. } => {
                    notice_id = Some(notice.id.clone());
                    options_seen = notice.options.clone();
                    assert_eq!(notice.kind, crate::protocol::NoticeKind::Interaction);
                    assert_eq!(notice.interaction_type.as_deref(), Some("approval"));
                    break;
                }
                ClientFrame::ItemUpsert { .. } => saw_item = true,
                other => {
                    let _ = other;
                }
            }
        }
        let notice_id = notice_id.expect("no notice was forwarded");
        assert_eq!(notice_id, "dsh_notice_1");
        assert_eq!(
            options_seen
                .iter()
                .map(|option| option.id().to_string())
                .collect::<Vec<_>>(),
            vec!["allow-once", "reject"]
        );

        cmd_tx
            .send(SessionCmd::NoticeDecision {
                notice_id: notice_id.clone(),
                decision: None,
                action_id: Some(String::from("allow-once")),
                input: None,
            })
            .expect("send command");

        let mut resolved = None;
        for _ in 0..12 {
            let frame = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
                .await
                .expect("frame timed out")
                .expect("channel closed");
            if let ClientFrame::NoticeResolved {
                notice_id,
                decision,
                action_id,
            } = frame
            {
                resolved = Some((notice_id, decision, action_id));
                break;
            }
        }
        let (resolved_id, decision, action_id) = resolved.expect("no notice.resolved");
        assert_eq!(resolved_id, "dsh_notice_1");
        assert_eq!(decision, Some(Decision::Approve));
        assert_eq!(action_id.as_deref(), Some("allow-once"));

        let params = tokio::time::timeout(Duration::from_secs(5), responded_rx)
            .await
            .expect("no respondInteraction")
            .expect("channel");
        assert_eq!(params["noticeId"], json!("dsh_notice_1"));
        assert_eq!(params["actionId"], json!("allow-once"));
        assert_eq!(params["sessionId"], json!("sess_hub_1"));
        assert_eq!(params["externalSessionId"], json!("ext-1"));

        // The item really was forwarded too: a notice without the turn would be a
        // strange half of the contract to get right.
        assert!(
            saw_item,
            "the timeline item from the same batch must be forwarded"
        );
        run.abort();
        bridge.abort();
    }

    /// Capture the `createAndStart` params a session actually sends, then stop it.
    async fn sent_create_and_start_params(
        selections: Option<Value>,
        agent_preset: Option<&str>,
    ) -> Value {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (captured_tx, captured_rx) = oneshot::channel();
        let bridge = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut captured = Some(captured_tx);
            while let Some(frame) = next_frame(&mut lines).await {
                let id = frame["id"].clone();
                match frame["method"].as_str().unwrap_or_default() {
                    "initialize" => reply(&mut write, id, initialize_result()).await,
                    "session.createAndStart" => {
                        if let Some(captured) = captured.take() {
                            let _ = captured.send(frame["params"].clone());
                        }
                        reply(
                            &mut write,
                            id,
                            json!({ "sessionId": "sess_hub_1", "externalSessionId": "ext-1" }),
                        )
                        .await
                    }
                    _ => {}
                }
            }
        });
        let mut options = options(port);
        options.selections = selections;
        options.agent_preset = agent_preset.map(str::to_string);
        let (out_tx, _out_rx) = unbounded_channel();
        let (_cmd_tx, cmd_rx) = unbounded_channel();
        let session = DshSession::new(options, out_tx, cmd_rx);
        let run = tokio::spawn(async move { session.run().await });
        let params = tokio::time::timeout(Duration::from_secs(10), captured_rx)
            .await
            .expect("createAndStart timed out")
            .expect("channel");
        run.abort();
        bridge.abort();
        params
    }

    #[tokio::test]
    async fn a_configured_permission_reaches_create_and_start() {
        let selections = selections::Selections {
            permission: Some(String::from("dsh:permission:cmVhZC1vbmx5")),
            model: None,
            preset: None,
        };
        let params = sent_create_and_start_params(selections.to_value(), None).await;
        assert_eq!(
            params["selections"]["permission"],
            json!("dsh:permission:cmVhZC1vbmx5")
        );
        assert!(
            params["selections"].get("model").is_none(),
            "a model the operator did not name must not be overridden: {params}"
        );
    }

    #[tokio::test]
    async fn an_unspecified_selection_leaves_the_key_out_of_create_and_start() {
        let params = sent_create_and_start_params(None, None).await;
        assert!(
            params.get("selections").is_none(),
            "DSH's own default must win, not an empty object: {params}"
        );
        assert!(
            params.get("agentPreset").is_none(),
            "an unconfigured Agent mode must not send the key: {params}"
        );
    }

    #[tokio::test]
    async fn a_configured_agent_preset_reaches_create_and_start() {
        let params = sent_create_and_start_params(None, Some("ptc")).await;
        assert_eq!(params["agentPreset"], json!("ptc"));
        assert!(
            params.get("selections").is_none(),
            "the Agent mode is not a selection: {params}"
        );
    }

    fn frame_type(frame: &ClientFrame) -> String {
        match frame {
            ClientFrame::Hello { .. } => "hello",
            ClientFrame::Heartbeat { .. } => "heartbeat",
            ClientFrame::SessionStarted { .. } => "session.started",
            ClientFrame::SessionStateFrame { .. } => "session.state",
            ClientFrame::SessionExit { .. } => "session.exit",
            ClientFrame::SessionCapabilities { .. } => "session.capabilities",
            ClientFrame::ItemUpsert { .. } => "item.upsert",
            ClientFrame::NoticeUpsert { .. } => "notice.upsert",
            ClientFrame::NoticeResolved { .. } => "notice.resolved",
            ClientFrame::FsResponse(_) => "fs.response",
            ClientFrame::TerminalOpened { .. } => "terminal.opened",
            ClientFrame::TerminalData { .. } => "terminal.data",
            ClientFrame::TerminalExit { .. } => "terminal.exit",
            ClientFrame::TerminalError { .. } => "terminal.error",
            ClientFrame::Error { .. } => "error",
        }
        .to_string()
    }

    #[test]
    fn the_session_namespace_is_stable_per_connector() {
        assert_eq!(session_namespace("conn_1"), "farwork-conn_1");
        assert_ne!(session_namespace("conn_1"), session_namespace("conn_2"));
    }

    #[test]
    fn an_absent_external_session_id_falls_back_to_the_hub_session() {
        assert_eq!(
            external_session_id(&json!({ "externalSessionId": "ext-1" })),
            Some(String::from("ext-1"))
        );
        assert_eq!(
            external_session_id(&json!({ "session": { "externalSessionId": "ext-2" } })),
            Some(String::from("ext-2"))
        );
        assert_eq!(
            external_session_id(&json!({ "externalSessionId": "  " })),
            None
        );
        assert_eq!(external_session_id(&json!({})), None);
    }

    /// The durable half of the sync contract, over a real socket: the bridge asks
    /// for a checkpoint on the ack, we answer from disk, and a `checkpoint.save`
    /// lands on disk before the ack goes back.
    ///
    /// This is the fix for a subscribe that replays every session in full: without
    /// it the answer is always "none" and the bridge starts over every time.
    #[tokio::test]
    async fn a_bridge_checkpoint_is_read_from_disk_and_written_back() {
        let dir = std::env::temp_dir().join(format!(
            "farwork-agent-dsh-checkpoints-{}",
            uuid::Uuid::new_v4().simple()
        ));
        // What an earlier connection stored: the answer this one has to give.
        let mut earlier = Checkpoints::open(&dir);
        earlier.save("ext-persisted", json!({"throughSeq": 42, "version": 1}));
        drop(earlier);

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let bridge = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut acks: Vec<Value> = Vec::new();
            let mut subscribe_params: Option<Value> = None;
            while let Some(frame) = next_frame(&mut lines).await {
                let id = frame["id"].clone();
                match frame["method"].as_str().unwrap_or_default() {
                    "initialize" => reply(&mut write, id, initialize_result()).await,
                    "session.createAndStart" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "sessionId": "sess_hub_1", "externalSessionId": "ext-1" }),
                        )
                        .await
                    }
                    "catalog.listModels" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "models": [] }),
                        )
                        .await
                    }
                    "catalog.listPermissions" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "permissions": [] }),
                        )
                        .await
                    }
                    "session.getCapabilities" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "capabilities": [] }),
                        )
                        .await
                    }
                    "runtime.sync.subscribe" => {
                        subscribe_params = Some(frame["params"].clone());
                        reply(
                            &mut write,
                            id,
                            json!({ "streamId": "sync_1", "projectionVersion": 3, "checkpointVersion": 1 }),
                        )
                        .await;
                        for (batch_seq, operations) in [
                            (
                                1u64,
                                json!([{ "kind": "checkpoint.load", "externalSessionId": "ext-persisted" }]),
                            ),
                            (
                                2u64,
                                json!([{ "kind": "checkpoint.save", "externalSessionId": "ext-new",
                                    "checkpoint": { "throughSeq": 99, "version": 1 } }]),
                            ),
                            (
                                3u64,
                                json!([{ "kind": "checkpoint.load", "externalSessionId": "ext-never-seen" }]),
                            ),
                            (
                                4u64,
                                json!([{ "kind": "checkpoint.delete", "externalSessionId": "ext-persisted" }]),
                            ),
                        ] {
                            notify(&mut write, "runtime.sync.batch", json!({
                                "streamId": "sync_1", "batchSeq": batch_seq, "projectionVersion": 3,
                                "operations": operations,
                            }))
                            .await;
                        }
                    }
                    "runtime.sync.ack" => {
                        acks.push(frame["params"].clone());
                        reply(&mut write, id, json!({ "ok": true })).await;
                        if acks.len() >= 4 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (acks, subscribe_params)
        });

        let mut options = options(port);
        options.checkpoint_dir = Some(dir.clone());
        let (out_tx, _out_rx) = unbounded_channel();
        let (_cmd_tx, cmd_rx) = unbounded_channel();
        let session = DshSession::new(options, out_tx, cmd_rx);
        let run = tokio::spawn(async move { session.run().await });

        let (acks, subscribe_params) = tokio::time::timeout(Duration::from_secs(20), bridge)
            .await
            .expect("bridge timed out")
            .expect("bridge task");
        assert_eq!(acks.len(), 4, "every batch must be acked: {acks:?}");

        let subscribe_params = subscribe_params.expect("no subscribe");
        assert_eq!(subscribe_params["checkpointVersion"], json!(1));
        let subscription = subscribe_params["subscriptionId"]
            .as_str()
            .expect("subscriptionId is what scopes the stream");
        assert_eq!(subscription.len(), 32, "{subscription}");

        // 1: the stored checkpoint comes back on the ack.
        assert_eq!(acks[0]["batchSeq"], json!(1));
        assert_eq!(
            acks[0]["checkpoint"],
            json!({ "throughSeq": 42, "version": 1 })
        );
        // 2: a save answers nothing, and it is already on disk.
        assert!(acks[1].get("checkpoint").is_none(), "{}", acks[1]);
        let saved = std::fs::read_to_string(dir.join("ext-new.json")).expect("saved file");
        let saved: Value = serde_json::from_str(&saved).expect("json");
        assert_eq!(saved, json!({ "throughSeq": 99, "version": 1 }));
        // 3: a session we know nothing about is answered with an explicit null —
        // an omitted key would read as "this extension is not implemented".
        assert_eq!(acks[2]["checkpoint"], Value::Null);
        // 4: a delete removes the file, so the next connection replays that one.
        assert!(!dir.join("ext-persisted.json").exists());
        let surviving = Checkpoints::open(&dir);
        assert_eq!(surviving.load("ext-persisted"), None);
        assert_eq!(surviving.load("ext-new"), Some(saved));

        run.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sync feed the bridge killed is not an unknown error: on this machine it is
    /// its own 10k-event queue overflowing during a full inventory replay. With no
    /// checkpoint to resume from, retrying would replay the same inventory and die
    /// again, so the session must say so once and stop.
    #[tokio::test]
    async fn a_dead_sync_feed_is_reported_with_the_one_action_that_fixes_it() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let bridge = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut subscribes = 0u32;
            while let Some(frame) = next_frame(&mut lines).await {
                let id = frame["id"].clone();
                match frame["method"].as_str().unwrap_or_default() {
                    "initialize" => reply(&mut write, id, initialize_result()).await,
                    "session.createAndStart" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "sessionId": "sess_hub_1", "externalSessionId": "ext-1" }),
                        )
                        .await
                    }
                    "catalog.listModels" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "models": [] }),
                        )
                        .await
                    }
                    "catalog.listPermissions" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "permissions": [] }),
                        )
                        .await
                    }
                    "session.getCapabilities" => {
                        reply(
                            &mut write,
                            id,
                            json!({ "runtime": "dsh", "revision": 1, "capabilities": [] }),
                        )
                        .await
                    }
                    "runtime.sync.subscribe" => {
                        subscribes += 1;
                        reply(
                            &mut write,
                            id,
                            json!({ "streamId": "sync_1", "projectionVersion": 3, "checkpointVersion": 1 }),
                        )
                        .await;
                        notify(
                            &mut write,
                            "runtime.error",
                            json!({
                                "code": -32603,
                                "message": "DSH could not complete this request. Check the Bridge logs page and retry.",
                                "data": { "code": "INTERNAL_ERROR", "retryable": true, "scope": "sync" },
                            }),
                        )
                        .await;
                    }
                    _ => {}
                }
            }
            subscribes
        });

        let mut options = options(port);
        // Nothing persisted: there is no checkpoint to resume from, so the only
        // honest answer is to report the cause instead of replaying again.
        options.checkpoint_dir = None;
        let (out_tx, mut out_rx) = unbounded_channel();
        let (_cmd_tx, cmd_rx) = unbounded_channel();
        let session = DshSession::new(options, out_tx, cmd_rx);
        let run = tokio::spawn(async move { session.run().await });

        let report = tokio::time::timeout(Duration::from_secs(20), run)
            .await
            .expect("the session must not retry forever")
            .expect("join");
        assert_eq!(report.code, 1);
        assert!(
            report.reason.contains("INTERNAL_ERROR"),
            "the bridge's own code belongs in the reason: {}",
            report.reason
        );
        assert!(
            report.reason.contains("Archive old sessions"),
            "an operator needs the action, not just the symptom: {}",
            report.reason
        );

        let mut saw_error = false;
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_millis(200), out_rx.recv()).await
        {
            if let ClientFrame::Error { message, .. } = &frame {
                assert!(message.contains("Archive old sessions"), "{message}");
                saw_error = true;
            }
        }
        assert!(saw_error, "the hub must hear why the stream ended");

        let subscribes = tokio::time::timeout(Duration::from_secs(5), bridge)
            .await
            .expect("bridge")
            .expect("task");
        assert_eq!(
            subscribes, 1,
            "a replay that cannot help must not be retried"
        );
    }
}
