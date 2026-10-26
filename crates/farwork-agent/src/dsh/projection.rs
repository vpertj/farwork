//! DSH notifications in, Ingress Protocol frames out.
//!
//! Everything here is pure: no sockets, no clocks that matter, no state beyond the
//! two mappings that the contract actually leaves open (an item's type and its
//! status). The point is that "what does DSH's wording mean in our vocabulary" is
//! answered in one place that can be read against the schema.

use serde_json::{json, Value};

use crate::protocol::{
    CapabilityFlags, CatalogItem, CatalogSet, ClientFrame, CommandEntry, Decision, ItemEnvelope,
    ItemStatus, ItemType, NoticeAction, NoticeEnvelope, NoticeKind, NoticeOption, Role,
    SessionState,
};

use super::client::{CapabilitySet, CatalogEntry};
use super::handshake::Features;

/// The notice kind our timeline contract uses for a generic runtime interaction.
/// DSH approvals are not command executions, so reporting them as one would be a
/// lie the browser then renders.
pub const INTERACTION_KIND: &str = "interaction";

/// Turn markers are the runtime's own bookkeeping: the contract says they stay out
/// of the timeline, and a chat filled with `turn.start` rows is unusable.
const SKIPPED_TYPES: [&str; 2] = ["turn.start", "turn.end"];

pub fn item_type(raw: &str) -> Option<ItemType> {
    match raw {
        "message" => Some(ItemType::Message),
        "tool" => Some(ItemType::Tool),
        "artifact" => Some(ItemType::Artifact),
        "marker" => Some(ItemType::Marker),
        "system" => Some(ItemType::System),
        _ => None,
    }
}

pub fn item_status(raw: &str) -> Option<ItemStatus> {
    match raw {
        "pending" => Some(ItemStatus::Pending),
        // The bridge spells in-flight rows both ways depending on the projector.
        "inProgress" | "in_progress" | "running" => Some(ItemStatus::Running),
        "waiting_approval" => Some(ItemStatus::WaitingApproval),
        "done" | "complete" | "completed" => Some(ItemStatus::Done),
        "failed" => Some(ItemStatus::Failed),
        "cancelled" => Some(ItemStatus::Cancelled),
        "interrupted" => Some(ItemStatus::Interrupted),
        // "hidden" is DSH's way of saying "do not show this one".
        _ => None,
    }
}

pub fn role(raw: Option<&str>) -> Option<Role> {
    match raw {
        Some("user") => Some(Role::User),
        Some("assistant") => Some(Role::Assistant),
        Some("system") => Some(Role::System),
        Some("tool") => Some(Role::Tool),
        _ => None,
    }
}

/// Keys that describe *what kind of thing* a content object is rather than any
/// of its text. `{"kind":"reasoning","text":""}` is all discriminator and no
/// content, which is exactly the empty card we must not create.
const CONTENT_DISCRIMINATORS: [&str; 3] = ["kind", "type", "format"];

/// Whether a projected content value can render anything at all.
///
/// The rule is deliberately conservative in both directions: numbers and booleans
/// are metadata (a `streaming:false` flag is not something to look at), while a
/// tool call is worth a row as soon as it carries a non-blank name or input — so
/// tool/artifact rows survive even before they produce output text.
fn has_displayable_content(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(has_displayable_content),
        Value::Object(map) => map.iter().any(|(key, child)| {
            !CONTENT_DISCRIMINATORS.contains(&key.as_str()) && has_displayable_content(child)
        }),
        // Null, numbers and booleans carry no text of their own.
        _ => false,
    }
}

/// One `timeline.itemUpsert` payload. `None` means "not part of the timeline",
/// which is a decision, not a failure: turn markers and hidden rows are excluded
/// on purpose, and so are the two noise categories below.
pub fn item(value: &Value, fallback_time: &str) -> Option<ItemEnvelope> {
    let raw_type = value.get("type").and_then(Value::as_str)?;
    if SKIPPED_TYPES.contains(&raw_type) {
        return None;
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let item_type = item_type(raw_type)?;
    let raw_status = value.get("status").and_then(Value::as_str)?;
    let status = item_status(raw_status)?;
    let role = role(value.get("role").and_then(Value::as_str));
    // The hub persists the user's own message from the REST payload (with
    // clientMessageId idempotency), and DSH echoes that same message back as an
    // item. Re-projecting the echo would print every prompt twice in one
    // timeline — the same decision codex's mapper makes for `userMessage`
    // (codex/mapper.rs:720-726). Only the user role is skipped; assistant, tool
    // and artifact items are the runtime's own output and must flow.
    if role == Some(Role::User) {
        tracing::debug!(item_id = %id, "dsh echoed the user message back; not re-projecting it");
        return None;
    }
    let content = value.get("content").cloned().unwrap_or(Value::Null);
    // An item with nothing to display becomes an empty card in the browser. This
    // is how the blank `{kind:"reasoning",text:""}` rows stop showing up as
    // `system` items; real reasoning text is kept (it is displayable content we
    // may render later) and produces no second, empty row of its own.
    if !has_displayable_content(&content) {
        tracing::debug!(
            item_id = %id,
            item_type = %raw_type,
            "skipping a dsh item with no displayable content"
        );
        return None;
    }
    let item_time = value
        .get("metadata")
        .and_then(|metadata| metadata.get("itemTime"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("source")
                .and_then(|s| s.get("createdAt"))
                .and_then(Value::as_str)
        })
        .unwrap_or(fallback_time)
        .to_string();
    Some(ItemEnvelope {
        id: id.to_string(),
        item_type,
        status,
        role,
        content,
        item_time,
    })
}

/// DSH session status → the four states our contract has.
pub fn session_state(raw: &str) -> SessionState {
    match raw {
        "idle" => SessionState::Idle,
        "waiting_approval" | "blocked" => SessionState::WaitingApproval,
        "error" | "disconnected" => SessionState::Error,
        // pending / running / stopping: a turn is in flight.
        _ => SessionState::Running,
    }
}

/// The notice's action list, as our `options` array.
pub fn notice_actions(value: &Value) -> Vec<NoticeAction> {
    value
        .get("actions")
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(|action| {
                    let id = action.get("actionId").and_then(Value::as_str)?.trim();
                    if id.is_empty() {
                        return None;
                    }
                    let label = action
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .unwrap_or(id);
                    Some(NoticeAction {
                        id: id.to_string(),
                        label: label.to_string(),
                        style: action
                            .get("style")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn notice_status(value: &Value) -> &str {
    value
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty())
        .unwrap_or("open")
}

/// An open interaction notice → our notice envelope.
///
/// The handshake gates this: a bridge that never advertised `approval` (or a
/// read-only runtime) produces no approval notice here, because forwarding one
/// would hand the browser a button that cannot be answered.
pub fn notice(value: &Value, features: &Features, session_id: &str) -> Option<NoticeEnvelope> {
    if value.get("sessionId").and_then(Value::as_str) != Some(session_id) {
        return None;
    }
    let notice_id = value
        .get("noticeId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let notice_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("notification");
    let interaction_type = value
        .get("interactionType")
        .and_then(Value::as_str)
        .map(str::to_string);
    let response_required = value
        .get("responseRequired")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if response_required {
        match interaction_type.as_deref() {
            Some("approval") if !features.approval_available() => {
                tracing::debug!(%notice_id, "the bridge did not advertise approval; notice not forwarded");
                return None;
            }
            Some("question") | Some("input_request") | Some("input_request_v1")
                if !features.questions_available() =>
            {
                tracing::debug!(%notice_id, "the bridge did not advertise user questions; notice not forwarded");
                return None;
            }
            _ => {}
        }
    }
    let actions = notice_actions(value);
    // A response-required notice with no actions cannot be answered: forwarding it
    // would leave the browser with a prompt and nothing to click.
    if response_required && actions.is_empty() {
        tracing::warn!(%notice_id, "dropping a response-required notice with no actions");
        return None;
    }
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("DSH interaction");
    Some(NoticeEnvelope {
        id: notice_id.to_string(),
        kind: NoticeKind::Interaction,
        status: String::from(NoticeEnvelope::OPEN),
        title: title.to_string(),
        body: json!({
            "runtime": "dsh",
            "type": notice_type,
            "interactionType": interaction_type,
            "message": value.get("message").cloned().unwrap_or(Value::Null),
            "severity": value.get("severity").cloned().unwrap_or(json!("info")),
            "blocking": value.get("blocking").cloned().unwrap_or(Value::Null),
            "context": value.get("context").cloned().unwrap_or(Value::Null),
            "source": value.get("source").cloned().unwrap_or(Value::Null),
            "inputSchema": value.get("inputSchema").cloned().unwrap_or(Value::Null),
        }),
        interaction_type: interaction_type.or_else(|| Some(notice_type.to_string())),
        options: actions.into_iter().map(NoticeOption::Action).collect(),
    })
}

/// Our own catalogs, from the bridge's real lists. Never a placeholder: an empty
/// list is the honest answer when the bridge returned nothing.
pub fn catalogs(models: &[CatalogEntry], permissions: &[CatalogEntry]) -> CatalogSet {
    CatalogSet {
        models: models
            .iter()
            .map(|model| CatalogItem::new(model.id.clone(), model.title.clone(), None))
            .collect(),
        permissions: permissions
            .iter()
            .map(|permission| {
                CatalogItem::new(permission.id.clone(), permission.title.clone(), None)
            })
            .collect(),
    }
}

/// What this session can do, from the bridge's own capability set plus whether a
/// real catalog exists to choose from.
pub fn capability_flags(caps: &CapabilitySet, catalog: &CatalogSet) -> CapabilityFlags {
    let has = |id: &str| caps.has(id);
    let catalog_non_empty = !catalog.models.is_empty() || !catalog.permissions.is_empty();
    // With no capability entries at all we know nothing; a catalog we really read
    // is then the only honest reason to offer a picker.
    let selections = if caps.entries.is_empty() {
        catalog_non_empty
    } else {
        catalog_non_empty && (has("catalog.model") || has("catalog.permission"))
    };
    CapabilityFlags {
        steer: has("session.steer"),
        interrupt: has("session.interrupt"),
        selections,
        // `session.executeCommand` is not wired up here, so claiming commands
        // would hand the browser a box whose input goes nowhere.
        commands: false,
    }
}

pub fn capabilities_frame(
    stream_id: &str,
    caps: &CapabilitySet,
    catalog: &CatalogSet,
) -> ClientFrame {
    ClientFrame::SessionCapabilities {
        stream_id: stream_id.to_string(),
        capabilities: capability_flags(caps, catalog),
        catalogs: catalog.clone(),
        commands: Vec::<CommandEntry>::new(),
    }
}

/// The closed decision vocabulary is the hub's; only the mappings that are
/// actually true are made. Anything else stays `None` and the action id speaks for
/// itself on the wire.
pub fn decision_for_action(action_id: &str) -> Option<Decision> {
    let normalized = action_id.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "approve" | "allow" | "allow-once" | "allowed-once" | "accept" | "yes" | "submit"
        | "continue" => Some(Decision::Approve),
        "approve-for-session" | "allow-always" | "allow-always-session" | "always" => {
            Some(Decision::ApproveForSession)
        }
        "reject" | "deny" | "denied" | "no" | "decline" | "cancel" | "abort" | "dismiss" => {
            Some(Decision::Reject)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features() -> Features {
        Features {
            approval: true,
            user_questions: true,
            sync_mode: Some(String::from("events")),
            projection_version: Some(3),
            ..Features::default()
        }
    }

    fn approval_notice() -> Value {
        json!({
            "noticeId": "dsh_approval_1",
            "sessionId": "sess_dsh_1",
            "runtime": "dsh",
            "type": "interaction",
            "interactionType": "approval",
            "title": "请求批准：shell",
            "message": "此操作需要你的批准。允许仅对本次请求生效。",
            "severity": "warning",
            "status": "open",
            "revision": 1,
            "responseRequired": true,
            "blocking": { "scope": "session", "targetId": "sess_dsh_1" },
            "context": { "toolName": "shell" },
            "actions": [
                { "actionId": "allow-once", "label": "允许一次", "style": "primary" },
                { "actionId": "reject", "label": "拒绝", "style": "secondary" },
            ],
        })
    }

    #[test]
    fn a_message_item_maps_onto_the_timeline_contract() {
        let envelope = item(
            &json!({
                "id": "dsh_abc", "type": "message", "status": "inProgress", "role": "assistant",
                "content": { "text": "hi" }, "source": { "createdAt": "2026-01-01T00:00:00Z" },
            }),
            "now",
        )
        .expect("item");
        assert_eq!(envelope.id, "dsh_abc");
        assert_eq!(envelope.item_type, ItemType::Message);
        assert_eq!(envelope.status, ItemStatus::Running);
        assert_eq!(envelope.role, Some(Role::Assistant));
        assert_eq!(envelope.content["text"], json!("hi"));
        assert_eq!(envelope.item_time, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn turn_markers_and_hidden_rows_are_not_items() {
        for raw in [
            json!({"id":"t1","type":"turn.start","status":"done"}),
            json!({"id":"t2","type":"turn.end","status":"done"}),
            json!({"id":"t3","type":"message","status":"hidden"}),
            json!({"id":"t4","type":"nonsense","status":"done"}),
            json!({"type":"message","status":"done"}),
        ] {
            assert!(item(&raw, "now").is_none(), "should be skipped: {raw}");
        }
    }

    #[test]
    fn a_user_message_echo_is_not_re_projected() {
        // The hub already wrote the prompt from the REST payload; DSH echoes that same
        // turn back with its own item id, so projecting it shows the prompt twice.
        let echo = json!({
            "id": "dsh_echo_1", "type": "message", "status": "done", "role": "user",
            "content": { "kind": "markdown", "text": "reply with the single word pong" },
        });
        assert!(
            item(&echo, "now").is_none(),
            "the user echo must be skipped"
        );
        // The same content from the assistant is the real reply and is kept.
        let reply = json!({
            "id": "dsh_reply_1", "type": "message", "status": "done", "role": "assistant",
            "content": { "kind": "markdown", "text": "pong" },
        });
        let envelope = item(&reply, "now").expect("assistant reply");
        assert_eq!(envelope.role, Some(Role::Assistant));
        assert_eq!(envelope.content["text"], json!("pong"));
    }

    #[test]
    fn an_empty_reasoning_item_is_not_projected() {
        for text in [json!(""), json!("   "), json!("\n\t ")] {
            let empty = json!({
                "id": "dsh_think_1", "type": "system", "status": "done", "role": "assistant",
                "content": { "kind": "reasoning", "text": text },
            });
            assert!(
                item(&empty, "now").is_none(),
                "an empty reasoning row must not become a system item: {empty}"
            );
        }
        // No content at all, or only discriminator keys, is the same empty card.
        for content in [
            Value::Null,
            json!({ "kind": "reasoning" }),
            json!({ "kind": "markdown", "text": "" }),
            json!([]),
        ] {
            let raw = json!({
                "id": "dsh_blank", "type": "system", "status": "done", "content": content,
            });
            assert!(item(&raw, "now").is_none(), "should be skipped: {raw}");
        }
    }

    #[test]
    fn real_reasoning_text_is_kept_once_and_not_reshaped_into_a_system_row() {
        let thinking = json!({
            "id": "dsh_think_2", "type": "system", "status": "done", "role": "assistant",
            "content": { "kind": "reasoning", "text": "the user wants one word" },
        });
        let envelope = item(&thinking, "now").expect("non-empty reasoning is kept");
        assert_eq!(envelope.role, Some(Role::Assistant));
        // One item in, one item out: the reasoning text is never turned into an
        // extra row (an empty one would land as a blank card).
        assert_eq!(envelope.content["kind"], json!("reasoning"));
        assert_eq!(envelope.content["text"], json!("the user wants one word"));
    }

    #[test]
    fn skipping_user_and_blank_rows_leaves_tool_and_artifact_rows_alone() {
        let tool_call = json!({
            "id": "dsh_tool_1", "type": "tool", "status": "running", "role": "tool",
            "content": { "kind": "tool", "toolName": "shell", "input": { "command": "ls" } },
        });
        let envelope = item(&tool_call, "now").expect("tool call");
        assert_eq!(envelope.item_type, ItemType::Tool);
        assert_eq!(envelope.role, Some(Role::Tool));

        let artifact = json!({
            "id": "dsh_artifact_1", "type": "artifact", "status": "done", "role": "assistant",
            "content": { "kind": "artifact", "title": "report.md", "path": "/w/report.md" },
        });
        let envelope = item(&artifact, "now").expect("artifact");
        assert_eq!(envelope.item_type, ItemType::Artifact);
        assert_eq!(envelope.content["path"], json!("/w/report.md"));

        // Tool output as a bare string, and a plain assistant message, both survive.
        for raw in [
            json!({"id":"dsh_tool_2","type":"tool","status":"done","role":"tool","content":"exit 0"}),
            json!({"id":"dsh_asst","type":"message","status":"done","role":"assistant","content":{"text":"pong"}}),
        ] {
            assert!(item(&raw, "now").is_some(), "should be kept: {raw}");
        }
    }

    #[test]
    fn session_status_maps_only_what_the_contract_can_say() {
        assert_eq!(session_state("idle"), SessionState::Idle);
        assert_eq!(
            session_state("waiting_approval"),
            SessionState::WaitingApproval
        );
        assert_eq!(session_state("blocked"), SessionState::WaitingApproval);
        assert_eq!(session_state("error"), SessionState::Error);
        assert_eq!(session_state("disconnected"), SessionState::Error);
        assert_eq!(session_state("running"), SessionState::Running);
        assert_eq!(session_state("pending"), SessionState::Running);
    }

    #[test]
    fn a_dsh_approval_becomes_a_notice_with_object_options() {
        let envelope = notice(&approval_notice(), &features(), "sess_dsh_1").expect("notice");
        assert_eq!(envelope.id, "dsh_approval_1");
        assert_eq!(envelope.kind, NoticeKind::Interaction);
        assert_eq!(envelope.status, NoticeEnvelope::OPEN);
        assert_eq!(envelope.interaction_type.as_deref(), Some("approval"));
        assert_eq!(envelope.title, "请求批准：shell");
        assert_eq!(
            envelope.options,
            vec![
                NoticeOption::action("allow-once", "允许一次", Some("primary")),
                NoticeOption::action("reject", "拒绝", Some("secondary")),
            ]
        );
        // The whole notice is carried in body: title/message/context all survive
        // so the browser never has to re-read the bridge.
        assert_eq!(envelope.body["context"]["toolName"], json!("shell"));
        assert_eq!(envelope.body["severity"], json!("warning"));
    }

    #[test]
    fn a_notice_for_another_session_is_not_ours() {
        assert!(notice(&approval_notice(), &features(), "sess_other").is_none());
    }

    #[test]
    fn without_the_approval_feature_no_approval_notice_is_forwarded() {
        let no_approval = Features::default();
        assert!(notice(&approval_notice(), &no_approval, "sess_dsh_1").is_none());
        // read-only is the same answer: the answer would be swallowed.
        let read_only = Features {
            approval: true,
            read_only: true,
            ..Features::default()
        };
        assert!(notice(&approval_notice(), &read_only, "sess_dsh_1").is_none());
        // A notification needs no answer, so it still flows.
        let mut notification = approval_notice();
        notification["responseRequired"] = json!(false);
        notification["type"] = json!("notification");
        notification["interactionType"] = Value::Null;
        assert!(notice(&notification, &no_approval, "sess_dsh_1").is_some());
    }

    #[test]
    fn a_response_required_notice_without_actions_is_dropped() {
        let mut value = approval_notice();
        value["actions"] = json!([]);
        assert!(notice(&value, &features(), "sess_dsh_1").is_none());
    }

    #[test]
    fn a_question_notice_is_gated_by_user_questions() {
        let mut value = approval_notice();
        value["interactionType"] = json!("question");
        value["actions"] = json!([
            { "actionId": "submit", "label": "提交回答", "style": "primary" },
            { "actionId": "cancel", "label": "取消", "style": "secondary" },
        ]);
        assert!(notice(&value, &features(), "sess_dsh_1").is_some());
        let no_questions = Features {
            approval: true,
            ..Features::default()
        };
        assert!(notice(&value, &no_questions, "sess_dsh_1").is_none());
    }

    fn capability_set(entries: &[(&str, bool)]) -> CapabilitySet {
        use super::super::client::CapabilityEntry;
        CapabilitySet {
            revision: 1,
            entries: entries
                .iter()
                .map(|(id, effective)| CapabilityEntry {
                    capability_id: (*id).to_string(),
                    supported: *effective,
                    available: *effective,
                    allowed: *effective,
                })
                .collect(),
        }
    }

    #[test]
    fn capabilities_come_from_the_bridge_and_only_what_it_actually_offers() {
        let catalog = catalogs(
            &[CatalogEntry {
                id: String::from("dsh:model:x"),
                title: String::from("DeepSeek-V4-Pro"),
            }],
            &[CatalogEntry {
                id: String::from("dsh:permission:y"),
                title: String::from("workspace-write"),
            }],
        );
        let caps = capability_set(&[
            ("session.interrupt", true),
            ("session.steer", false),
            ("catalog.model", true),
            ("catalog.permission", true),
        ]);
        let flags = capability_flags(&caps, &catalog);
        assert!(flags.interrupt);
        assert!(!flags.steer);
        assert!(flags.selections);
        assert!(!flags.commands);

        // No catalogs → no picker, whatever the capability set claims.
        let flags = capability_flags(&caps, &CatalogSet::default());
        assert!(!flags.selections);
        assert_eq!(catalog.models[0].name, "DeepSeek-V4-Pro");
    }

    #[test]
    fn an_empty_capability_set_claims_nothing_but_a_real_catalog_can_still_be_offered() {
        let empty = CapabilitySet::default();
        let flags = capability_flags(&empty, &CatalogSet::default());
        assert!(!flags.steer && !flags.interrupt && !flags.selections && !flags.commands);
        let catalog = catalogs(
            &[CatalogEntry {
                id: String::from("m"),
                title: String::from("M"),
            }],
            &[],
        );
        let flags = capability_flags(&empty, &catalog);
        assert!(
            flags.selections,
            "a real catalog with no capability claim is still offerable"
        );
    }

    #[test]
    fn decisions_are_only_mapped_when_the_mapping_is_true() {
        assert_eq!(decision_for_action("allow-once"), Some(Decision::Approve));
        assert_eq!(decision_for_action("reject"), Some(Decision::Reject));
        assert_eq!(
            decision_for_action("approve_for_session"),
            Some(Decision::ApproveForSession)
        );
        assert_eq!(
            decision_for_action("allow-always"),
            Some(Decision::ApproveForSession)
        );
        assert_eq!(decision_for_action("submit"), Some(Decision::Approve));
        // An action we do not recognise is not silently turned into "reject".
        assert_eq!(decision_for_action("edit-and-allow"), None);
    }
}
