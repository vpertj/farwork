mod support;

use farwork_timeline::*;
use serde_json::json;
use support::{message, NOW, SESSION};

#[test]
fn event_type_values_match_the_upstream_names() {
    let expected = [
        "session.subscribed",
        "session.meta.updated",
        "timeline.item_created",
        "timeline.item_updated",
        "timeline.snapshot",
        "runtime.state.updated",
        "runtime.notice.snapshot",
        "runtime.notice.updated",
        "runtime.capability.updated",
        "runtime.catalog.updated",
        "runtime.refetch_required",
        "session.refetch_required",
    ];
    assert_eq!(EventType::ALL.len(), expected.len());
    for (kind, name) in EventType::ALL.into_iter().zip(expected) {
        assert_eq!(kind.as_str(), name);
        assert_eq!(name.parse::<EventType>().unwrap(), kind);
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<EventType>(json!(name)).unwrap(),
            kind
        );
    }
    assert!(matches!(
        "runtime.unknown".parse::<EventType>(),
        Err(TimelineError::UnknownEnumValue {
            field: "event type",
            ..
        })
    ));
    assert!(EventType::SessionRefetchRequired.requires_refetch());
    assert!(EventType::TimelineSnapshot.carries_snapshot());
    assert!(!EventType::TimelineItemUpdated.requires_refetch());
}

#[test]
fn item_kind_values_match_the_upstream_wire_strings() {
    for (kind, name) in [
        (TimelineItemKind::Message, "message"),
        (TimelineItemKind::Tool, "tool"),
        (TimelineItemKind::Artifact, "artifact"),
        (TimelineItemKind::Marker, "marker"),
        (TimelineItemKind::System, "system"),
    ] {
        assert_eq!(kind.as_str(), name);
        assert_eq!(name.parse::<TimelineItemKind>().unwrap(), kind);
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
    }
    assert!(matches!(
        "audio".parse::<TimelineItemKind>(),
        Err(TimelineError::UnknownEnumValue { field: "type", .. })
    ));
}

#[test]
fn item_status_values_match_the_upstream_wire_strings() {
    for (status, name) in [
        (ItemStatus::Pending, "pending"),
        (ItemStatus::Running, "running"),
        (ItemStatus::WaitingApproval, "waiting_approval"),
        (ItemStatus::Done, "done"),
        (ItemStatus::Failed, "failed"),
        (ItemStatus::Cancelled, "cancelled"),
        (ItemStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(status.as_str(), name);
        assert_eq!(name.parse::<ItemStatus>().unwrap(), status);
        assert_eq!(serde_json::to_value(status).unwrap(), json!(name));
    }
}

#[test]
fn role_values_match_the_upstream_wire_strings() {
    for (role, name) in [
        (Role::User, "user"),
        (Role::Assistant, "assistant"),
        (Role::System, "system"),
        (Role::Tool, "tool"),
    ] {
        assert_eq!(role.as_str(), name);
        assert_eq!(name.parse::<Role>().unwrap(), role);
        assert_eq!(serde_json::to_value(role).unwrap(), json!(name));
    }
    assert!(matches!(
        "narrator".parse::<Role>(),
        Err(TimelineError::UnknownEnumValue { field: "role", .. })
    ));
}

#[test]
fn item_and_envelope_json_use_the_upstream_field_names() {
    let mut timeline = Timeline::new(SESSION);
    let outcome = timeline.append(message("m1"), NOW).unwrap();
    let item_json = serde_json::to_value(outcome.item()).unwrap();
    let object = item_json.as_object().unwrap();
    for key in [
        "id",
        "sessionId",
        "type",
        "status",
        "role",
        "orderSeq",
        "updatedSeq",
        "revision",
        "contentHash",
        "content",
        "createdAt",
        "updatedAt",
    ] {
        assert!(object.contains_key(key), "missing {key} in {item_json}");
    }
    assert!(
        !object.contains_key("payload"),
        "payload is content on the wire"
    );
    assert_eq!(
        serde_json::from_value::<TimelineItem>(item_json).unwrap(),
        *outcome.item()
    );

    let envelope_json = serde_json::to_value(outcome.event().unwrap()).unwrap();
    let envelope = envelope_json.as_object().unwrap();
    for key in [
        "eventId",
        "sequence",
        "cursor",
        "type",
        "sessionId",
        "emittedAt",
        "payload",
    ] {
        assert!(
            envelope.contains_key(key),
            "missing {key} in {envelope_json}"
        );
    }
    assert!(envelope_json["eventId"]
        .as_str()
        .unwrap()
        .starts_with("evt_1_"));
    assert_eq!(
        Some(&serde_json::from_value::<EventEnvelope>(envelope_json).unwrap()),
        outcome.event()
    );
}

#[test]
fn an_item_without_a_role_omits_the_field_but_still_decodes() {
    let mut timeline = Timeline::new(SESSION);
    let outcome = timeline
        .append(
            ItemInput::new("sys", TimelineItemKind::System, json!({ "note": "hi" })),
            NOW,
        )
        .unwrap();
    let value = serde_json::to_value(outcome.item()).unwrap();
    assert!(value.get("role").is_none(), "{value}");
    assert_eq!(
        serde_json::from_value::<TimelineItem>(value).unwrap(),
        *outcome.item()
    );
}
