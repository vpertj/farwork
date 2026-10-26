//! Per-session exclusive write ordering.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Mutex as AsyncMutex;

/// An asynchronous, per-session exclusive lock.
///
/// Upstream calls this `session_fence` and takes it around any operation that must not
/// interleave with another writer for the same session (`api/shares.py:45`,
/// `api/sessions.py:1167`): allocate a sequence, stage the change, publish it, advance
/// the watermark. Reproducing it here means the atomicity argument moves with the
/// code rather than living in a router.
///
/// Design decisions worth knowing:
///
/// * **Not reentrant.** Taking the same session's fence twice in one task deadlocks.
///   Reentrancy would need task-local bookkeeping to distinguish "same task" from
///   "another task", and every caller that needed it would be a caller that had not
///   decided where its transaction boundary is. See
///   [crate::store::InMemoryTimeline] for how composite writes are expressed without
///   nesting: the store's own commit lock is a *different* lock, so wrapping several
///   store calls in a fence cannot self-deadlock.
/// * **Per session.** Two sessions never contend, so a slow session cannot stall
///   another; the map holds one mutex per session id for the lifetime of this value.
/// * **Fifo-ish but unspecified.** Tokio's mutex wakes waiters in acquisition order,
///   but nothing downstream may depend on it: the guarantee is exclusion, not fairness.
#[derive(Debug, Clone, Default)]
pub struct SessionFence {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

impl SessionFence {
    /// Create an empty fence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `future` while holding the exclusive lock for `session_id`.
    ///
    /// The guard is released when `future` completes, so a cancelled caller cannot
    /// leak the lock (unlike a hand-rolled acquire/release pair).
    pub async fn with_fence<F, T>(&self, session_id: &str, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let lock = self.lock_for(session_id);
        let guard = lock.lock().await;
        let output = future.await;
        drop(guard);
        output
    }

    /// Number of session locks currently tracked; useful for leak assertions in tests.
    pub fn tracked_sessions(&self) -> usize {
        self.locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn lock_for(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        locks
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}
