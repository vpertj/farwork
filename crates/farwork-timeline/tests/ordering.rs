mod support;

use farwork_timeline::*;
use serde_json::json;
use support::{ids, message, message_saying, tool, LATER, NOW, SESSION};

#[test]
fn append_allocates_monotonic_order_seq() {
    let mut timeline = Timeline::new(SESSION);
    let first = timeline.append(message("m1"), NOW).unwrap();
    assert!(matches!(first, ApplyOutcome::Created { .. }));
    assert_eq!(first.change_kind(), Some(ChangeKind::Incremental));
    let item = first.item();
    assert_eq!(item.id, "m1");
    assert_eq!(item.session_id, SESSION);
    assert_eq!(item.order_seq, 1);
    assert_eq!(item.revision, 1);
    assert_eq!(item.updated_seq, 1);
    assert_eq!(item.created_at, NOW);
    assert_eq!(item.updated_at, NOW);
    assert_eq!(first.event().unwrap().kind, EventType::TimelineItemCreated);

    let second = timeline.append(message("m2"), NOW).unwrap();
    assert_eq!(second.item().order_seq, 2);
    assert_eq!(second.item().updated_seq, 2);
    assert_eq!(timeline.sequence(), 2);
    assert_eq!(timeline.cursor().to_wire(), "seq:2");
    assert_eq!(ids(timeline.items()), vec!["m1", "m2"]);
}

#[test]
fn explicit_order_seq_is_honoured_and_advances_the_allocator() {
    let mut timeline = Timeline::new(SESSION);
    let pinned = timeline
        .append(message("m1").with_order_seq(10), NOW)
        .unwrap();
    assert_eq!(pinned.item().order_seq, 10);

    let allocated = timeline.append(message("m2"), NOW).unwrap();
    assert_eq!(allocated.item().order_seq, 11);
}

#[test]
fn append_rejects_a_duplicate_id_without_consuming_a_sequence() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("m1"), NOW).unwrap();
    let error = timeline.append(message("m1"), NOW).unwrap_err();
    assert!(matches!(error, TimelineError::DuplicateItem { ref id, .. } if id == "m1"));
    assert_eq!(timeline.sequence(), 1);
    assert_eq!(timeline.items().len(), 1);
}

#[test]
fn append_rejects_an_unsortable_order_seq() {
    let mut timeline = Timeline::new(SESSION);
    let error = timeline
        .append(message("m1").with_order_seq(0), NOW)
        .unwrap_err();
    assert!(matches!(
        error,
        TimelineError::InvalidOrderSeq { order_seq: 0, .. }
    ));
    assert_eq!(timeline.sequence(), 0);
    assert!(timeline.events().is_empty());
}

#[test]
fn upsert_creates_then_bumps_revision_on_a_real_change() {
    let mut timeline = Timeline::new(SESSION);
    let created = timeline.upsert(message("m1"), NOW).unwrap();
    assert!(matches!(created, ApplyOutcome::Created { .. }));
    assert_eq!(created.item().revision, 1);
    assert_eq!(created.item().created_at, NOW);

    let updated = timeline
        .upsert(message_saying("m1", "hello"), LATER)
        .unwrap();
    assert!(matches!(updated, ApplyOutcome::Updated { .. }));
    let item = updated.item();
    assert_eq!(item.revision, 2);
    assert_eq!(item.updated_seq, 2);
    assert_eq!(item.created_at, NOW, "creation time must survive updates");
    assert_eq!(item.updated_at, LATER);
    assert_eq!(
        updated.event().unwrap().kind,
        EventType::TimelineItemUpdated
    );
    assert_eq!(timeline.items().len(), 1);
    assert_eq!(timeline.sequence(), 2);
}

#[test]
fn upsert_keeps_the_existing_slot_when_the_input_omits_order_seq() {
    let mut timeline = Timeline::new(SESSION);
    timeline
        .append(message("m1").with_order_seq(7), NOW)
        .unwrap();
    timeline
        .append(message("m2").with_order_seq(2), NOW)
        .unwrap();

    let updated = timeline
        .upsert(message_saying("m1", "changed"), LATER)
        .unwrap();
    assert_eq!(updated.item().order_seq, 7);
    assert_eq!(ids(timeline.items()), vec!["m2", "m1"]);
}

#[test]
fn an_update_can_move_an_item_and_the_projection_reorders() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(message("b"), NOW).unwrap();
    assert_eq!(ids(timeline.items()), vec!["a", "b"]);

    let moved = timeline
        .upsert(message_saying("a", "moved").with_order_seq(9), LATER)
        .unwrap();
    assert_eq!(moved.item().order_seq, 9);
    assert_eq!(moved.item().revision, 2);
    assert_eq!(ids(timeline.items()), vec!["b", "a"]);
}

#[test]
fn apply_update_refuses_an_unknown_item() {
    let mut timeline = Timeline::new(SESSION);
    let error = timeline.apply_update(message("ghost"), NOW).unwrap_err();
    assert!(matches!(error, TimelineError::UnknownItem { ref id, .. } if id == "ghost"));
    assert_eq!(timeline.sequence(), 0);

    timeline.append(message("m1"), NOW).unwrap();
    let outcome = timeline
        .apply_update(message_saying("m1", "next"), LATER)
        .unwrap();
    assert_eq!(outcome.item().revision, 2);
}

#[test]
fn items_sort_by_order_seq_then_by_id() {
    let mut timeline = Timeline::new(SESSION);
    timeline
        .append(message("m").with_order_seq(2), NOW)
        .unwrap();
    timeline
        .append(message("k").with_order_seq(2), NOW)
        .unwrap();
    timeline
        .append(message("n").with_order_seq(1), NOW)
        .unwrap();
    assert_eq!(ids(timeline.items()), vec!["n", "k", "m"]);
    assert_eq!(timeline.snapshot().items, timeline.items().to_vec());
}

#[test]
fn every_commit_reports_its_own_sequence_on_the_item_and_the_event() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(tool("t1"), NOW).unwrap();
    timeline.upsert(message_saying("a", "v2"), LATER).unwrap();

    for event in timeline.events() {
        assert_eq!(event.session_id, SESSION);
        assert_eq!(event.cursor.sequence(), event.sequence);
        let payload_item = event
            .payload
            .get("item")
            .expect("item events carry their item");
        assert_eq!(payload_item["updatedSeq"], json!(event.sequence));
    }
    assert_eq!(timeline.item("a").unwrap().updated_seq, 3);
    assert_eq!(timeline.item("a").unwrap().revision, 2);
    assert_eq!(timeline.item("t1").unwrap().updated_seq, 2);
}

#[test]
fn status_alone_counts_as_a_change() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(tool("t1"), NOW).unwrap();
    let running = timeline.item("t1").unwrap().clone();
    assert_eq!(running.status, ItemStatus::Running);
    assert!(!running.is_terminal());

    let mut finished = tool("t1");
    finished.status = ItemStatus::Done;
    let outcome = timeline.upsert(finished, LATER).unwrap();
    assert!(matches!(outcome, ApplyOutcome::Updated { .. }));
    assert_eq!(outcome.item().revision, 2);
    assert!(outcome.item().is_terminal());
}
