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
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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

/// Checkpoints the bridge asks us to keep, keyed by the native session id.
///
/// The bridge drives this: it asks `checkpoint.load` for a session before
/// deciding how to bring it up to date, and hands us `checkpoint.save` once it
/// has streamed one. The checkpoint is the *client's* durable state — keeping it
/// only in memory turns every reconnect into a full inventory replay, which is
/// exactly what overflows the bridge's own 10k-event queue on an instance with
/// dozens of sessions.
///
/// It is still only an optimisation: an unreadable, corrupt or foreign file is
/// read as "no checkpoint". A full replay is slow, never wrong.
#[derive(Debug)]
pub struct Checkpoints {
    dir: Option<PathBuf>,
    entries: HashMap<String, Value>,
}

impl Default for Checkpoints {
    fn default() -> Self {
        Self::in_memory()
    }
}

/// `$FARWORK_HOME/dsh/sync/checkpoints` — beside the agent identity, and
/// deliberately not in `~/.agents-anywhere`, which belongs to the DSH plugin.
pub fn default_dir() -> PathBuf {
    crate::identity::home()
        .join("dsh")
        .join("sync")
        .join("checkpoints")
}

const CHECKPOINT_SUFFIX: &str = ".json";

impl Checkpoints {
    /// Nothing is persisted: the next connection replays every session in full.
    pub fn in_memory() -> Self {
        Self {
            dir: None,
            entries: HashMap::new(),
        }
    }

    /// Read whatever an earlier process left in `dir`; unusable files are
    /// skipped (one bad file must not blind us to the rest).
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let entries = read_entries(&dir);
        Self {
            dir: Some(dir),
            entries,
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn load(&self, external_session_id: &str) -> Option<Value> {
        self.entries.get(external_session_id).cloned()
    }

    pub fn save(&mut self, external_session_id: &str, checkpoint: Value) {
        if !is_usable_id(external_session_id) {
            // The id names the file, so it is peer-controlled: anything that is
            // not one plain path component could escape the directory.
            tracing::warn!(
                external_session_id,
                "refusing to name a dsh checkpoint after this session id"
            );
            return;
        }
        self.entries
            .insert(external_session_id.to_string(), checkpoint.clone());
        let Some(path) = self.file_for(external_session_id) else {
            return;
        };
        if let Err(error) = write_atomic(&path, &checkpoint) {
            // Keeping the in-memory copy is still worth it for this process; the
            // operator has to know the next connection pays a full replay again.
            tracing::warn!(
                %error,
                path = %path.display(),
                "dsh checkpoint not persisted; the next connection replays this session in full"
            );
        }
    }

    pub fn delete(&mut self, external_session_id: &str) -> bool {
        let removed = self.entries.remove(external_session_id).is_some();
        if let Some(path) = self.file_for(external_session_id) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    %error,
                    path = %path.display(),
                    "cannot delete a dsh checkpoint file"
                ),
            }
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn file_for(&self, external_session_id: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        if !is_usable_id(external_session_id) {
            return None;
        }
        Some(dir.join(format!("{external_session_id}{CHECKPOINT_SUFFIX}")))
    }
}

/// One checkpoint per session is one file, so the id must be exactly one plain
/// path component: no separators, no dot-only names, nothing that a peer could
/// use to point outside the directory.
fn is_usable_id(external_session_id: &str) -> bool {
    !external_session_id.is_empty()
        && external_session_id.len() <= 200
        && !external_session_id.starts_with('.')
        && !external_session_id.contains(['/', '\\', '\0'])
}

fn read_entries(dir: &Path) -> HashMap<String, Value> {
    let mut entries = HashMap::new();
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == ErrorKind::NotFound => return entries,
        Err(error) => {
            tracing::warn!(%error, path = %dir.display(), "cannot read the dsh checkpoint directory");
            return entries;
        }
    };
    for entry in listing.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // Temp files from an interrupted write do not end in ".json", so a
        // half-written checkpoint is never mistaken for a whole one.
        let Some(id) = name.strip_suffix(CHECKPOINT_SUFFIX) else {
            continue;
        };
        if !is_usable_id(id) {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()));
        match parsed {
            Ok(value) => {
                entries.insert(id.to_string(), value);
            }
            Err(reason) => tracing::warn!(
                path = %path.display(),
                %reason,
                "ignoring an unusable dsh checkpoint file; that session replays in full"
            ),
        }
    }
    entries
}

/// Write to a temp file in the same directory, then rename over the target.
///
/// A checkpoint that is read while half written would make the bridge resume from
/// a boundary it never confirmed, so the rename is what makes it safe to have one
/// at all.
fn write_atomic(path: &Path, value: &Value) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "checkpoint path has no directory",
        ));
    };
    std::fs::create_dir_all(parent)?;
    crate::identity::restrict_dir(parent);
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("checkpoint.json");
    let temp = parent.join(format!(".{stem}.tmp.{}", std::process::id()));
    let body = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?;
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        crate::identity::restrict_file(&temp);
        file.write_all(&body)?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
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

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "farwork-agent-checkpoints-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn checkpoint(through_seq: i64) -> Value {
        json!({
            "historyHash": "e6dec046d543a40bd295808a46743585be1d73269731856f8cf9d44fb61418d9",
            "projectionVersion": 3,
            "settled": true,
            "throughSeq": through_seq,
            "version": 1,
        })
    }

    #[test]
    fn a_checkpoint_survives_the_process_that_saved_it() {
        let dir = temp_dir("reuse");
        let mut first = Checkpoints::open(&dir);
        assert!(first.is_empty());
        first.save("session-a", checkpoint(8624));
        drop(first);

        // A fresh store over the same directory is what the next connection gets:
        // without this the bridge replays all ~30 sessions in full.
        let second = Checkpoints::open(&dir);
        assert_eq!(second.load("session-a"), Some(checkpoint(8624)));
        assert_eq!(second.len(), 1);
        // A different externalSessionId is a different file, not a shared one.
        assert_eq!(second.load("session-b"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_checkpoint_removes_its_file_too() {
        let dir = temp_dir("delete");
        let mut checkpoints = Checkpoints::open(&dir);
        checkpoints.save("session-a", checkpoint(4));
        let path = dir.join("session-a.json");
        assert!(path.exists());
        assert!(checkpoints.delete("session-a"));
        assert!(!path.exists(), "the file is what makes it durable");
        assert!(!checkpoints.delete("session-a"));
        assert!(checkpoints.is_empty());
        assert_eq!(Checkpoints::open(&dir).load("session-a"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_holds_the_bridges_bytes_and_no_temp_file_is_left_behind() {
        let dir = temp_dir("atomic");
        let mut checkpoints = Checkpoints::open(&dir);
        checkpoints.save("session-a", checkpoint(7));
        let raw = std::fs::read_to_string(dir.join("session-a.json")).expect("file");
        let parsed: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(parsed, checkpoint(7), "the value is stored verbatim");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name != "session-a.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "an atomic replace leaves no partial file: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_checkpoint_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perms");
        let mut checkpoints = Checkpoints::open(&dir);
        checkpoints.save("session-a", checkpoint(1));
        let file = std::fs::metadata(dir.join("session-a.json")).expect("file");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let parent = std::fs::metadata(&dir).expect("dir");
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_or_unparsable_file_reads_as_no_checkpoint() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("session-broken.json"), "{ this is not json").expect("write");
        // A file whose name cannot be a session id is not a checkpoint either.
        std::fs::write(dir.join("notes.txt"), "{}").expect("write");
        let mut checkpoints = Checkpoints::open(&dir);
        assert_eq!(checkpoints.load("session-broken"), None);
        assert!(checkpoints.is_empty());
        // And the store still works afterwards instead of latching the failure.
        checkpoints.save("session-ok", checkpoint(2));
        assert_eq!(
            Checkpoints::open(&dir).load("session-ok"),
            Some(checkpoint(2))
        );
        assert_eq!(Checkpoints::open(&dir).load("session-broken"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_id_that_is_a_path_is_refused_instead_of_escaping_the_directory() {
        let dir = temp_dir("escape");
        let mut checkpoints = Checkpoints::open(&dir);
        for hostile in ["../../etc/passwd", "a/b", ".", "..", ".hidden", ""] {
            checkpoints.save(hostile, checkpoint(1));
            assert_eq!(
                checkpoints.load(hostile),
                None,
                "{hostile} must not be stored"
            );
        }
        assert!(checkpoints.is_empty());
        // Nothing was written anywhere: the store refused before touching a path.
        let created: Vec<String> = std::fs::read_dir(&dir)
            .map(|listing| {
                listing
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(created.is_empty(), "nothing was written: {created:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoints_live_under_farwork_home_and_not_in_the_dsh_plugin_directory() {
        let dir = default_dir();
        let text = dir.to_string_lossy().to_string();
        assert!(text.ends_with("dsh/sync/checkpoints"), "{text}");
        assert!(
            !text.contains(".agents-anywhere"),
            "the plugin's own state directory is not ours to write: {text}"
        );
    }

    #[test]
    fn an_in_memory_store_keeps_working_without_touching_the_disk() {
        let mut checkpoints = Checkpoints::in_memory();
        assert_eq!(checkpoints.dir(), None);
        checkpoints.save("ext-1", json!({"throughSeq": 4}));
        assert_eq!(checkpoints.load("ext-1"), Some(json!({"throughSeq": 4})));
        assert!(checkpoints.delete("ext-1"));
        assert!(checkpoints.is_empty());
        // default() is the in-memory store: a session with no directory configured
        // must not silently write into somebody's home.
        assert_eq!(Checkpoints::default().dir(), None);
    }
}
