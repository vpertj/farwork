//! Ordered event batches, snapshot staging and checkpoints.
//!
//! The bridge pushes the whole visible inventory over one stream, so this module
//! owns the two things a consumer can get wrong:
//!
//! * `batchSeq` must be exactly +1. A gap means events were lost, and the only
//!   safe recovery is a fresh `runtime.sync.subscribe` — never "carry on from
//!   here", which would leave a hole in the timeline forever.
//! * a snapshot page set is assembled and checked (item count and id uniqueness)
//!   before a single item is forwarded. Half a snapshot is worse than none: it
//!   would look like a complete replacement that dropped somebody's history.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub const PROJECTION_VERSIONS: [u64; 2] = [2, 3];
/// Negotiated extensions: we ask for both, and only use them when echoed.
pub const CHECKPOINT_VERSION: u64 = 1;
pub const UPLOAD_PROGRESS_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("batch speaks projection {found} but the subscription is {expected}")]
    WrongProjection { found: u64, expected: u64 },
    #[error("batch {batch_seq} arrived out of order (expected {expected})")]
    OutOfOrder { batch_seq: u64, expected: u64 },
    #[error("batch belongs to stream {found}, not {expected}")]
    WrongStream { found: String, expected: String },
    #[error("batch carries no operations")]
    EmptyBatch,
    #[error("malformed batch: {reason}")]
    Malformed { reason: String },
    #[error("snapshot item {id:?} appears twice")]
    SnapshotDuplicateId { id: String },
    #[error("snapshot item has no id")]
    SnapshotItemWithoutId,
    #[error("snapshot commit declared {declared} items but {staged} were staged")]
    SnapshotCount { declared: usize, staged: usize },
    #[error("snapshot commit for {found} without a matching begin")]
    SnapshotNotOpen { found: String },
    #[error("no snapshot is open")]
    NoOpenSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub stream_id: String,
    pub batch_seq: u64,
    pub projection_version: u64,
    pub operations: Vec<Value>,
}

impl Batch {
    pub fn parse(params: &Value) -> Result<Self, SyncError> {
        let stream_id = params
            .get("streamId")
            .and_then(Value::as_str)
            .ok_or_else(|| SyncError::Malformed {
                reason: String::from("missing streamId"),
            })?;
        let batch_seq = params
            .get("batchSeq")
            .and_then(Value::as_u64)
            .ok_or_else(|| SyncError::Malformed {
                reason: String::from("missing batchSeq"),
            })?;
        let projection_version = params
            .get("projectionVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| SyncError::Malformed {
                reason: String::from("missing projectionVersion"),
            })?;
        let operations = params
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| SyncError::Malformed {
                reason: String::from("missing operations"),
            })?;
        if operations.is_empty() {
            return Err(SyncError::EmptyBatch);
        }
        Ok(Self {
            stream_id: stream_id.to_string(),
            batch_seq,
            projection_version,
            operations,
        })
    }
}

/// One live subscription. Replacing it (resubscribe) resets the batch sequence,
/// because the bridge numbers the new stream from 1 again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStream {
    stream_id: String,
    projection_version: u64,
    next_batch: u64,
}

impl SyncStream {
    pub fn new(stream_id: impl Into<String>, projection_version: u64) -> Self {
        Self {
            stream_id: stream_id.into(),
            projection_version,
            next_batch: 1,
        }
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn projection_version(&self) -> u64 {
        self.projection_version
    }

    pub fn expected_batch(&self) -> u64 {
        self.next_batch
    }

    /// Accept one batch. On success the sequence advances; on any mismatch the
    /// caller must resubscribe and discard whatever was staged.
    pub fn accept(&mut self, batch: &Batch) -> Result<(), SyncError> {
        if batch.stream_id != self.stream_id {
            return Err(SyncError::WrongStream {
                found: batch.stream_id.clone(),
                expected: self.stream_id.clone(),
            });
        }
        if batch.projection_version != self.projection_version {
            return Err(SyncError::WrongProjection {
                found: batch.projection_version,
                expected: self.projection_version,
            });
        }
        if batch.batch_seq != self.next_batch {
            return Err(SyncError::OutOfOrder {
                batch_seq: batch.batch_seq,
                expected: self.next_batch,
            });
        }
        self.next_batch += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct OpenSnapshot {
    session_id: String,
    snapshot_id: String,
    external_session_id: Option<String>,
    through_seq: i64,
    items: Vec<Value>,
    ids: HashSet<String>,
}

/// Pages of one capture, held until the commit proves the set is whole.
#[derive(Debug, Default)]
pub struct SnapshotStage {
    open: Option<OpenSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommittedSnapshot {
    pub session_id: String,
    pub snapshot_id: String,
    pub external_session_id: Option<String>,
    pub through_seq: i64,
    pub items: Vec<Value>,
}

impl SnapshotStage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open_session(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.session_id.as_str())
    }

    pub fn open_snapshot_id(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.snapshot_id.as_str())
    }

    /// A new `snapshot.begin` replaces any half-assembled capture: the contract
    /// says an interrupted capture is discarded, never merged.
    pub fn begin(
        &mut self,
        session_id: &str,
        snapshot_id: &str,
        through_seq: i64,
        external_session_id: Option<&str>,
    ) -> bool {
        let replaced = self.open.take().is_some();
        self.open = Some(OpenSnapshot {
            session_id: session_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            external_session_id: external_session_id.map(str::to_string),
            through_seq,
            items: Vec::new(),
            ids: HashSet::new(),
        });
        replaced
    }

    pub fn push(&mut self, items: &[Value]) -> Result<usize, SyncError> {
        let Some(open) = self.open.as_mut() else {
            return Err(SyncError::NoOpenSnapshot);
        };
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or(SyncError::SnapshotItemWithoutId)?;
            if !open.ids.insert(id.to_string()) {
                return Err(SyncError::SnapshotDuplicateId { id: id.to_string() });
            }
            open.items.push(item.clone());
        }
        Ok(open.items.len())
    }

    /// Validate the assembled capture: the declared count must match what was
    /// staged, otherwise the page set is incomplete and must not be forwarded.
    pub fn commit(
        &mut self,
        snapshot_id: &str,
        total_items: usize,
        through_seq: i64,
    ) -> Result<CommittedSnapshot, SyncError> {
        let staged = match self.open.as_ref() {
            Some(open) if open.snapshot_id == snapshot_id => open.items.len(),
            _ => {
                return Err(SyncError::SnapshotNotOpen {
                    found: snapshot_id.to_string(),
                })
            }
        };
        if staged != total_items {
            // A page set that does not add up is discarded wholesale: forwarding
            // it would look like a complete replacement that lost items.
            self.open = None;
            return Err(SyncError::SnapshotCount {
                declared: total_items,
                staged,
            });
        }
        let open = self.open.take().ok_or(SyncError::NoOpenSnapshot)?;
        Ok(CommittedSnapshot {
            session_id: open.session_id,
            snapshot_id: open.snapshot_id,
            external_session_id: open.external_session_id,
            through_seq: open.through_seq.max(through_seq),
            items: open.items,
        })
    }

    /// `snapshot.abort`: throw the partial capture away and report whether one
    /// was open.
    pub fn abort(&mut self, snapshot_id: &str) -> bool {
        match self.open.as_ref() {
            Some(open) if open.snapshot_id == snapshot_id => {
                self.open = None;
                true
            }
            _ => false,
        }
    }
}

/// Checkpoints received from the bridge, keyed by the native session id.
///
/// They are only an optimisation: a missing one costs a full snapshot, never
/// correctness, so they live in memory for the life of this process.
#[derive(Debug, Default)]
pub struct Checkpoints {
    entries: HashMap<String, Value>,
}

impl Checkpoints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(&self, external_session_id: &str) -> Option<Value> {
        self.entries.get(external_session_id).cloned()
    }

    pub fn save(&mut self, external_session_id: &str, checkpoint: Value) {
        self.entries
            .insert(external_session_id.to_string(), checkpoint);
    }

    pub fn delete(&mut self, external_session_id: &str) -> bool {
        self.entries.remove(external_session_id).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn batch(stream_id: &str, batch_seq: u64, projection_version: u64) -> Batch {
        Batch {
            stream_id: stream_id.to_string(),
            batch_seq,
            projection_version,
            operations: vec![json!({"kind": "notifications", "notifications": []})],
        }
    }

    #[test]
    fn batches_must_arrive_in_strict_sequence_and_a_gap_is_out_of_order() {
        let mut stream = SyncStream::new("str_1", 3);
        assert!(stream.accept(&batch("str_1", 1, 3)).is_ok());
        assert_eq!(stream.expected_batch(), 2);
        // A repeated batch is as broken as a skipped one: neither is "close enough".
        let error = stream.accept(&batch("str_1", 1, 3)).unwrap_err();
        assert_eq!(
            error,
            SyncError::OutOfOrder {
                batch_seq: 1,
                expected: 2
            }
        );
        let error = stream.accept(&batch("str_1", 5, 3)).unwrap_err();
        assert!(matches!(error, SyncError::OutOfOrder { batch_seq: 5, .. }));
        // Still expecting 2: a rejected batch never advances the cursor.
        assert_eq!(stream.expected_batch(), 2);
        assert!(stream.accept(&batch("str_1", 2, 3)).is_ok());
    }

    #[test]
    fn a_batch_from_another_stream_or_projection_is_refused() {
        let mut stream = SyncStream::new("str_1", 3);
        let error = stream.accept(&batch("str_other", 1, 3)).unwrap_err();
        assert!(matches!(error, SyncError::WrongStream { .. }));
        let error = stream.accept(&batch("str_1", 1, 2)).unwrap_err();
        assert_eq!(
            error,
            SyncError::WrongProjection {
                found: 2,
                expected: 3
            }
        );
    }

    #[test]
    fn a_batch_without_operations_or_identity_is_malformed() {
        assert!(matches!(
            Batch::parse(&json!({"streamId": "s", "batchSeq": 1})).unwrap_err(),
            SyncError::Malformed { .. }
        ));
        assert_eq!(
            Batch::parse(&json!({
                "streamId": "s", "batchSeq": 1, "projectionVersion": 3, "operations": [],
            }))
            .unwrap_err(),
            SyncError::EmptyBatch
        );
        let parsed = Batch::parse(&json!({
            "streamId": "s", "batchSeq": 7, "projectionVersion": 3,
            "operations": [{"kind": "notifications", "notifications": []}],
        }))
        .expect("parse");
        assert_eq!(parsed.batch_seq, 7);
        assert_eq!(parsed.projection_version, 3);
    }

    fn item(id: &str) -> Value {
        json!({ "id": id, "type": "message", "status": "done", "content": {"text": "hi"} })
    }

    #[test]
    fn a_snapshot_is_only_committed_when_the_item_count_matches() {
        let mut stage = SnapshotStage::new();
        assert!(
            !stage.begin("sess_1", "snap_1", 4, Some("ext-1")),
            "nothing to replace"
        );
        assert_eq!(stage.push(&[item("a"), item("b")]).expect("push"), 2);
        let error = stage.commit("snap_1", 3, 9).unwrap_err();
        assert_eq!(
            error,
            SyncError::SnapshotCount {
                declared: 3,
                staged: 2
            }
        );
        // The bad capture is dropped, not left half-open.
        assert!(!stage.is_open());
    }

    #[test]
    fn a_duplicate_item_id_invalidates_the_whole_snapshot() {
        let mut stage = SnapshotStage::new();
        stage.begin("sess_1", "snap_1", 0, None);
        stage.push(&[item("a")]).expect("push");
        let error = stage.push(&[item("a")]).unwrap_err();
        assert_eq!(
            error,
            SyncError::SnapshotDuplicateId {
                id: String::from("a")
            }
        );
        // An item without an id is not a timeline item at all.
        let mut stage = SnapshotStage::new();
        stage.begin("sess_1", "snap_1", 0, None);
        assert_eq!(
            stage.push(&[json!({"type": "message"})]).unwrap_err(),
            SyncError::SnapshotItemWithoutId
        );
    }

    #[test]
    fn a_complete_snapshot_commits_with_its_identity_and_through_seq() {
        let mut stage = SnapshotStage::new();
        stage.begin("sess_1", "snap_1", 4, Some("ext-1"));
        stage
            .push(&[item("a"), item("b"), item("c")])
            .expect("push");
        let committed = stage.commit("snap_1", 3, 7).expect("commit");
        assert_eq!(committed.items.len(), 3);
        assert_eq!(committed.session_id, "sess_1");
        assert_eq!(committed.external_session_id.as_deref(), Some("ext-1"));
        assert_eq!(committed.through_seq, 7);
        assert!(!stage.is_open());
    }

    #[test]
    fn a_new_begin_replaces_a_half_assembled_capture_and_abort_drops_it() {
        let mut stage = SnapshotStage::new();
        stage.begin("sess_1", "snap_1", 0, None);
        stage.push(&[item("a")]).expect("push");
        assert!(stage.begin("sess_1", "snap_2", 0, None), "replaces");
        assert_eq!(stage.open_session(), Some("sess_1"));
        assert_eq!(stage.push(&[item("b")]).expect("push"), 1);
        assert!(!stage.abort("snap_1"), "a stale abort is ignored");
        assert!(stage.abort("snap_2"));
        assert!(!stage.is_open());
        assert_eq!(
            stage.commit("snap_2", 1, 0).unwrap_err(),
            SyncError::SnapshotNotOpen {
                found: String::from("snap_2")
            }
        );
    }

    #[test]
    fn checkpoints_are_kept_per_native_session_and_can_be_deleted() {
        let mut checkpoints = Checkpoints::new();
        assert!(checkpoints.is_empty());
        checkpoints.save("ext-1", json!({"throughSeq": 4}));
        assert_eq!(checkpoints.load("ext-1"), Some(json!({"throughSeq": 4})));
        assert_eq!(checkpoints.load("ext-2"), None);
        assert_eq!(checkpoints.len(), 1);
        assert!(checkpoints.delete("ext-1"));
        assert!(!checkpoints.delete("ext-1"));
        assert!(checkpoints.is_empty());
    }
}
