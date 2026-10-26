mod support;

use farwork_timeline::*;
use support::{message, message_saying, tool, LATER, NOW, SESSION};

fn sequences(recovery: &Recovery) -> Vec<i64> {
    match recovery {
        Recovery::Events { events, .. } => events.iter().map(|event| event.sequence).collect(),
        Recovery::RefetchRequired { reason, .. } => {
            panic!("expected an event suffix, got refetch: {reason:?}")
        }
    }
}

fn expect_events(recovery: Recovery) -> Vec<EventEnvelope> {
    match recovery {
        Recovery::Events { events, .. } => events,
        Recovery::RefetchRequired { reason, .. } => {
            panic!("expected an event suffix, got refetch: {reason:?}")
        }
    }
}

#[test]
fn events_after_returns_only_the_missing_suffix() {
    let mut timeline = Timeline::new(SESSION);
    for id in ["m1", "m2", "m3"] {
        timeline.append(message(id), NOW).unwrap();
    }

    let all = expect_events(timeline.events_after(Cursor::ZERO).unwrap());
    assert_eq!(all.len(), 3);
    assert_eq!(
        timeline
            .events_after(Cursor::ZERO)
            .unwrap()
            .next_cursor()
            .sequence(),
        3
    );

    let tail = expect_events(timeline.events_after(Cursor::new(2).unwrap()).unwrap());
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].sequence, 3);
    assert_eq!(tail[0].kind, EventType::TimelineItemCreated);

    let empty = expect_events(timeline.events_after(Cursor::new(3).unwrap()).unwrap());
    assert!(empty.is_empty());
}

#[test]
fn events_after_is_dense_and_duplicate_free_under_out_of_order_inserts() {
    let mut timeline = Timeline::new(SESSION);
    for (id, order_seq) in [
        ("m4", 4),
        ("m1", 1),
        ("m6", 6),
        ("m3", 3),
        ("m2", 2),
        ("m5", 5),
    ] {
        timeline
            .append(message(id).with_order_seq(order_seq), NOW)
            .unwrap();
    }
    let head = timeline.sequence();
    assert_eq!(head, 6);

    for after in 0..=head {
        let cursor = Cursor::new(after).unwrap();
        let observed = sequences(&timeline.events_after(cursor).unwrap());
        let expected: Vec<i64> = ((after + 1)..=head).collect();
        assert_eq!(observed, expected, "cursor seq:{after}");
    }

    let everything = expect_events(timeline.events_after(Cursor::ZERO).unwrap());
    let mut seen: Vec<i64> = everything.iter().map(|event| event.sequence).collect();
    seen.dedup();
    assert_eq!(seen.len(), everything.len(), "no duplicates");

    let committed_order: Vec<&str> = everything
        .iter()
        .map(|event| event.payload["item"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(committed_order, vec!["m4", "m1", "m6", "m3", "m2", "m5"]);
    assert_eq!(
        timeline
            .items()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1", "m2", "m3", "m4", "m5", "m6"]
    );
}

#[test]
fn interleaved_creates_and_updates_stay_contiguous() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("a"), NOW).unwrap();
    timeline.append(tool("t1"), NOW).unwrap();
    timeline.upsert(message_saying("a", "v2"), LATER).unwrap();
    timeline
        .upsert(tool("t1").with_status(ItemStatus::Done), LATER)
        .unwrap();
    timeline.append(message("b"), NOW).unwrap();

    assert_eq!(timeline.sequence(), 5);
    for (index, event) in timeline.events().iter().enumerate() {
        assert_eq!(event.sequence, index as i64 + 1);
        assert_eq!(event.cursor, Cursor::new(event.sequence).unwrap());
    }
    assert_eq!(
        sequences(&timeline.events_after(Cursor::new(3).unwrap()).unwrap()),
        vec![4, 5]
    );
    assert_eq!(timeline.events()[2].kind, EventType::TimelineItemUpdated);
}

#[test]
fn a_cursor_from_the_future_requires_a_refetch() {
    let mut timeline = Timeline::new(SESSION);
    timeline.append(message("m1"), NOW).unwrap();
    match timeline.events_after(Cursor::new(9).unwrap()).unwrap() {
        Recovery::RefetchRequired { reason, next } => {
            assert_eq!(
                reason,
                RefetchReason::CursorAhead {
                    requested: 9,
                    current: 1
                }
            );
            assert_eq!(next.sequence(), 1);
        }
        other => panic!("expected refetch, got {other:?}"),
    }
}
