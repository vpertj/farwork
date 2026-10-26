mod support;

use std::sync::Arc;

use farwork_timeline::*;
use support::{message, message_saying, LATER, NOW, SESSION};

async fn seed_and_read_back<S: TimelineStore>(store: &S) -> Result<Vec<i64>, TimelineError> {
    store.append(SESSION, message("a"), NOW).await?;
    store
        .upsert(SESSION, message_saying("a", "v2"), LATER)
        .await?;
    let recovery = store.events_after(SESSION, Cursor::ZERO).await?;
    match recovery {
        Recovery::Events { events, .. } => Ok(events.iter().map(|event| event.sequence).collect()),
        Recovery::RefetchRequired { reason, .. } => {
            panic!("unexpected refetch: {reason:?}")
        }
    }
}

#[tokio::test]
async fn the_store_trait_is_usable_generically() {
    let store = InMemoryTimeline::new();
    assert_eq!(seed_and_read_back(&store).await.unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn a_store_write_reports_the_same_outcomes_as_the_core_type() {
    let store = InMemoryTimeline::new();
    let created = store.upsert(SESSION, message("a"), NOW).await.unwrap();
    assert!(matches!(created, ApplyOutcome::Created { .. }));

    let repeat = store.upsert(SESSION, message("a"), NOW).await.unwrap();
    assert!(matches!(repeat, ApplyOutcome::NoChange { .. }));

    let duplicate = store.append(SESSION, message("a"), NOW).await.unwrap_err();
    assert!(matches!(duplicate, TimelineError::DuplicateItem { .. }));

    let missing = store
        .apply_update(SESSION, message("ghost"), NOW)
        .await
        .unwrap_err();
    assert!(matches!(missing, TimelineError::UnknownItem { .. }));
}

#[tokio::test]
async fn an_unknown_session_is_an_empty_timeline() {
    let store = InMemoryTimeline::new();
    let snapshot = store.snapshot("nobody").await.unwrap();
    assert!(snapshot.is_empty());
    assert_eq!(snapshot.cursor, Cursor::ZERO);
    assert_eq!(snapshot.session_id, "nobody");

    assert!(matches!(
        store.events_after("nobody", Cursor::ZERO).await.unwrap(),
        Recovery::Events { events, .. } if events.is_empty()
    ));
    assert!(matches!(
        store
            .events_after("nobody", Cursor::new(3).unwrap())
            .await
            .unwrap(),
        Recovery::RefetchRequired {
            reason: RefetchReason::CursorAhead {
                requested: 3,
                current: 0
            },
            ..
        }
    ));
}

#[tokio::test]
async fn sessions_do_not_share_state() {
    let store = InMemoryTimeline::new();
    store.append("s1", message("a"), NOW).await.unwrap();
    store.append("s2", message("a"), NOW).await.unwrap();
    store.append("s2", message("b"), NOW).await.unwrap();

    assert_eq!(store.snapshot("s1").await.unwrap().items.len(), 1);
    assert_eq!(store.snapshot("s2").await.unwrap().items.len(), 2);
    assert_eq!(store.snapshot("s1").await.unwrap().cursor.sequence(), 1);
    assert_eq!(store.snapshot("s2").await.unwrap().cursor.sequence(), 2);
    assert_eq!(store.tracked_sessions(), 2);
}

#[tokio::test]
async fn a_failed_write_leaves_the_committed_state_untouched() {
    let store = Arc::new(InMemoryTimeline::new());
    store.append(SESSION, message("a"), NOW).await.unwrap();

    let before = store.committed_view(SESSION);
    assert!(store.append(SESSION, message("a"), NOW).await.is_err());
    assert!(store
        .apply_update(SESSION, message("ghost"), NOW)
        .await
        .is_err());
    assert!(store
        .upsert(SESSION, message("b").with_order_seq(0), NOW)
        .await
        .is_err());

    let after = store.committed_view(SESSION);
    assert_eq!(before.sequence(), after.sequence());
    assert_eq!(before.items(), after.items());
    assert_eq!(after.events().len(), 1);

    store.append(SESSION, message("b"), NOW).await.unwrap();
    assert_eq!(store.committed_view(SESSION).items().len(), 2);
}

#[tokio::test]
async fn request_refetch_and_replace_all_go_through_the_store() {
    let store = InMemoryTimeline::new();
    store.append(SESSION, message("a"), NOW).await.unwrap();

    let event = store
        .request_refetch(SESSION, RefetchReason::Requested, LATER)
        .await
        .unwrap();
    assert_eq!(event.kind, EventType::SessionRefetchRequired);

    let replaced = store
        .replace_all(SESSION, vec![message("z")], LATER)
        .await
        .unwrap();
    assert_eq!(replaced.change_kind(), Some(ChangeKind::RefetchRequired));
    assert_eq!(store.snapshot(SESSION).await.unwrap().items.len(), 1);
    assert_eq!(store.committed_view(SESSION).refetch_floor(), 3);
}
