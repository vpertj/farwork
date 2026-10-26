//! Pure mapping layer: codex app-server JSON-RPC frames -> Ingress Protocol v1.
//!
//! Everything here is IO-free so the whole event/approval state machine can be
//! driven by a scripted fake transport in unit tests. The driver in
//! 'session.rs' only moves bytes: each 'CodexOut' is either a frame for the
//! codex child or a frame for the hub.
//!
//! Method and field names are taken from the generated schemas
//! (/tmp/codex-schema, codex-cli 0.162.1), not from memory:
//!   ClientRequest.json            -> initialize, thread/start, turn/start, turn/interrupt
//!   ClientNotification.json       -> initialized
//!   ServerNotification.json       -> item/started, item/completed, item/agentMessage/delta,
//!                                    item/commandExecution/outputDelta, item/fileChange/patchUpdated,
//!                                    turn/started, turn/completed, error, serverRequest/resolved
//!   ServerRequest.json            -> item/commandExecution/requestApproval,
//!                                    item/fileChange/requestApproval, item/permissions/requestApproval,
//!                                    execCommandApproval, applyPatchApproval
//!   ThreadItem.json (v2 bundle)   -> item variants mapped to timeline item types

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::ids;
use crate::protocol::{
    ClientFrame, Decision, ItemEnvelope, ItemStatus, ItemType, NoticeEnvelope, NoticeKind, Role,
    SessionState,
};
use crate::rpc::{self, RpcId, RpcMessage};
use crate::timeutil;

/// Characters of streamed text that may accumulate before another timeline
/// upsert is worth a row update. Small enough to look live, large enough that a
/// token-at-a-time stream does not hammer the hub.
pub const DEFAULT_FLUSH_CHARS: usize = 256;
/// Streamed command output kept in the item payload; the tail is dropped and
/// flagged so a runaway build log cannot bloat a timeline row.
pub const DEFAULT_OUTPUT_LIMIT: usize = 20_000;
/// A pending approval blocks the turn; after this long the agent cancels it and
/// says so instead of hanging the session forever.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct MapperConfig {
    pub flush_chars: usize,
    pub output_limit: usize,
    pub approval_timeout: Duration,
    pub approval_policy: String,
    pub sandbox: String,
    pub client_name: String,
    pub client_version: String,
}

impl Default for MapperConfig {
    fn default() -> Self {
        Self {
            flush_chars: DEFAULT_FLUSH_CHARS,
            output_limit: DEFAULT_OUTPUT_LIMIT,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            // Ask the user rather than silently escalating: the approval round
            // trip is the point of this milestone.
            approval_policy: String::from("on-request"),
            sandbox: String::from("workspace-write"),
            client_name: String::from("farwork-agent"),
            client_version: String::from(env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSeed {
    pub stream_id: String,
    pub session_id: String,
    pub cwd: String,
    pub prompt: String,
}

/// One unit of work produced by the mapper.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexOut {
    /// Ready-to-wire JSON-RPC frame for the codex child.
    Rpc(Value),
    /// Ingress frame for the hub.
    Hub(ClientFrame),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestPurpose {
    Initialize,
    ThreadStart,
    TurnStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalFamily {
    /// v2 method, responds with CommandExecutionApprovalDecision.
    CommandExecutionV2,
    /// v2 method, responds with FileChangeApprovalDecision.
    FileChangeV2,
    /// v1 methods, respond with ReviewDecision.
    Legacy,
}

#[derive(Debug, Clone)]
struct PendingApproval {
    notice_id: String,
    rpc_id: RpcId,
    family: ApprovalFamily,
    item_id: Option<String>,
    deadline: Instant,
}

#[derive(Debug, Clone)]
struct ItemTrack {
    local_id: String,
    item_type: ItemType,
    role: Role,
    content: Value,
    status: ItemStatus,
    /// Length of the streamed field (text/output) as last sent to the hub.
    sent_len: usize,
}

impl ItemTrack {
    fn streamed_len(&self) -> usize {
        ["text", "output"]
            .iter()
            .filter_map(|key| self.content.get(key).and_then(Value::as_str))
            .map(|text| text.chars().count())
            .sum()
    }
}

pub struct CodexMapper {
    cfg: MapperConfig,
    stream_id: String,
    session_id: String,
    cwd: String,
    initial_prompt: String,
    next_id: i64,
    inflight: BTreeMap<i64, RequestPurpose>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    state: SessionState,
    started_reported: bool,
    items: BTreeMap<String, ItemTrack>,
    pending: Vec<PendingApproval>,
}

impl CodexMapper {
    pub fn new(seed: SessionSeed, cfg: MapperConfig) -> Self {
        Self {
            cfg,
            stream_id: seed.stream_id,
            session_id: seed.session_id,
            cwd: seed.cwd,
            initial_prompt: seed.prompt,
            next_id: 1,
            inflight: BTreeMap::new(),
            thread_id: None,
            turn_id: None,
            state: SessionState::Idle,
            started_reported: false,
            items: BTreeMap::new(),
            pending: Vec::new(),
        }
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pending_notices(&self) -> usize {
        self.pending.len()
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Kick off the handshake: initialize must land before anything else.
    pub fn begin(&mut self) -> Vec<CodexOut> {
        let mut outs = Vec::new();
        if let Some(frame) = self.set_state(SessionState::Running) {
            outs.push(frame);
        }
        outs.push(self.request(
            RequestPurpose::Initialize,
            "initialize",
            json!({"clientInfo": {
                "name": self.cfg.client_name,
                "version": self.cfg.client_version,
            }}),
        ));
        outs
    }

    /// A subsequent user message becomes a new turn on the same thread.
    pub fn on_user_message(&mut self, text: &str, client_message_id: &str) -> Vec<CodexOut> {
        let Some(thread_id) = self.thread_id.clone() else {
            return vec![self.error_frame(String::from(
                "session.message received before the codex thread was ready",
            ))];
        };
        vec![self.request(
            RequestPurpose::TurnStart,
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": text}],
                "clientUserMessageId": client_message_id,
            }),
        )]
    }

    pub fn on_interrupt(&mut self) -> Vec<CodexOut> {
        let mut outs = Vec::new();
        if let (Some(thread_id), Some(turn_id)) = (self.thread_id.clone(), self.turn_id.clone()) {
            outs.push(CodexOut::Rpc(rpc::request(
                self.take_id(),
                "turn/interrupt",
                json!({"threadId": thread_id, "turnId": turn_id}),
            )));
        }
        outs.extend(self.resolve_all(Decision::Reject));
        outs
    }

    /// Hub answered a notice. Answer codex in its own vocabulary.
    pub fn on_notice_decision(&mut self, notice_id: &str, decision: Decision) -> Vec<CodexOut> {
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.notice_id == notice_id)
        else {
            // notice.respond carries no streamId, so the connector fans it out to
            // every live session; not owning the notice is routine, not an error.
            tracing::debug!(
                target: "farwork_agent::codex",
                %notice_id,
                stream_id = %self.stream_id,
                "notice.respond for a notice this session does not own"
            );
            return Vec::new();
        };
        let pending = self.pending.remove(index);
        let mut outs = Vec::new();
        if let Some(item_key) = pending.item_id.clone() {
            if let Some(track) = self.items.get_mut(&item_key) {
                track.status = match decision {
                    Decision::Reject => ItemStatus::Cancelled,
                    _ => ItemStatus::Running,
                };
            }
            outs.extend(self.emit_track(&item_key));
        }
        outs.push(CodexOut::Rpc(rpc::result(
            &pending.rpc_id,
            approval_result(pending.family, decision),
        )));
        outs.push(CodexOut::Hub(ClientFrame::NoticeResolved {
            notice_id: notice_id.to_owned(),
            decision,
        }));
        if let Some(frame) = self.set_state(SessionState::Running) {
            outs.push(frame);
        }
        outs
    }

    /// Cancel approvals that blew their deadline. The turn is unblocked either
    /// way, and the hub hears the real reason rather than a silent gap.
    pub fn expire_notices(&mut self, now: Instant) -> Vec<CodexOut> {
        let expired: Vec<PendingApproval> = self
            .pending
            .iter()
            .filter(|pending| pending.deadline <= now)
            .cloned()
            .collect();
        if expired.is_empty() {
            return Vec::new();
        }
        self.pending.retain(|pending| pending.deadline > now);
        let mut outs = Vec::new();
        for pending in expired {
            outs.push(CodexOut::Rpc(rpc::result(
                &pending.rpc_id,
                approval_result(pending.family, Decision::Reject),
            )));
            outs.push(CodexOut::Hub(ClientFrame::NoticeResolved {
                notice_id: pending.notice_id,
                decision: Decision::Reject,
            }));
        }
        outs.push(self.error_frame(String::from(
            "approval request timed out; cancelled and rejected",
        )));
        if let Some(frame) = self.set_state(SessionState::Running) {
            outs.push(frame);
        }
        outs
    }

    /// Frames to send when the hub connection dies: nothing is answered (the
    /// peer is gone), but every open notice is reported as rejected so the hub
    /// does not leave a zombie approval pending.
    pub fn on_disconnect(&mut self) -> Vec<CodexOut> {
        let mut outs = Vec::new();
        for pending in self.pending.drain(..) {
            outs.push(CodexOut::Hub(ClientFrame::NoticeResolved {
                notice_id: pending.notice_id,
                decision: Decision::Reject,
            }));
        }
        outs
    }

    pub fn exit_frame(&self, code: i32) -> ClientFrame {
        ClientFrame::SessionExit {
            stream_id: self.stream_id.clone(),
            code,
        }
    }

    /// Handle one inbound JSON-RPC frame from codex.
    pub fn on_rpc(&mut self, value: Value) -> Vec<CodexOut> {
        let Some(message) = RpcMessage::decode(value) else {
            return Vec::new();
        };
        match message {
            RpcMessage::Response { id, result } => self.on_response(&id, result),
            RpcMessage::Failure { id, error } => self.on_failure(&id, &error.message),
            RpcMessage::Notification { method, params } => self.on_notification(&method, params),
            RpcMessage::Request { id, method, params } => {
                self.on_server_request(id, &method, params)
            }
        }
    }

    fn on_response(&mut self, id: &RpcId, result: Value) -> Vec<CodexOut> {
        let Some(purpose) = id.as_i64().and_then(|id| self.inflight.remove(&id)) else {
            return Vec::new();
        };
        match purpose {
            RequestPurpose::Initialize => {
                let mut outs = vec![CodexOut::Rpc(rpc::notification("initialized", json!({})))];
                outs.push(self.request(
                    RequestPurpose::ThreadStart,
                    "thread/start",
                    json!({
                        "cwd": self.cwd,
                        "approvalPolicy": self.cfg.approval_policy,
                        "sandbox": self.cfg.sandbox,
                    }),
                ));
                outs
            }
            RequestPurpose::ThreadStart => {
                let Some(thread_id) = result
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                else {
                    return vec![
                        self.error_frame(String::from("codex thread/start returned no thread id"))
                    ];
                };
                self.thread_id = Some(thread_id.to_owned());
                let mut outs = Vec::new();
                if !self.started_reported {
                    self.started_reported = true;
                    outs.push(CodexOut::Hub(ClientFrame::SessionStarted {
                        stream_id: self.stream_id.clone(),
                        external_session_id: thread_id.to_owned(),
                    }));
                    // Immediately after session.started, in the same batch: the
                    // browser must learn what this runtime supports before it can
                    // render (or hide) the controls for it.
                    outs.push(CodexOut::Hub(crate::codex::capabilities_frame(
                        &self.stream_id,
                    )));
                }
                let prompt = self.initial_prompt.clone();
                if prompt.trim().is_empty() {
                    if let Some(frame) = self.set_state(SessionState::Idle) {
                        outs.push(frame);
                    }
                } else {
                    outs.push(self.request(
                        RequestPurpose::TurnStart,
                        "turn/start",
                        json!({
                            "threadId": thread_id,
                            "input": [{"type": "text", "text": prompt}],
                        }),
                    ));
                }
                outs
            }
            RequestPurpose::TurnStart => {
                if let Some(turn_id) = result
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                {
                    self.turn_id = Some(turn_id.to_owned());
                }
                self.set_state(SessionState::Running).into_iter().collect()
            }
        }
    }

    fn on_failure(&mut self, id: &RpcId, message: &str) -> Vec<CodexOut> {
        let purpose = id.as_i64().and_then(|id| self.inflight.remove(&id));
        let mut outs = vec![self.error_frame(format!("codex request failed: {message}"))];
        outs.extend(self.resolve_all(Decision::Reject));
        if let Some(frame) = self.set_state(SessionState::Error) {
            outs.push(frame);
        }
        if matches!(
            purpose,
            Some(RequestPurpose::Initialize | RequestPurpose::ThreadStart)
        ) {
            outs.push(CodexOut::Hub(self.exit_frame(1)));
        }
        outs
    }

    fn on_notification(&mut self, method: &str, params: Value) -> Vec<CodexOut> {
        match method {
            "thread/started" => {
                if self.thread_id.is_none() {
                    if let Some(thread_id) = params
                        .get("thread")
                        .and_then(|thread| thread.get("id"))
                        .and_then(Value::as_str)
                    {
                        self.thread_id = Some(thread_id.to_owned());
                    }
                }
                Vec::new()
            }
            "turn/started" => {
                if let Some(turn_id) = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                {
                    self.turn_id = Some(turn_id.to_owned());
                }
                self.set_state(SessionState::Running).into_iter().collect()
            }
            "turn/completed" => self.on_turn_completed(&params),
            "item/started" => match params.get("item") {
                Some(item) => self.on_item(item, true),
                None => Vec::new(),
            },
            "item/completed" => match params.get("item") {
                Some(item) => self.on_item(item, false),
                None => Vec::new(),
            },
            "item/agentMessage/delta" => {
                let id = params.get("itemId").and_then(Value::as_str);
                let delta = params.get("delta").and_then(Value::as_str);
                match (id, delta) {
                    (Some(id), Some(delta)) => {
                        self.on_delta(id, "text", delta, StreamKind::Message)
                    }
                    _ => Vec::new(),
                }
            }
            "item/commandExecution/outputDelta" => {
                let id = params.get("itemId").and_then(Value::as_str);
                let delta = params.get("delta").and_then(Value::as_str);
                match (id, delta) {
                    (Some(id), Some(delta)) => self.on_delta(id, "output", delta, StreamKind::Tool),
                    _ => Vec::new(),
                }
            }
            "item/fileChange/patchUpdated" => {
                let id = params.get("itemId").and_then(Value::as_str);
                let changes = params.get("changes").cloned();
                match (id, changes) {
                    (Some(id), Some(changes)) => {
                        let stream_id = self.stream_id.clone();
                        let track = self
                            .items
                            .entry(id.to_owned())
                            .or_insert_with(|| ItemTrack {
                                local_id: ids::item_id(),
                                item_type: ItemType::Artifact,
                                role: Role::Tool,
                                content: json!({"title": "File change"}),
                                status: ItemStatus::Running,
                                sent_len: 0,
                            });
                        track.content["changes"] = changes;
                        vec![Self::emit_item(track, &stream_id)]
                    }
                    _ => Vec::new(),
                }
            }
            "error" => self.on_error_notification(&params),
            "serverRequest/resolved" => {
                let Some(request_id) = params.get("requestId") else {
                    return Vec::new();
                };
                self.resolve_one(request_id)
            }
            _ => Vec::new(),
        }
    }

    fn on_turn_completed(&mut self, params: &Value) -> Vec<CodexOut> {
        let status = params
            .get("turn")
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("completed")
            .to_owned();
        let mut outs = self.resolve_all(Decision::Reject);
        if status == "interrupted" {
            outs.extend(self.close_running_items(ItemStatus::Interrupted));
        }
        let next_state = if status == "failed" {
            SessionState::Error
        } else {
            SessionState::Idle
        };
        if let Some(frame) = self.set_state(next_state) {
            outs.push(frame);
        }
        self.turn_id = None;
        outs
    }

    fn on_error_notification(&mut self, params: &Value) -> Vec<CodexOut> {
        let message = params
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("codex reported an unknown error")
            .to_owned();
        let will_retry = params
            .get("willRetry")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if will_retry {
            // Transient reconnect chatter: not a timeline event.
            return Vec::new();
        }
        let mut outs = vec![self.error_frame(message.clone())];
        let local_id = ids::item_id();
        let item = ItemEnvelope {
            id: local_id,
            item_type: ItemType::System,
            status: ItemStatus::Failed,
            role: Some(Role::System),
            content: json!({"message": message}),
            item_time: timeutil::utc_now(),
        };
        outs.push(CodexOut::Hub(ClientFrame::ItemUpsert {
            stream_id: self.stream_id.clone(),
            item,
        }));
        outs
    }

    fn on_server_request(&mut self, id: RpcId, method: &str, params: Value) -> Vec<CodexOut> {
        let (kind, title, body, family, item_id) = match method {
            "item/commandExecution/requestApproval" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown command>");
                (
                    NoticeKind::CommandExecution,
                    format!("Run command: {}", first_line(command, 120)),
                    json!({
                        "command": params.get("command").cloned().unwrap_or(Value::Null),
                        "cwd": params.get("cwd").cloned().unwrap_or(Value::Null),
                        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
                        "itemId": params.get("itemId").cloned().unwrap_or(Value::Null),
                        "turnId": params.get("turnId").cloned().unwrap_or(Value::Null),
                        "approvalKind": params.get("kind").cloned().unwrap_or(Value::Null),
                    }),
                    ApprovalFamily::CommandExecutionV2,
                    params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            }
            "item/fileChange/requestApproval" => {
                let reason = params.get("reason").and_then(Value::as_str);
                (
                    NoticeKind::FileChange,
                    match reason {
                        Some(reason) => {
                            format!("Apply file changes: {}", first_line(reason, 120))
                        }
                        None => String::from("Apply file changes"),
                    },
                    json!({
                        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
                        "itemId": params.get("itemId").cloned().unwrap_or(Value::Null),
                        "grantRoot": params.get("grantRoot").cloned().unwrap_or(Value::Null),
                        "turnId": params.get("turnId").cloned().unwrap_or(Value::Null),
                    }),
                    ApprovalFamily::FileChangeV2,
                    params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            }
            "execCommandApproval" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                (
                    NoticeKind::CommandExecution,
                    format!("Run command: {}", first_line(&command, 120)),
                    json!({
                        "command": command,
                        "cwd": params.get("cwd").cloned().unwrap_or(Value::Null),
                        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
                        "callId": params.get("callId").cloned().unwrap_or(Value::Null),
                    }),
                    ApprovalFamily::Legacy,
                    None,
                )
            }
            "applyPatchApproval" => (
                NoticeKind::FileChange,
                String::from("Apply patch"),
                json!({
                    "reason": params.get("reason").cloned().unwrap_or(Value::Null),
                    "fileChanges": params.get("fileChanges").cloned().unwrap_or(Value::Null),
                    "callId": params.get("callId").cloned().unwrap_or(Value::Null),
                }),
                ApprovalFamily::Legacy,
                None,
            ),
            "item/permissions/requestApproval" => {
                // The v2 response requires a GrantedPermissionProfile. Building
                // one from thin air would grant permissions the user never saw,
                // so answer with an error and surface it instead.
                let mut outs = vec![CodexOut::Rpc(rpc::error(
                    &id,
                    -32601,
                    "farwork-agent cannot grant additional permissions",
                ))];
                outs.push(self.error_frame(String::from(
                    "codex requested additional permissions; farwork-agent cannot grant them",
                )));
                return outs;
            }
            _ => {
                return vec![CodexOut::Rpc(rpc::error(
                    &id,
                    -32601,
                    &format!("farwork-agent does not handle {method}"),
                ))];
            }
        };

        let notice_id = ids::notice_id();
        let notice = NoticeEnvelope {
            id: notice_id.clone(),
            kind,
            status: String::from(NoticeEnvelope::OPEN),
            title,
            body,
            options: vec![
                String::from("approve"),
                String::from("approve_for_session"),
                String::from("reject"),
            ],
        };
        let mut outs = Vec::new();
        if let Some(item_key) = item_id.as_deref() {
            if let Some(track) = self.items.get_mut(item_key) {
                track.status = ItemStatus::WaitingApproval;
            }
            outs.extend(self.emit_track(item_key));
        }
        outs.push(CodexOut::Hub(ClientFrame::NoticeUpsert {
            stream_id: self.stream_id.clone(),
            notice,
        }));
        if let Some(frame) = self.set_state(SessionState::WaitingApproval) {
            outs.push(frame);
        }
        self.pending.push(PendingApproval {
            notice_id,
            rpc_id: id,
            family,
            item_id,
            deadline: Instant::now() + self.cfg.approval_timeout,
        });
        outs
    }

    fn on_item(&mut self, item: &Value, started: bool) -> Vec<CodexOut> {
        // The hub persists the user's own message from the REST payload (with
        // clientMessageId idempotency). Re-emitting codex's copy would show the
        // same prompt twice in one timeline.
        if item.get("type").and_then(Value::as_str) == Some("userMessage") {
            return Vec::new();
        }
        let Some(codex_id) = item.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return Vec::new();
        };
        let descriptor = descriptor_for(item);
        let status = if started {
            ItemStatus::Running
        } else {
            item_status(item)
        };
        let track = self.items.entry(codex_id).or_insert_with(|| ItemTrack {
            local_id: ids::item_id(),
            item_type: descriptor.item_type,
            role: descriptor.role,
            content: descriptor.content.clone(),
            status,
            sent_len: 0,
        });
        track.item_type = descriptor.item_type;
        track.role = descriptor.role;
        track.content = merge_content(track.content.clone(), descriptor.content);
        track.status = status;
        let stream_id = self.stream_id.clone();
        vec![Self::emit_item(track, &stream_id)]
    }

    fn on_delta(
        &mut self,
        codex_id: &str,
        field: &str,
        delta: &str,
        kind: StreamKind,
    ) -> Vec<CodexOut> {
        let track = self.items.entry(codex_id.to_owned()).or_insert_with(|| {
            let (item_type, role, content) = match kind {
                StreamKind::Message => (ItemType::Message, Role::Assistant, json!({"text": ""})),
                StreamKind::Tool => (
                    ItemType::Tool,
                    Role::Tool,
                    json!({"tool": "command_execution"}),
                ),
            };
            ItemTrack {
                local_id: ids::item_id(),
                item_type,
                role,
                content,
                status: ItemStatus::Running,
                sent_len: 0,
            }
        });
        let current = track
            .content
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut merged = String::with_capacity(current.len() + delta.len());
        merged.push_str(&current);
        merged.push_str(delta);
        if field == "output" {
            let limit = self.cfg.output_limit;
            let truncated = merged.chars().count() > limit;
            let kept: String = merged.chars().take(limit).collect();
            track.content[field] = json!(kept);
            track.content["outputTruncated"] = json!(truncated);
        } else {
            track.content[field] = json!(merged);
        }
        let flush_chars = self.cfg.flush_chars;
        let grew = track.streamed_len().saturating_sub(track.sent_len);
        if grew >= flush_chars {
            let stream_id = self.stream_id.clone();
            vec![Self::emit_item(track, &stream_id)]
        } else {
            Vec::new()
        }
    }

    fn close_running_items(&mut self, status: ItemStatus) -> Vec<CodexOut> {
        let stream_id = self.stream_id.clone();
        let mut outs = Vec::new();
        for track in self.items.values_mut() {
            if matches!(
                track.status,
                ItemStatus::Running | ItemStatus::WaitingApproval
            ) {
                track.status = status;
                outs.push(Self::emit_item(track, &stream_id));
            }
        }
        outs
    }

    fn emit_item(track: &mut ItemTrack, stream_id: &str) -> CodexOut {
        track.sent_len = track.streamed_len();
        CodexOut::Hub(ClientFrame::ItemUpsert {
            stream_id: stream_id.to_owned(),
            item: ItemEnvelope {
                id: track.local_id.clone(),
                item_type: track.item_type,
                status: track.status,
                role: Some(track.role),
                content: track.content.clone(),
                item_time: timeutil::utc_now(),
            },
        })
    }

    fn emit_track(&mut self, codex_id: &str) -> Option<CodexOut> {
        let stream_id = self.stream_id.clone();
        let track = self.items.get_mut(codex_id)?;
        Some(Self::emit_item(track, &stream_id))
    }

    fn resolve_all(&mut self, decision: Decision) -> Vec<CodexOut> {
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .map(|pending| {
                CodexOut::Hub(ClientFrame::NoticeResolved {
                    notice_id: pending.notice_id,
                    decision,
                })
            })
            .collect()
    }

    fn resolve_one(&mut self, request_id: &Value) -> Vec<CodexOut> {
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| &pending.rpc_id == request_id)
        else {
            return Vec::new();
        };
        let pending = self.pending.remove(index);
        let mut outs = vec![CodexOut::Hub(ClientFrame::NoticeResolved {
            notice_id: pending.notice_id,
            decision: Decision::Reject,
        })];
        if let Some(frame) = self.set_state(SessionState::Running) {
            outs.push(frame);
        }
        outs
    }

    fn request(&mut self, purpose: RequestPurpose, method: &str, params: Value) -> CodexOut {
        let id = self.take_id();
        self.inflight.insert(id, purpose);
        CodexOut::Rpc(rpc::request(id, method, params))
    }

    fn take_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn set_state(&mut self, state: SessionState) -> Option<CodexOut> {
        if self.state == state {
            return None;
        }
        self.state = state;
        Some(CodexOut::Hub(ClientFrame::SessionStateFrame {
            stream_id: self.stream_id.clone(),
            state,
        }))
    }

    fn error_frame(&self, message: String) -> CodexOut {
        CodexOut::Hub(ClientFrame::Error {
            message,
            stream_id: Some(self.stream_id.clone()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Message,
    Tool,
}

struct Descriptor {
    item_type: ItemType,
    role: Role,
    content: Value,
}

fn descriptor_for(item: &Value) -> Descriptor {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let keep_raw = |item_type: ItemType, role: Role| Descriptor {
        item_type,
        role,
        content: json!({"kind": kind, "raw": item}),
    };
    match kind {
        "agentMessage" => Descriptor {
            item_type: ItemType::Message,
            role: Role::Assistant,
            content: json!({"text": item.get("text").and_then(Value::as_str).unwrap_or("")}),
        },
        "commandExecution" => Descriptor {
            item_type: ItemType::Tool,
            role: Role::Tool,
            content: json!({
                "tool": "command_execution",
                "command": item.get("command").cloned().unwrap_or(Value::Null),
                "cwd": item.get("cwd").cloned().unwrap_or(Value::Null),
                "exitCode": item.get("exitCode").cloned().unwrap_or(Value::Null),
                "durationMs": item.get("durationMs").cloned().unwrap_or(Value::Null),
                "status": item.get("status").cloned().unwrap_or(Value::Null),
                "output": item.get("aggregatedOutput").cloned().unwrap_or(Value::Null),
            }),
        },
        "fileChange" => Descriptor {
            item_type: ItemType::Artifact,
            role: Role::Tool,
            content: json!({
                "title": "File change",
                "status": item.get("status").cloned().unwrap_or(Value::Null),
                "changes": item.get("changes").cloned().unwrap_or(json!([])),
            }),
        },
        "mcpToolCall" => Descriptor {
            item_type: ItemType::Tool,
            role: Role::Tool,
            content: json!({
                "tool": "mcp",
                "server": item.get("server").cloned().unwrap_or(Value::Null),
                "name": item.get("tool").cloned().unwrap_or(Value::Null),
                "status": item.get("status").cloned().unwrap_or(Value::Null),
                "arguments": item.get("arguments").cloned().unwrap_or(Value::Null),
            }),
        },
        "dynamicToolCall" => Descriptor {
            item_type: ItemType::Tool,
            role: Role::Tool,
            content: json!({
                "tool": "dynamic",
                "name": item.get("tool").cloned().unwrap_or(Value::Null),
                "status": item.get("status").cloned().unwrap_or(Value::Null),
                "arguments": item.get("arguments").cloned().unwrap_or(Value::Null),
            }),
        },
        "collabAgentToolCall" => Descriptor {
            item_type: ItemType::Tool,
            role: Role::Tool,
            content: json!({
                "tool": "collab",
                "name": item.get("tool").cloned().unwrap_or(Value::Null),
                "status": item.get("status").cloned().unwrap_or(Value::Null),
            }),
        },
        "webSearch" => Descriptor {
            item_type: ItemType::Tool,
            role: Role::Tool,
            content: json!({
                "tool": "web_search",
                "query": item.get("query").cloned().unwrap_or(Value::Null),
            }),
        },
        "reasoning" => Descriptor {
            item_type: ItemType::Marker,
            role: Role::Assistant,
            content: json!({
                "kind": "reasoning",
                "summary": item.get("summary").cloned().unwrap_or(json!([])),
                "text": item.get("content").cloned().unwrap_or(json!([])),
            }),
        },
        "plan" => Descriptor {
            item_type: ItemType::Marker,
            role: Role::Assistant,
            content: json!({
                "kind": "plan",
                "text": item.get("text").cloned().unwrap_or(Value::Null),
            }),
        },
        "imageGeneration" => Descriptor {
            item_type: ItemType::Artifact,
            role: Role::Tool,
            content: json!({
                "title": "Image generation",
                "status": item.get("status").cloned().unwrap_or(Value::Null),
                "savedPath": item.get("savedPath").cloned().unwrap_or(Value::Null),
            }),
        },
        "contextCompaction" => Descriptor {
            item_type: ItemType::Marker,
            role: Role::System,
            content: json!({"kind": "context_compaction"}),
        },
        "enteredReviewMode" | "exitedReviewMode" => Descriptor {
            item_type: ItemType::Marker,
            role: Role::System,
            content: json!({"kind": kind}),
        },
        // Anything codex adds later still becomes visible instead of vanishing.
        _ => keep_raw(ItemType::Marker, Role::System),
    }
}

fn merge_content(mut previous: Value, next: Value) -> Value {
    if let (Some(previous), Some(next)) = (previous.as_object_mut(), next.as_object()) {
        for (key, value) in next {
            // Streamed fields already carry partial text; never let a
            // "started" payload wipe it back to an empty string.
            if value.is_null() {
                previous.entry(key.clone()).or_insert(Value::Null);
                continue;
            }
            previous.insert(key.clone(), value.clone());
        }
    }
    previous
}

fn item_status(item: &Value) -> ItemStatus {
    match item.get("status").and_then(Value::as_str) {
        Some("inProgress") => ItemStatus::Running,
        Some("completed") => ItemStatus::Done,
        Some("failed") => ItemStatus::Failed,
        Some("declined") => ItemStatus::Cancelled,
        _ => ItemStatus::Done,
    }
}

fn first_line(value: &str, limit: usize) -> String {
    let line = value.lines().next().unwrap_or("");
    if line.chars().count() <= limit {
        return line.to_owned();
    }
    line.chars().take(limit).collect::<String>() + "…"
}

fn approval_result(family: ApprovalFamily, decision: Decision) -> Value {
    let decision = match (family, decision) {
        (ApprovalFamily::Legacy, Decision::Approve) => "approved",
        (ApprovalFamily::Legacy, Decision::ApproveForSession) => "approved_for_session",
        (ApprovalFamily::Legacy, Decision::Reject) => "denied",
        (_, Decision::Approve) => "accept",
        (_, Decision::ApproveForSession) => "acceptForSession",
        (_, Decision::Reject) => "decline",
    };
    json!({"decision": decision})
}
