//! Mapping + driver tests. They run against scripted frames only: no provider,
//! no network, no child process.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::mpsc::unbounded_channel;

use super::mapper::{CodexMapper, CodexOut, MapperConfig, SessionSeed, DEFAULT_APPROVAL_TIMEOUT};
use super::session::{CodexSession, SessionCmd};
use super::transport::fake::FakeTransport;
use crate::protocol::{
    ClientFrame, Decision, ItemEnvelope, ItemStatus, ItemType, NoticeEnvelope, NoticeKind, Role,
    SessionState,
};

fn seed() -> SessionSeed {
    SessionSeed {
        stream_id: String::from("str_1"),
        session_id: String::from("sess_1"),
        cwd: String::from("/tmp"),
        prompt: String::from("do it"),
    }
}

fn config() -> MapperConfig {
    MapperConfig {
        flush_chars: 16,
        output_limit: 40,
        ..MapperConfig::default()
    }
}

fn rpcs(outs: &[CodexOut]) -> Vec<Value> {
    outs.iter()
        .filter_map(|out| match out {
            CodexOut::Rpc(value) => Some(value.clone()),
            CodexOut::Hub(_) => None,
        })
        .collect()
}

fn frames(outs: &[CodexOut]) -> Vec<ClientFrame> {
    outs.iter()
        .filter_map(|out| match out {
            CodexOut::Hub(frame) => Some(frame.clone()),
            CodexOut::Rpc(_) => None,
        })
        .collect()
}

fn items(outs: &[CodexOut]) -> Vec<ItemEnvelope> {
    frames(outs)
        .into_iter()
        .filter_map(|frame| match frame {
            ClientFrame::ItemUpsert { item, .. } => Some(item),
            _ => None,
        })
        .collect()
}

fn notices(outs: &[CodexOut]) -> Vec<NoticeEnvelope> {
    frames(outs)
        .into_iter()
        .filter_map(|frame| match frame {
            ClientFrame::NoticeUpsert { notice, .. } => Some(notice),
            _ => None,
        })
        .collect()
}

fn states(outs: &[CodexOut]) -> Vec<SessionState> {
    frames(outs)
        .into_iter()
        .filter_map(|frame| match frame {
            ClientFrame::SessionStateFrame { state, .. } => Some(state),
            _ => None,
        })
        .collect()
}

fn resolved(outs: &[CodexOut]) -> Vec<(String, Option<Decision>)> {
    frames(outs)
        .into_iter()
        .filter_map(|frame| match frame {
            ClientFrame::NoticeResolved {
                notice_id,
                decision,
                action_id,
            } => {
                // codex never answers with a runtime action id.
                assert!(action_id.is_none());
                Some((notice_id, decision))
            }
            _ => None,
        })
        .collect()
}

fn errors(outs: &[CodexOut]) -> Vec<String> {
    frames(outs)
        .into_iter()
        .filter_map(|frame| match frame {
            ClientFrame::Error { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

/// Drive the handshake to the point where a turn is running.
fn started_mapper() -> CodexMapper {
    let mut mapper = CodexMapper::new(seed(), config());
    let outs = mapper.begin();
    assert_eq!(rpcs(&outs)[0]["method"], json!("initialize"));
    let outs = mapper.on_rpc(json!({
        "id": 1,
        "result": {"codexHome": "/home", "platformFamily": "unix", "platformOs": "macos", "userAgent": "x"}
    }));
    assert_eq!(rpcs(&outs)[0]["method"], json!("initialized"));
    let outs = mapper.on_rpc(json!({"id": 2, "result": {"thread": {"id": "th_1"}}}));
    assert_eq!(rpcs(&outs)[0]["method"], json!("turn/start"));
    let outs = mapper.on_rpc(json!({"id": 3, "result": {"turn": {"id": "tu_1"}}}));
    assert!(outs.is_empty(), "unexpected outs: {outs:?}");
    mapper
}

#[test]
fn handshake_sends_initialize_then_initialized_then_thread_start_then_turn_start() {
    let mut mapper = CodexMapper::new(seed(), config());
    let outs = mapper.begin();
    assert_eq!(states(&outs), vec![SessionState::Running]);
    let first = rpcs(&outs);
    assert_eq!(first[0]["id"], json!(1));
    assert_eq!(first[0]["method"], json!("initialize"));
    assert_eq!(
        first[0]["params"]["clientInfo"]["name"],
        json!("farwork-agent")
    );

    let outs = mapper.on_rpc(json!({"id": 1, "result": {}}));
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["method"], json!("initialized"));
    assert_eq!(outbound[1]["id"], json!(2));
    assert_eq!(outbound[1]["method"], json!("thread/start"));
    assert_eq!(outbound[1]["params"]["cwd"], json!("/tmp"));
    assert_eq!(outbound[1]["params"]["approvalPolicy"], json!("on-request"));
    assert_eq!(outbound[1]["params"]["sandbox"], json!("workspace-write"));

    let outs = mapper.on_rpc(json!({"id": 2, "result": {"thread": {"id": "th_1"}}}));
    assert_eq!(
        frames(&outs)[0],
        ClientFrame::SessionStarted {
            stream_id: String::from("str_1"),
            external_session_id: String::from("th_1"),
        }
    );
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["id"], json!(3));
    assert_eq!(outbound[0]["method"], json!("turn/start"));
    assert_eq!(outbound[0]["params"]["threadId"], json!("th_1"));
    assert_eq!(outbound[0]["params"]["input"][0]["text"], json!("do it"));
}

#[test]
fn subsequent_user_message_starts_a_new_turn_with_the_client_message_id() {
    let mut mapper = started_mapper();
    let outs = mapper.on_user_message("second", "cm_1");
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["method"], json!("turn/start"));
    assert_eq!(outbound[0]["params"]["clientUserMessageId"], json!("cm_1"));
    assert_eq!(outbound[0]["params"]["input"][0]["text"], json!("second"));
}

#[test]
fn user_message_before_the_thread_exists_is_an_error_not_a_guess() {
    let mut mapper = CodexMapper::new(seed(), config());
    let outs = mapper.on_user_message("hi", "cm_1");
    assert!(rpcs(&outs).is_empty());
    assert_eq!(errors(&outs).len(), 1);
}

#[test]
fn assistant_deltas_merge_into_one_message_item_that_finishes_done() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {"type": "agentMessage", "id": "i1", "text": ""}, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].item_type, ItemType::Message);
    assert_eq!(emitted[0].role, Some(Role::Assistant));
    assert_eq!(emitted[0].status, ItemStatus::Running);
    let local_id = emitted[0].id.clone();

    // Below the flush threshold: no extra row write.
    let outs = mapper.on_rpc(json!({
        "method": "item/agentMessage/delta",
        "params": {"itemId": "i1", "delta": "hello ", "threadId": "th_1", "turnId": "tu_1"}
    }));
    assert!(items(&outs).is_empty());

    // Crossing it publishes the merged text under the same item id.
    let outs = mapper.on_rpc(json!({
        "method": "item/agentMessage/delta",
        "params": {"itemId": "i1", "delta": "world, this is long", "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].id, local_id);
    assert_eq!(
        emitted[0].content["text"],
        json!("hello world, this is long")
    );
    assert_eq!(emitted[0].status, ItemStatus::Running);

    let outs = mapper.on_rpc(json!({
        "method": "item/completed",
        "params": {"item": {"type": "agentMessage", "id": "i1", "text": "hello world, this is long!"}, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].id, local_id, "same timeline row must be updated");
    assert_eq!(emitted[0].status, ItemStatus::Done);
    assert_eq!(
        emitted[0].content["text"],
        json!("hello world, this is long!")
    );
}

#[test]
fn delta_for_an_unseen_item_creates_the_message_track_lazily() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/agentMessage/delta",
        "params": {"itemId": "i9", "delta": "a".repeat(20), "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].item_type, ItemType::Message);
    assert_eq!(emitted[0].role, Some(Role::Assistant));
}

#[test]
fn codex_user_message_items_are_not_re_emitted() {
    // The hub already wrote the user's message from the REST payload; a second
    // row for codex's own copy of the same prompt would be a duplicate.
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {"type": "userMessage", "id": "u1",
                            "content": [{"type": "text", "text": "do it", "text_elements": []}]},
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    assert!(outs.is_empty(), "unexpected outs: {outs:?}");
    let outs = mapper.on_rpc(json!({
        "method": "item/completed",
        "params": {"item": {"type": "userMessage", "id": "u1",
                            "content": [{"type": "text", "text": "do it", "text_elements": []}]},
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    assert!(outs.is_empty(), "unexpected outs: {outs:?}");
}

#[test]
fn command_execution_becomes_a_tool_item_with_output_and_exit_code() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {
            "type": "commandExecution", "id": "c1", "command": "cargo test", "cwd": "/w",
            "commandActions": [], "status": "inProgress", "aggregatedOutput": null, "exitCode": null
        }, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted[0].item_type, ItemType::Tool);
    assert_eq!(emitted[0].role, Some(Role::Tool));
    assert_eq!(emitted[0].status, ItemStatus::Running);
    assert_eq!(emitted[0].content["command"], json!("cargo test"));
    let local_id = emitted[0].id.clone();

    let outs = mapper.on_rpc(json!({
        "method": "item/commandExecution/outputDelta",
        "params": {"itemId": "c1", "delta": "ok\n", "threadId": "th_1", "turnId": "tu_1"}
    }));
    assert!(items(&outs).is_empty());

    let outs = mapper.on_rpc(json!({
        "method": "item/completed",
        "params": {"item": {
            "type": "commandExecution", "id": "c1", "command": "cargo test", "cwd": "/w",
            "commandActions": [], "status": "failed", "aggregatedOutput": "boom\n", "exitCode": 3
        }, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted[0].id, local_id);
    assert_eq!(emitted[0].status, ItemStatus::Failed);
    assert_eq!(emitted[0].content["exitCode"], json!(3));
    assert_eq!(emitted[0].content["output"], json!("boom\n"));
}

#[test]
fn streamed_output_is_capped_and_flagged() {
    let mut mapper = started_mapper();
    mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {
            "type": "commandExecution", "id": "c1", "command": "x", "cwd": "/w",
            "commandActions": [], "status": "inProgress"
        }, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let outs = mapper.on_rpc(json!({
        "method": "item/commandExecution/outputDelta",
        "params": {"itemId": "c1", "delta": "z".repeat(200), "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    let text = emitted[0].content["output"].as_str().unwrap();
    assert_eq!(text.chars().count(), 40);
    assert_eq!(emitted[0].content["outputTruncated"], json!(true));
}

#[test]
fn file_change_becomes_an_artifact_with_the_patch() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/completed",
        "params": {"item": {
            "type": "fileChange", "id": "f1", "status": "completed",
            "changes": [{"path": "src/lib.rs", "kind": {"type": "update"}, "diff": "@@ -1 +1 @@"}]
        }, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted[0].item_type, ItemType::Artifact);
    assert_eq!(emitted[0].status, ItemStatus::Done);
    assert_eq!(
        emitted[0].content["changes"][0]["path"],
        json!("src/lib.rs")
    );
}

#[test]
fn patch_updated_notification_keeps_the_artifact_row_in_sync() {
    let mut mapper = started_mapper();
    mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {"type": "fileChange", "id": "f1", "status": "inProgress", "changes": []},
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    let outs = mapper.on_rpc(json!({
        "method": "item/fileChange/patchUpdated",
        "params": {"itemId": "f1", "changes": [{"path": "a.txt", "kind": {"type": "add"}, "diff": "+a"}],
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].content["changes"][0]["path"], json!("a.txt"));
}

#[test]
fn unknown_item_type_is_surfaced_as_a_marker_not_dropped() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "item/completed",
        "params": {"item": {"type": "brandNewThing", "id": "n1", "payload": 1},
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted[0].item_type, ItemType::Marker);
    assert_eq!(emitted[0].content["kind"], json!("brandNewThing"));
}

#[test]
fn command_approval_becomes_a_notice_and_answers_in_codex_vocabulary() {
    let mut mapper = started_mapper();
    mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {
            "type": "commandExecution", "id": "c1", "command": "rm -rf build", "cwd": "/w",
            "commandActions": [], "status": "inProgress"
        }, "threadId": "th_1", "turnId": "tu_1"}
    }));
    let outs = mapper.on_rpc(json!({
        "id": 9,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1",
                   "command": "rm -rf build", "cwd": "/w", "reason": null, "kind": "command"}
    }));
    let raised = notices(&outs);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].kind, NoticeKind::CommandExecution);
    assert_eq!(raised[0].status, String::from("open"));
    assert!(
        raised[0].title.contains("rm -rf build"),
        "title: {}",
        raised[0].title
    );
    // Approval buttons are objects now, with the same ids as before: the closed
    // decision mapping and the browser's actionId path both keep working.
    assert_eq!(
        raised[0].options,
        crate::protocol::NoticeOption::decision_options()
    );
    assert_eq!(
        raised[0]
            .options
            .iter()
            .map(|option| option.id().to_string())
            .collect::<Vec<_>>(),
        vec![
            String::from("approve"),
            String::from("approve_for_session"),
            String::from("reject")
        ]
    );
    assert_eq!(raised[0].body["command"], json!("rm -rf build"));
    // The tool item is marked as waiting on a human.
    let emitted = items(&outs);
    assert_eq!(emitted[0].status, ItemStatus::WaitingApproval);
    assert_eq!(states(&outs), vec![SessionState::WaitingApproval]);
    assert_eq!(mapper.pending_notices(), 1);

    let notice_id = raised[0].id.clone();
    let outs = mapper.on_notice_decision(&notice_id, Decision::ApproveForSession);
    assert_eq!(
        rpcs(&outs)[0],
        json!({"id": 9, "result": {"decision": "acceptForSession"}})
    );
    assert_eq!(
        resolved(&outs),
        vec![(notice_id, Some(Decision::ApproveForSession))]
    );
    assert_eq!(states(&outs), vec![SessionState::Running]);
    assert_eq!(mapper.pending_notices(), 0);
    assert_eq!(items(&outs)[0].status, ItemStatus::Running);
}

#[test]
fn reject_maps_to_decline() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 4,
        "method": "item/fileChange/requestApproval",
        "params": {"itemId": "f1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1",
                   "reason": "needs write access", "grantRoot": "/w"}
    }));
    let raised = notices(&outs);
    assert_eq!(raised[0].kind, NoticeKind::FileChange);
    let notice_id = raised[0].id.clone();
    let outs = mapper.on_notice_decision(&notice_id, Decision::Reject);
    assert_eq!(
        rpcs(&outs)[0],
        json!({"id": 4, "result": {"decision": "decline"}})
    );
}

#[test]
fn legacy_approval_methods_use_the_review_decision_vocabulary() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 11,
        "method": "applyPatchApproval",
        "params": {"callId": "call_1", "conversationId": "th_1", "fileChanges": {"a.txt": {}}}
    }));
    let raised = notices(&outs);
    assert_eq!(raised[0].kind, NoticeKind::FileChange);
    let notice_id = raised[0].id.clone();
    let outs = mapper.on_notice_decision(&notice_id, Decision::Approve);
    assert_eq!(
        rpcs(&outs)[0],
        json!({"id": 11, "result": {"decision": "approved"}})
    );

    let outs = mapper.on_rpc(json!({
        "id": 12,
        "method": "execCommandApproval",
        "params": {"callId": "call_2", "conversationId": "th_1", "command": ["ls", "-la"],
                   "cwd": "/w", "parsedCmd": []}
    }));
    let raised = notices(&outs);
    assert_eq!(raised[0].kind, NoticeKind::CommandExecution);
    assert_eq!(raised[0].body["command"], json!("ls -la"));
    let notice_id = raised[0].id.clone();
    let outs = mapper.on_notice_decision(&notice_id, Decision::Reject);
    assert_eq!(
        rpcs(&outs)[0],
        json!({"id": 12, "result": {"decision": "denied"}})
    );
}

#[test]
fn permissions_request_is_declined_without_fabricating_a_grant() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 21,
        "method": "item/permissions/requestApproval",
        "params": {"itemId": "p1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1",
                   "cwd": "/w", "permissions": {}}
    }));
    assert!(
        notices(&outs).is_empty(),
        "no notice may claim a grant we cannot issue"
    );
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["id"], json!(21));
    assert_eq!(outbound[0]["error"]["code"], json!(-32601));
    assert_eq!(mapper.pending_notices(), 0);
    assert_eq!(errors(&outs).len(), 1);
}

#[test]
fn unhandled_server_request_gets_a_jsonrpc_error() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 31,
        "method": "item/tool/requestUserInput",
        "params": {"itemId": "q1", "questions": []}
    }));
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["error"]["code"], json!(-32601));
    assert!(outbound[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("item/tool/requestUserInput"));
}

#[test]
fn approval_timeout_cancels_the_request_and_reports_it() {
    let mut mapper = CodexMapper::new(
        seed(),
        MapperConfig {
            approval_timeout: Duration::from_secs(1),
            ..config()
        },
    );
    mapper.on_rpc(json!({
        "id": 5,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1",
                   "command": "sleep 1000"}
    }));
    assert_eq!(mapper.pending_notices(), 1);
    let outs = mapper.expire_notices(Instant::now() + Duration::from_secs(2));
    assert_eq!(
        rpcs(&outs)[0],
        json!({"id": 5, "result": {"decision": "decline"}})
    );
    assert_eq!(resolved(&outs).len(), 1);
    assert!(errors(&outs)[0].contains("timed out"));
    assert_eq!(mapper.pending_notices(), 0);
}

#[test]
fn approvals_older_than_the_deadline_are_untouched_by_an_early_tick() {
    let mut mapper = CodexMapper::new(seed(), config());
    assert_eq!(config().approval_timeout, DEFAULT_APPROVAL_TIMEOUT);
    mapper.on_rpc(json!({
        "id": 5,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1", "command": "x"}
    }));
    assert!(mapper.expire_notices(Instant::now()).is_empty());
    assert_eq!(mapper.pending_notices(), 1);
}

#[test]
fn interrupt_sends_turn_interrupt_and_rejects_open_notices() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 6,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1", "command": "x"}
    }));
    let notice_id = notices(&outs)[0].id.clone();
    let outs = mapper.on_interrupt();
    let outbound = rpcs(&outs);
    assert_eq!(outbound[0]["method"], json!("turn/interrupt"));
    assert_eq!(outbound[0]["params"]["threadId"], json!("th_1"));
    assert_eq!(outbound[0]["params"]["turnId"], json!("tu_1"));
    assert_eq!(resolved(&outs), vec![(notice_id, Some(Decision::Reject))]);
}

#[test]
fn notice_decision_for_another_session_is_ignored_silently() {
    // The hub has no streamId on notice.respond, so every live session sees it.
    let mut mapper = started_mapper();
    let outs = mapper.on_notice_decision("ntc_missing", Decision::Approve);
    assert!(
        outs.is_empty(),
        "non-owner session must stay silent, got: {outs:?}"
    );
}

#[test]
fn turn_completion_reports_idle_and_resolves_leftover_notices() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 7,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1", "command": "x"}
    }));
    let notice_id = notices(&outs)[0].id.clone();
    let outs = mapper.on_rpc(json!({
        "method": "turn/completed",
        "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "completed", "items": []}}
    }));
    assert_eq!(resolved(&outs), vec![(notice_id, Some(Decision::Reject))]);
    assert_eq!(states(&outs), vec![SessionState::Idle]);
}

#[test]
fn failed_turn_reports_error_state() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "method": "turn/completed",
        "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "failed",
            "items": [], "error": {"message": "boom"}}}
    }));
    assert_eq!(states(&outs), vec![SessionState::Error]);
    assert_eq!(mapper.state(), SessionState::Error);
}

#[test]
fn interrupted_turn_closes_running_items_as_interrupted() {
    let mut mapper = started_mapper();
    mapper.on_rpc(json!({
        "method": "item/started",
        "params": {"item": {"type": "commandExecution", "id": "c1", "command": "sleep",
                            "cwd": "/w", "commandActions": [], "status": "inProgress"},
                   "threadId": "th_1", "turnId": "tu_1"}
    }));
    let outs = mapper.on_rpc(json!({
        "method": "turn/completed",
        "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "interrupted", "items": []}}
    }));
    let emitted = items(&outs);
    assert_eq!(emitted[0].status, ItemStatus::Interrupted);
    assert_eq!(states(&outs), vec![SessionState::Idle]);
}

#[test]
fn retrying_error_is_not_a_timeline_event_but_a_final_one_is() {
    let mut mapper = started_mapper();
    let retrying = mapper.on_rpc(json!({
        "method": "error",
        "params": {"error": {"message": "Reconnecting... 1/5"}, "threadId": "th_1",
                   "turnId": "tu_1", "willRetry": true}
    }));
    assert!(retrying.is_empty());
    let fatal = mapper.on_rpc(json!({
        "method": "error",
        "params": {"error": {"message": "401 Unauthorized"}, "threadId": "th_1",
                   "turnId": "tu_1", "willRetry": false}
    }));
    assert_eq!(errors(&fatal), vec![String::from("401 Unauthorized")]);
    let emitted = items(&fatal);
    assert_eq!(emitted[0].item_type, ItemType::System);
    assert_eq!(emitted[0].status, ItemStatus::Failed);
}

#[test]
fn server_request_resolved_notification_closes_the_notice() {
    let mut mapper = started_mapper();
    let outs = mapper.on_rpc(json!({
        "id": 8,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1", "command": "x"}
    }));
    let notice_id = notices(&outs)[0].id.clone();
    let outs = mapper.on_rpc(json!({
        "method": "serverRequest/resolved",
        "params": {"requestId": 8, "threadId": "th_1"}
    }));
    assert_eq!(resolved(&outs), vec![(notice_id, Some(Decision::Reject))]);
    assert_eq!(mapper.pending_notices(), 0);
}

#[test]
fn disconnect_reports_every_open_notice_as_rejected() {
    let mut mapper = started_mapper();
    mapper.on_rpc(json!({
        "id": 9,
        "method": "item/commandExecution/requestApproval",
        "params": {"itemId": "c1", "startedAtMs": 1, "threadId": "th_1", "turnId": "tu_1", "command": "x"}
    }));
    let outs = mapper.on_disconnect();
    assert_eq!(resolved(&outs).len(), 1);
    assert_eq!(mapper.pending_notices(), 0);
}

#[test]
fn unknown_notifications_and_garbage_are_ignored_without_state_change() {
    let mut mapper = started_mapper();
    assert!(mapper
        .on_rpc(json!({"method": "some/futureNotification", "params": {}}))
        .is_empty());
    assert!(mapper.on_rpc(json!({"id": 999, "result": {}})).is_empty());
    assert!(mapper.on_rpc(json!({"nonsense": true})).is_empty());
    assert_eq!(mapper.state(), SessionState::Running);
}

#[test]
fn fatal_request_failure_reports_error_and_exits() {
    let mut mapper = CodexMapper::new(seed(), config());
    mapper.begin();
    let outs = mapper.on_rpc(json!({"id": 1, "error": {"code": -32000, "message": "no auth"}}));
    let emitted = frames(&outs);
    assert!(emitted.iter().any(|frame| matches!(
        frame,
        ClientFrame::SessionExit { code: 1, stream_id } if stream_id == "str_1"
    )));
    assert_eq!(states(&outs), vec![SessionState::Error]);
    assert_eq!(errors(&outs).len(), 1);
}

#[tokio::test]
async fn session_driver_pumps_codex_frames_to_the_hub_until_eof() {
    let (transport, handle) = FakeTransport::pair();
    let (out_tx, mut out_rx) = unbounded_channel();
    let (_cmd_tx, cmd_rx) = unbounded_channel();
    let session = CodexSession::new(Box::new(transport), seed(), config(), out_tx, cmd_rx);
    let task = tokio::spawn(session.run());

    handle.push(json!({"id": 1, "result": {}}));
    handle.push(json!({"id": 2, "result": {"thread": {"id": "th_1"}}}));
    handle.push(json!({
        "method": "item/completed",
        "params": {"item": {"type": "agentMessage", "id": "i1", "text": "hi"}, "threadId": "th_1", "turnId": "tu_1"}
    }));
    handle.push(json!({
        "method": "turn/completed",
        "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "completed", "items": []}}
    }));
    handle.close();

    let mut collected = Vec::new();
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
        .await
        .expect("timed out waiting for hub frames")
    {
        collected.push(frame);
    }
    let report = task.await.expect("session task");
    let kinds: Vec<&str> = collected
        .iter()
        .map(|frame| match frame {
            ClientFrame::SessionStarted { .. } => "session.started",
            ClientFrame::SessionCapabilities { .. } => "session.capabilities",
            ClientFrame::SessionStateFrame { .. } => "session.state",
            ClientFrame::ItemUpsert { .. } => "item.upsert",
            ClientFrame::SessionExit { .. } => "session.exit",
            ClientFrame::Error { .. } => "error",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "session.state",
            "session.started",
            // v1.3: one report per stream, immediately after session.started.
            "session.capabilities",
            "item.upsert",
            "session.state",
            "session.exit"
        ]
    );
    assert!(matches!(
        collected.last(),
        Some(ClientFrame::SessionExit { code: 0, .. })
    ));
    assert_eq!(report.code, 0);

    // The handshake really went out on the wire.
    let sent = handle.sent();
    assert_eq!(sent[0]["method"], json!("initialize"));
    assert_eq!(sent[1]["method"], json!("initialized"));
    assert_eq!(sent[2]["method"], json!("thread/start"));
    assert_eq!(sent[3]["method"], json!("turn/start"));
}

#[tokio::test]
async fn session_driver_routes_hub_commands_into_codex_requests() {
    let (transport, handle) = FakeTransport::pair();
    let (out_tx, mut out_rx) = unbounded_channel();
    let (cmd_tx, cmd_rx) = unbounded_channel();
    let session = CodexSession::new(Box::new(transport), seed(), config(), out_tx, cmd_rx);
    let task = tokio::spawn(session.run());

    handle.push(json!({"id": 1, "result": {}}));
    handle.push(json!({"id": 2, "result": {"thread": {"id": "th_1"}}}));
    handle.push(json!({"id": 3, "result": {"turn": {"id": "tu_1"}}}));

    // Block until the handshake has been fully processed by the driver.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
            .await
            .expect("timed out waiting for session.started")
            .expect("channel closed");
        if matches!(frame, ClientFrame::SessionStarted { .. }) {
            break;
        }
    }

    cmd_tx
        .send(SessionCmd::Message {
            text: String::from("more"),
            client_message_id: String::from("cm_2"),
        })
        .unwrap();
    wait_for_sent(&handle, |frame| {
        frame["method"] == json!("turn/start")
            && frame["params"]["clientUserMessageId"] == json!("cm_2")
    })
    .await;

    cmd_tx.send(SessionCmd::Interrupt).unwrap();
    wait_for_sent(&handle, |frame| frame["method"] == json!("turn/interrupt")).await;

    handle.close();
    let _ = task.await.expect("session task");
}

async fn wait_for_sent(
    handle: &super::transport::fake::FakeHandle,
    predicate: impl Fn(&Value) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if handle.sent().iter().any(&predicate) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected outbound frame never appeared: {:?}",
            handle.sent()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn codex_capabilities_claim_only_what_the_mapper_can_do() {
    let ClientFrame::SessionCapabilities {
        stream_id,
        capabilities,
        catalogs,
        commands,
    } = crate::codex::capabilities_frame("str_9")
    else {
        panic!("codex must answer with session.capabilities");
    };
    assert_eq!(stream_id, "str_9");
    // turn/interrupt is a real app-server method the mapper sends.
    assert!(capabilities.interrupt);
    // No steer method, no command catalog, no catalog method for models or
    // permission profiles: claiming any of them would put dead controls in the UI.
    assert!(!capabilities.steer);
    assert!(!capabilities.selections);
    assert!(!capabilities.commands);
    assert!(catalogs.models.is_empty(), "no model list may be invented");
    assert!(catalogs.permissions.is_empty());
    assert!(commands.is_empty());
}
