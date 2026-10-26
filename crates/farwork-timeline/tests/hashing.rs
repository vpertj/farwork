mod support;

use farwork_timeline::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use support::{message, message_saying, NOW, SESSION};

fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

#[test]
fn canonical_json_sorts_object_keys() {
    let left = json!({ "b": 1, "a": { "d": [1, 2], "c": true } });
    let right = json!({ "a": { "c": true, "d": [1, 2] }, "b": 1 });
    assert_eq!(canonical_json(&left), canonical_json(&right));
    assert_eq!(canonical_json(&left), r#"{"a":{"c":true,"d":[1,2]},"b":1}"#);
}

#[test]
fn canonical_json_keeps_array_order_because_arrays_are_ordered_content() {
    let ascending = json!({ "parts": [1, 2] });
    let descending = json!({ "parts": [2, 1] });
    assert_ne!(canonical_json(&ascending), canonical_json(&descending));
}

#[test]
fn canonical_json_escapes_like_serde_and_python() {
    let value = json!({ "s": "quote\" slash\\ newline\n tab\t control\u{1}" });
    assert_eq!(
        canonical_json(&value),
        r#"{"s":"quote\" slash\\ newline\n tab\t control\u0001"}"#
    );
    assert_eq!(canonical_json(&json!("héllo")), "\"héllo\"");
}

#[test]
fn state_hash_matches_the_documented_identity_bytes() {
    let payload = json!({ "text": "hi" });
    // The identity is exactly upstream's contentHash input:
    // {type, status, role, content} canonicalised with sorted keys.
    let expected_bytes =
        r#"{"content":{"text":"hi"},"role":"assistant","status":"done","type":"message"}"#;
    assert_eq!(
        state_hash(
            TimelineItemKind::Message,
            ItemStatus::Done,
            Some(Role::Assistant),
            &payload
        ),
        sha256_hex(expected_bytes)
    );

    let anonymous = r#"{"content":{"text":"hi"},"role":null,"status":"done","type":"message"}"#;
    assert_eq!(
        state_hash(TimelineItemKind::Message, ItemStatus::Done, None, &payload),
        sha256_hex(anonymous)
    );
}

#[test]
fn state_hash_covers_status_role_and_kind() {
    let payload = json!({ "text": "same" });
    let base = state_hash(
        TimelineItemKind::Message,
        ItemStatus::Done,
        Some(Role::Assistant),
        &payload,
    );
    assert_ne!(
        base,
        state_hash(
            TimelineItemKind::Message,
            ItemStatus::Failed,
            Some(Role::Assistant),
            &payload
        ),
        "a status transition must not look unchanged"
    );
    assert_ne!(
        base,
        state_hash(
            TimelineItemKind::Message,
            ItemStatus::Done,
            Some(Role::User),
            &payload
        )
    );
    assert_ne!(
        base,
        state_hash(
            TimelineItemKind::Tool,
            ItemStatus::Done,
            Some(Role::Assistant),
            &payload
        )
    );
}

#[test]
fn state_hash_ignores_order_seq_because_position_is_not_content() {
    let placed = message("m1").with_order_seq(3);
    let elsewhere = message("m1").with_order_seq(41);
    assert_eq!(placed.state_hash(), elsewhere.state_hash());
}

#[test]
fn repeating_identical_state_commits_nothing() {
    let mut timeline = Timeline::new(SESSION);
    let first = timeline.upsert(message("m1"), NOW).unwrap();
    assert_eq!(first.item().revision, 1);

    for _ in 0..5 {
        let repeat = timeline.upsert(message("m1"), NOW).unwrap();
        assert!(matches!(repeat, ApplyOutcome::NoChange { .. }));
        assert_eq!(repeat.change_kind(), None);
        assert!(repeat.event().is_none());
        assert_eq!(repeat.item().revision, 1);
    }
    assert_eq!(timeline.sequence(), 1, "no-ops must not consume a sequence");
    assert_eq!(timeline.events().len(), 1);
    assert_eq!(
        timeline
            .events_after(Cursor::ZERO)
            .unwrap()
            .next_cursor()
            .sequence(),
        1
    );
}

#[test]
fn identical_content_written_with_a_different_key_order_is_a_no_op() {
    let mut timeline = Timeline::new(SESSION);
    timeline
        .upsert(
            ItemInput::new("m1", TimelineItemKind::Message, json!({ "a": 1, "b": 2 })),
            NOW,
        )
        .unwrap();
    let repeat = timeline
        .upsert(
            ItemInput::new("m1", TimelineItemKind::Message, json!({ "b": 2, "a": 1 })),
            NOW,
        )
        .unwrap();
    assert!(matches!(repeat, ApplyOutcome::NoChange { .. }));
}

#[test]
fn an_explicit_reorder_of_unchanged_content_is_still_a_commit() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("m1"), NOW).unwrap();
    let moved = timeline
        .upsert(message_saying("m1", "m1").with_order_seq(5), NOW)
        .unwrap();
    assert!(matches!(moved, ApplyOutcome::Updated { .. }));
    assert_eq!(moved.item().revision, 2);
    assert_eq!(moved.item().order_seq, 5);
}
