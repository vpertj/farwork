mod support;

use std::collections::BTreeMap;

use farwork_timeline::*;
use serde_json::json;
use support::{message, message_saying, tool, LATER, NOW, SESSION};

fn sequences(recovery: &Recovery) -> Vec<i64> {
    match recovery {
        Recovery::Events { events, .. } => events.iter().map(|event| event.sequence).collect(),
        Recovery::RefetchRequired { reason, .. } => {
            panic!("expected an event suffix, got refetch: {reason:?}")
        }
    }
}

#[test]
fn snapshot_cursor_and_event_log_agree() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(tool("t1"), NOW).unwrap();
    timeline
        .upsert(message_saying("a", "changed"), LATER)
        .unwrap();

    let snapshot = timeline.snapshot();
    assert_eq!(snapshot.session_id, SESSION);
    assert_eq!(snapshot.cursor, timeline.cursor());
    assert_eq!(snapshot.cursor.sequence(), timeline.events().len() as i64);
    assert_eq!(snapshot.items, timeline.items().to_vec());

    // Replaying the log from zero must reproduce exactly what the snapshot holds.
    let mut replayed: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for event in timeline.events() {
        match event.kind {
            EventType::TimelineItemCreated | EventType::TimelineItemUpdated => {
                let item = &event.payload["item"];
                replayed.insert(item["id"].as_str().unwrap().to_owned(), item.clone());
            }
            other => panic!("unexpected event in replay: {other}"),
        }
    }
    assert_eq!(replayed.len(), snapshot.items.len());
    for item in &snapshot.items {
        let replayed_item = replayed.get(&item.id).expect("item present in log");
        assert_eq!(replayed_item["revision"], json!(item.revision));
        assert_eq!(replayed_item["status"], json!(item.status.as_str()));
        assert_eq!(replayed_item["contentHash"], json!(item.content_hash));
        assert_eq!(replayed_item["content"], item.payload);
    }
}

#[test]
fn replace_all_rebuilds_with_one_snapshot_event() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(message("b"), NOW).unwrap();
    let stale_cursor = timeline.cursor();

    let outcome = timeline
        .replace_all(
            vec![message("a"), message_saying("b", "rewritten"), message("c")],
            LATER,
        )
        .unwrap();
    let ReplaceOutcome::Rebuilt { snapshot, event } = &outcome else {
        panic!("expected a rebuild, got {outcome:?}");
    };
    assert_eq!(outcome.change_kind(), Some(ChangeKind::RefetchRequired));
    assert_eq!(event.kind, EventType::TimelineSnapshot);
    assert_eq!(event.payload["items"].as_array().unwrap().len(), 3);
    assert_eq!(snapshot.items.len(), 3);
    assert_eq!(timeline.sequence(), 3);

    assert_eq!(timeline.item("a").unwrap().revision, 1);
    assert_eq!(timeline.item("a").unwrap().updated_seq, 1);
    assert_eq!(timeline.item("b").unwrap().revision, 2);
    assert_eq!(timeline.item("c").unwrap().revision, 1);
    assert_eq!(timeline.item("c").unwrap().order_seq, 3);

    match timeline.events_after(stale_cursor).unwrap() {
        Recovery::RefetchRequired { reason, .. } => {
            assert_eq!(reason, RefetchReason::TimelineReset { floor: 3 })
        }
        other => panic!("expected refetch, got {other:?}"),
    }
    assert!(matches!(
        timeline.events_after(timeline.cursor()).unwrap(),
        Recovery::Events { events, .. } if events.is_empty()
    ));
}

#[test]
fn replace_all_with_an_identical_projection_commits_nothing() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(message("b"), NOW).unwrap();

    let outcome = timeline
        .replace_all(vec![message("a"), message("b")], LATER)
        .unwrap();
    assert!(matches!(outcome, ReplaceOutcome::NoChange { .. }));
    assert_eq!(outcome.change_kind(), None);
    assert!(outcome.event().is_none());
    assert_eq!(timeline.sequence(), 2);
    assert_eq!(timeline.refetch_floor(), 0);
}

#[test]
fn replace_all_of_an_empty_projection_is_a_rebuild() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    let outcome = timeline.replace_all(Vec::new(), LATER).unwrap();
    assert!(matches!(outcome, ReplaceOutcome::Rebuilt { .. }));
    assert!(timeline.items().is_empty());
    assert_eq!(timeline.refetch_floor(), 2);
}

#[test]
fn pruning_the_log_raises_the_refetch_floor() {
    let mut timeline = Timeline::new(SESSION);
    for id in ["a", "b", "c", "d"] {
        timeline.append(message(id), NOW).unwrap();
    }
    timeline.prune_events_below(3).unwrap();
    assert_eq!(timeline.refetch_floor(), 3);
    assert_eq!(timeline.events().len(), 2);

    assert!(timeline
        .events_after(Cursor::new(2).unwrap())
        .unwrap()
        .is_refetch_required());
    assert_eq!(
        sequences(&timeline.events_after(Cursor::new(3).unwrap()).unwrap()),
        vec![4]
    );
    assert!(matches!(
        timeline.prune_events_below(9),
        Err(TimelineError::InvalidFloor {
            floor: 9,
            sequence: 4
        })
    ));
    assert_eq!(
        timeline.events().len(),
        2,
        "a rejected prune changes nothing"
    );
}

#[test]
fn reset_clears_the_projection_and_refuses_old_cursors() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(message("b"), NOW).unwrap();

    let event = timeline.reset(LATER).unwrap();
    assert_eq!(event.kind, EventType::SessionRefetchRequired);
    assert_eq!(event.payload["reason"], json!("timeline_reset"));
    assert_eq!(event.sequence, 3);
    assert!(timeline.items().is_empty());
    assert_eq!(timeline.refetch_floor(), 3);
    assert!(matches!(
        timeline.events_after(Cursor::new(2).unwrap()).unwrap(),
        Recovery::RefetchRequired {
            reason: RefetchReason::TimelineReset { floor: 3 },
            ..
        }
    ));
}

#[test]
fn a_requested_refetch_is_observable_without_changing_items() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    let event = timeline
        .request_refetch(RefetchReason::Requested, LATER)
        .unwrap();
    assert_eq!(event.kind, EventType::SessionRefetchRequired);
    assert_eq!(event.payload["reason"], json!("requested"));
    assert_eq!(event.payload["eventCursor"], json!("seq:2"));
    assert_eq!(timeline.items().len(), 1);
    assert_eq!(timeline.refetch_floor(), 0);
}

#[test]
fn an_incompatible_kind_change_signals_a_refetch_instead_of_a_lying_delta() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();

    let outcome = timeline.upsert(tool("a"), LATER).unwrap();
    let ApplyOutcome::RefetchRequired {
        item,
        reason,
        event,
    } = &outcome
    else {
        panic!("expected refetch, got {outcome:?}");
    };
    assert_eq!(item.kind, TimelineItemKind::Tool);
    assert_eq!(item.revision, 2);
    assert_eq!(
        *reason,
        RefetchReason::IncompatibleItemChange {
            item_id: "a".to_owned(),
            previous: TimelineItemKind::Message,
            next: TimelineItemKind::Tool,
        }
    );
    assert_eq!(reason.code(), "incompatible_item_change");
    assert_eq!(event.kind, EventType::SessionRefetchRequired);
    assert_eq!(event.payload["reason"], json!("incompatible_item_change"));
    assert_eq!(event.payload["eventCursor"], json!("seq:2"));
    assert_eq!(outcome.change_kind(), Some(ChangeKind::RefetchRequired));

    // The signal is an event in the log, not a hole: a client learns about it through
    // ordinary recovery instead of noticing missing data.
    assert_eq!(
        sequences(&timeline.events_after(Cursor::ZERO).unwrap()),
        vec![1, 2]
    );
    assert_eq!(timeline.refetch_floor(), 0);
    assert_eq!(timeline.item("a").unwrap().kind, TimelineItemKind::Tool);
}
