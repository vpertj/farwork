mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use farwork_timeline::*;
use serde_json::json;
use support::{message, message_saying, NOW, SESSION};

/// Every invariant a reader is allowed to assume about one committed state.
fn assert_consistent(view: &Timeline) {
    let head = view.sequence();
    assert_eq!(view.cursor().sequence(), head);

    let mut expected = 1;
    for event in view.events() {
        assert_eq!(event.sequence, expected, "the log must be dense");
        assert_eq!(event.cursor.sequence(), expected);
        expected += 1;
    }
    assert_eq!(expected - 1, head, "the head must equal the event count");

    let snapshot = view.snapshot();
    assert_eq!(snapshot.cursor.sequence(), head);
    assert_eq!(snapshot.items, view.items().to_vec());

    for item in &snapshot.items {
        let item_events: Vec<&EventEnvelope> = view
            .events()
            .iter()
            .filter(|event| event.payload["item"]["id"].as_str() == Some(item.id.as_str()))
            .collect();
        assert_eq!(
            item_events.len() as i64,
            item.revision,
            "one committed event per revision of {}",
            item.id
        );
        let last = item_events.last().expect("every item has an event");
        assert_eq!(last.payload["item"]["revision"], json!(item.revision));
        assert_eq!(last.payload["item"]["updatedSeq"], json!(item.updated_seq));
        assert_eq!(
            last.payload["item"]["contentHash"],
            json!(item.content_hash)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fence_serialises_writers_for_one_session() {
    let fence = Arc::new(SessionFence::new());
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let fence = Arc::clone(&fence);
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        handles.push(tokio::spawn(async move {
            let body = async move {
                let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(concurrent, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            };
            fence.with_fence(SESSION, body).await;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    assert_eq!(peak.load(Ordering::SeqCst), 1, "writers overlapped");
    assert_eq!(fence.tracked_sessions(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fences_are_independent_per_session() {
    let fence = Arc::new(SessionFence::new());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for session in ["s1", "s2"] {
        let fence = Arc::clone(&fence);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            let body = async move { barrier.wait().await };
            fence.with_fence(session, body).await;
        }));
    }
    let joined = tokio::time::timeout(Duration::from_secs(5), async {
        for handle in handles {
            handle.await.unwrap();
        }
    })
    .await;
    assert!(
        joined.is_ok(),
        "two sessions must be able to hold their fences at the same time"
    );
    assert_eq!(fence.tracked_sessions(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fenced_composite_write_cannot_deadlock_on_the_commit_lock() {
    let store = Arc::new(InMemoryTimeline::new());
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let inner = Arc::clone(&store);
        store
            .with_fence(SESSION, async move {
                inner.upsert(SESSION, message("a"), NOW).await.unwrap();
                inner.upsert(SESSION, message("b"), NOW).await.unwrap();
            })
            .await;
    })
    .await;
    assert!(
        result.is_ok(),
        "the commit lock and the caller fence must be different locks"
    );
    assert_eq!(store.snapshot(SESSION).await.unwrap().items.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_produce_a_dense_unique_log() {
    let store = Arc::new(InMemoryTimeline::new());
    let mut handles = Vec::new();
    for worker in 0..8 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for index in 0..20 {
                let id = format!("w{worker}-{index}");
                store.upsert(SESSION, message(&id), NOW).await.unwrap();
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let view = store.committed_view(SESSION);
    assert_eq!(view.sequence(), 160);
    assert_eq!(view.events().len(), 160);
    assert_eq!(view.items().len(), 160);
    for (index, event) in view.events().iter().enumerate() {
        assert_eq!(event.sequence, index as i64 + 1);
    }
    assert!(view.items().iter().all(|item| item.revision == 1));
    let ids: BTreeSet<&str> = view.items().iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids.len(), 160, "no writer lost or duplicated a slot");
    assert_consistent(&view);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_never_observe_a_partial_state() {
    let store = Arc::new(InMemoryTimeline::new());
    store.append(SESSION, message("a"), NOW).await.unwrap();

    let writer = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            for index in 0..60 {
                let text = if index % 2 == 0 { "even" } else { "odd" };
                store
                    .upsert(SESSION, message_saying("a", text), NOW)
                    .await
                    .unwrap();
            }
        })
    };
    let reader = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            let mut revisions = BTreeSet::new();
            for _ in 0..400 {
                let view = store.committed_view(SESSION);
                assert_consistent(&view);
                revisions.insert(view.item("a").unwrap().revision);
                tokio::task::yield_now().await;
            }
            revisions
        })
    };

    writer.await.unwrap();
    let revisions = reader.await.unwrap();
    assert!(
        revisions.len() > 1,
        "the reader never interleaved with the writer, so the test proved nothing"
    );
    let final_view = store.committed_view(SESSION);
    assert_consistent(&final_view);
    assert_eq!(final_view.sequence(), 61);
}
