//! Storage abstraction plus the in-memory reference implementation.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, PoisonError, RwLock};

use crate::cursor::Cursor;
use crate::error::TimelineError;
use crate::event::EventEnvelope;
use crate::fence::SessionFence;
use crate::item::ItemInput;
use crate::timeline::{
    ApplyOutcome, Recovery, RefetchReason, ReplaceOutcome, Timeline, TimelineSnapshot,
};

/// The operations a session timeline backend must provide.
///
/// Every write is exclusive per session and every read observes a committed state:
/// that pair is the whole contract, so a PostgreSQL implementation can fulfil it with
/// one transaction per write and `READ COMMITTED` reads, without changing a single
/// caller.
///
/// The trait returns `impl Future` rather than being object-safe on purpose. Making it
/// `dyn`-compatible would force `Box<dyn Future>` on every call, including the
/// streaming hot path, for the benefit of a runtime backend swap the hub can get from
/// an enum instead:
///
/// ```ignore
/// enum TimelineBackend { Memory(InMemoryTimeline), Postgres(PgTimeline) }
/// ```
pub trait TimelineStore {
    /// Insert a new item, refusing to overwrite an existing one.
    fn append(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send;

    /// Create the item if absent, otherwise update it. No-op when nothing changed.
    fn upsert(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send;

    /// Update an item that must already exist.
    fn apply_update(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send;

    /// Replace the whole projection in one commit.
    fn replace_all(
        &self,
        session_id: &str,
        inputs: Vec<ItemInput>,
        now: &str,
    ) -> impl Future<Output = Result<ReplaceOutcome, TimelineError>> + Send;

    /// Emit an explicit refetch signal without changing the projection.
    fn request_refetch(
        &self,
        session_id: &str,
        reason: RefetchReason,
        now: &str,
    ) -> impl Future<Output = Result<EventEnvelope, TimelineError>> + Send;

    /// The hole-free event suffix a client holding `cursor` is missing.
    fn events_after(
        &self,
        session_id: &str,
        cursor: Cursor,
    ) -> impl Future<Output = Result<Recovery, TimelineError>> + Send;

    /// A self-consistent snapshot at the current head.
    fn snapshot(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<TimelineSnapshot, TimelineError>> + Send;
}

/// A committed state a reader can hold while the writers keep going.
///
/// Wrapped in an `Arc` so taking a snapshot costs one atomic increment instead of a
/// deep copy, and so a reader that is halfway through serialising a response cannot be
/// affected by a concurrent write.
#[derive(Debug)]
struct SessionEntry {
    committed: RwLock<Arc<Timeline>>,
}

impl SessionEntry {
    fn new(session_id: &str) -> Self {
        Self {
            committed: RwLock::new(Arc::new(Timeline::new(session_id))),
        }
    }
}

/// The in-memory reference backend, used by the hub before PostgreSQL lands and by
/// every test in this crate.
///
/// ## Read policy (why a reader never sees half a write)
///
/// A writer clones the committed [Timeline], applies the change to the clone, and
/// swaps the clone in while holding the entry's write guard. Two consequences:
///
/// * the mutation is invisible until the swap, because the guard blocks readers for
///   the whole (synchronous, short) critical section; and
/// * a failed write drops the clone, so a rejected batch can never leave the log with
///   an event no item matches.
///
/// The cost is one deep copy per write. That is a deliberate trade: this backend exists
/// to make the semantics testable and to run the hub without a database, not to serve
/// throughput — a PostgreSQL backend gets the same isolation from MVCC for free.
///
/// ## Two locks, not one
///
/// [InMemoryTimeline::with_fence] serialises *composite* caller operations (e.g. "append
/// an item, then bump session metadata"). [TimelineStore] writes each take a separate,
/// shorter commit lock so they are atomic on their own. Keeping them separate is what
/// makes it impossible to deadlock by calling `upsert` inside `with_fence`, which is
/// exactly the trap a single non-reentrant lock would set.
#[derive(Debug, Default)]
pub struct InMemoryTimeline {
    sessions: RwLock<HashMap<String, Arc<SessionEntry>>>,
    commits: SessionFence,
    fences: SessionFence,
}

impl InMemoryTimeline {
    /// Create an empty backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `future` while holding the fence for `session_id`.
    ///
    /// See [SessionFence::with_fence] for the semantics and the non-reentrancy rule.
    pub async fn with_fence<F, T>(&self, session_id: &str, future: F) -> T
    where
        F: Future<Output = T>,
    {
        self.fences.with_fence(session_id, future).await
    }

    /// The committed [Timeline] for a session, as one consistent value.
    ///
    /// Exists so a caller that needs *both* a snapshot and an event suffix can read
    /// them from the same state instead of racing two separate calls. Cheap: it clones
    /// an `Arc`.
    ///
    /// Reading an unknown session yields an empty timeline rather than an error: a
    /// session's timeline legitimately starts empty, and forcing callers to create it
    /// first would only move the race.
    pub fn committed_view(&self, session_id: &str) -> Arc<Timeline> {
        self.entry(session_id)
            .committed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Number of sessions this backend has materialised; bounded by session count.
    pub fn tracked_sessions(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn entry(&self, session_id: &str) -> Arc<SessionEntry> {
        if let Some(entry) = self
            .sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
        {
            return entry.clone();
        }
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(SessionEntry::new(session_id)))
            .clone()
    }

    async fn commit<F>(&self, session_id: &str, operation: F) -> Result<ApplyOutcome, TimelineError>
    where
        F: FnOnce(&mut Timeline) -> Result<ApplyOutcome, TimelineError>,
    {
        self.commits
            .with_fence(session_id, async {
                let entry = self.entry(session_id);
                let mut committed = entry
                    .committed
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                let mut working = committed.as_ref().clone();
                let outcome = operation(&mut working)?;
                *committed = Arc::new(working);
                Ok(outcome)
            })
            .await
    }

    async fn commit_replace<F>(
        &self,
        session_id: &str,
        operation: F,
    ) -> Result<ReplaceOutcome, TimelineError>
    where
        F: FnOnce(&mut Timeline) -> Result<ReplaceOutcome, TimelineError>,
    {
        self.commits
            .with_fence(session_id, async {
                let entry = self.entry(session_id);
                let mut committed = entry
                    .committed
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                let mut working = committed.as_ref().clone();
                let outcome = operation(&mut working)?;
                *committed = Arc::new(working);
                Ok(outcome)
            })
            .await
    }

    async fn commit_event<F>(
        &self,
        session_id: &str,
        operation: F,
    ) -> Result<EventEnvelope, TimelineError>
    where
        F: FnOnce(&mut Timeline) -> Result<EventEnvelope, TimelineError>,
    {
        self.commits
            .with_fence(session_id, async {
                let entry = self.entry(session_id);
                let mut committed = entry
                    .committed
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                let mut working = committed.as_ref().clone();
                let event = operation(&mut working)?;
                *committed = Arc::new(working);
                Ok(event)
            })
            .await
    }
}

impl TimelineStore for InMemoryTimeline {
    fn append(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send {
        self.commit(session_id, move |timeline| timeline.append(input, now))
    }

    fn upsert(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send {
        self.commit(session_id, move |timeline| timeline.upsert(input, now))
    }

    fn apply_update(
        &self,
        session_id: &str,
        input: ItemInput,
        now: &str,
    ) -> impl Future<Output = Result<ApplyOutcome, TimelineError>> + Send {
        self.commit(session_id, move |timeline| {
            timeline.apply_update(input, now)
        })
    }

    fn replace_all(
        &self,
        session_id: &str,
        inputs: Vec<ItemInput>,
        now: &str,
    ) -> impl Future<Output = Result<ReplaceOutcome, TimelineError>> + Send {
        self.commit_replace(session_id, move |timeline| {
            timeline.replace_all(inputs, now)
        })
    }

    fn request_refetch(
        &self,
        session_id: &str,
        reason: RefetchReason,
        now: &str,
    ) -> impl Future<Output = Result<EventEnvelope, TimelineError>> + Send {
        self.commit_event(session_id, move |timeline| {
            timeline.request_refetch(reason, now)
        })
    }

    fn events_after(
        &self,
        session_id: &str,
        cursor: Cursor,
    ) -> impl Future<Output = Result<Recovery, TimelineError>> + Send {
        let view = self.committed_view(session_id);
        async move { view.events_after(cursor) }
    }

    fn snapshot(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<TimelineSnapshot, TimelineError>> + Send {
        let view = self.committed_view(session_id);
        async move { Ok(view.snapshot()) }
    }
}
