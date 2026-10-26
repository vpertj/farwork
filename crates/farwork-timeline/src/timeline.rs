//! The ordered timeline, its event log, and the rules that connect them.

use serde_json::{json, Value};

use crate::cursor::Cursor;
use crate::error::TimelineError;
use crate::event::{EventEnvelope, EventType};
use crate::item::{ItemInput, TimelineItem, TimelineItemKind};
use crate::PROTOCOL_MAX_REVISION;

/// Whether one change can be delivered as a per-item delta.
///
/// This is the judgement call the whole module exists for. A client that receives a
/// delta applies it to a projection it already holds; if the change cannot be
/// expressed that way, sending *something close* corrupts the projection silently and
/// the only recovery is a full reload the user did not ask for. So the write path
/// classifies first and emits a refetch signal when the answer is "no".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// A single item was created or updated; the event payload describes that item
    /// completely, so the client can patch its projection.
    Incremental,
    /// The change is not a per-item delta. The emitted event is either a whole-state
    /// snapshot or an explicit `*refetch_required`; a client must rebuild rather
    /// than patch.
    RefetchRequired,
}

impl ChangeKind {
    /// Whether the change is a per-item delta.
    pub const fn is_incremental(self) -> bool {
        matches!(self, Self::Incremental)
    }
}

/// Why a client is being told to rebuild its projection instead of patching it.
///
/// The variant is not just diagnostic: it is what makes a refetch signal actionable.
/// "Ahead of the server" means the client kept a cursor from a different incarnation
/// and must resnapshot rather than wait; "reset" means the log it is asking about no
/// longer describes the timeline it would patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefetchReason {
    /// The client's cursor is beyond anything this timeline ever committed.
    CursorAhead {
        /// The cursor the client asked from.
        requested: i64,
        /// The newest sequence this timeline committed.
        current: i64,
    },
    /// The log below `floor` was reset or pruned, so the client's cursor points into
    /// history that no longer describes the current projection.
    TimelineReset {
        /// Lowest sequence whose successor events are still meaningful.
        floor: i64,
    },
    /// An item changed kind, which no delta can express: the client's renderer for
    /// that id has to be built anew.
    IncompatibleItemChange {
        /// The id whose kind changed.
        item_id: String,
        /// Kind the client already rendered.
        previous: TimelineItemKind,
        /// Kind the runtime now reports.
        next: TimelineItemKind,
    },
    /// The runtime or an operator asked for a rebuild without a structural cause.
    Requested,
}

impl RefetchReason {
    /// Stable machine-readable code, also used inside event payloads.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CursorAhead { .. } => "cursor_ahead",
            Self::TimelineReset { .. } => "timeline_reset",
            Self::IncompatibleItemChange { .. } => "incompatible_item_change",
            Self::Requested => "requested",
        }
    }

    /// The JSON body describing this reason.
    ///
    /// Field names are camelCase to match upstream wire conventions; `eventCursor` is
    /// added by the emitter because only it knows the sequence.
    pub fn to_payload(&self) -> Value {
        match self {
            Self::CursorAhead { requested, current } => json!({
                "reason": self.code(),
                "requested": requested,
                "current": current,
            }),
            Self::TimelineReset { floor } => json!({
                "reason": self.code(),
                "floor": floor,
            }),
            Self::IncompatibleItemChange {
                item_id,
                previous,
                next,
            } => json!({
                "reason": self.code(),
                "itemId": item_id,
                "previousType": previous.as_str(),
                "nextType": next.as_str(),
            }),
            Self::Requested => json!({ "reason": self.code() }),
        }
    }
}

/// What a write did.
///
/// [ApplyOutcome::NoChange] is a first-class outcome, not an error and not a silent
/// no-op: the runtime re-reports item state constantly, and a caller that treats
/// "nothing changed" as "failed" would retry forever. It carries the item so the
/// caller still has the canonical (already committed) row to echo back.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    /// The reported state matched the committed state byte for byte.
    ///
    /// No sequence was consumed, so no event exists for it.
    NoChange {
        /// The already committed item.
        item: TimelineItem,
    },
    /// A new item was committed.
    Created {
        /// The committed item, with `revision` 1.
        item: TimelineItem,
        /// The `timeline.item_created` event announcing it.
        event: EventEnvelope,
    },
    /// An existing item changed and the delta is applicable.
    Updated {
        /// The committed item, at its new revision.
        item: TimelineItem,
        /// The `timeline.item_updated` event announcing it.
        event: EventEnvelope,
    },
    /// The change was committed but cannot be patched into a client projection.
    RefetchRequired {
        /// The committed item, at its new revision.
        item: TimelineItem,
        /// Cause of the refetch signal.
        reason: RefetchReason,
        /// The `session.refetch_required` event carrying [RefetchReason::code].
        event: EventEnvelope,
    },
}

impl ApplyOutcome {
    /// The committed item in every variant.
    pub fn item(&self) -> &TimelineItem {
        match self {
            Self::NoChange { item }
            | Self::Created { item, .. }
            | Self::Updated { item, .. }
            | Self::RefetchRequired { item, .. } => item,
        }
    }

    /// The event this write produced, if it produced one.
    pub fn event(&self) -> Option<&EventEnvelope> {
        match self {
            Self::NoChange { .. } => None,
            Self::Created { event, .. }
            | Self::Updated { event, .. }
            | Self::RefetchRequired { event, .. } => Some(event),
        }
    }

    /// Whether anything was committed.
    pub const fn changed(&self) -> bool {
        !matches!(self, Self::NoChange { .. })
    }

    /// How the change has to be delivered to clients.
    pub const fn change_kind(&self) -> Option<ChangeKind> {
        match self {
            Self::NoChange { .. } => None,
            Self::Created { .. } | Self::Updated { .. } => Some(ChangeKind::Incremental),
            Self::RefetchRequired { .. } => Some(ChangeKind::RefetchRequired),
        }
    }
}

/// What a whole-projection replacement did.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplaceOutcome {
    /// The incoming projection was identical to the committed one; nothing happened.
    NoChange {
        /// The unchanged committed state.
        snapshot: TimelineSnapshot,
    },
    /// The projection was rebuilt and announced with one `timeline.snapshot` event.
    ///
    /// Items whose state and slot did not change keep their original `updated_seq`
    /// and `revision`; only genuinely new or changed items get a fresh revision. That
    /// is what makes a runtime replay idempotent instead of inflating every revision.
    Rebuilt {
        /// The new committed state.
        snapshot: TimelineSnapshot,
        /// The `timeline.snapshot` event carrying the full item list.
        event: EventEnvelope,
    },
}

impl ReplaceOutcome {
    /// The committed state in both variants.
    pub fn snapshot(&self) -> &TimelineSnapshot {
        match self {
            Self::NoChange { snapshot } | Self::Rebuilt { snapshot, .. } => snapshot,
        }
    }

    /// The event this replacement produced, if it produced one.
    pub fn event(&self) -> Option<&EventEnvelope> {
        match self {
            Self::NoChange { .. } => None,
            Self::Rebuilt { event, .. } => Some(event),
        }
    }

    /// [ChangeKind::RefetchRequired] for a rebuild, `None` when nothing changed.
    ///
    /// A rebuild is never [ChangeKind::Incremental]: the event carries a whole list,
    /// so a client must replace the projection rather than patch one row.
    pub const fn change_kind(&self) -> Option<ChangeKind> {
        match self {
            Self::NoChange { .. } => None,
            Self::Rebuilt { .. } => Some(ChangeKind::RefetchRequired),
        }
    }
}

/// The answer to "what did this client miss?".
///
/// There is deliberately no third variant that returns "some events, maybe". A suffix
/// of the log is either applicable to the client's projection or the projection has
/// to be rebuilt, and this type forces the caller to say which.
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    /// A hole-free suffix of the log, ordered by sequence, with no duplicates.
    Events {
        /// Every event with `sequence > cursor`, in commit order.
        events: Vec<EventEnvelope>,
        /// The cursor to persist after applying them (the server's current head).
        next: Cursor,
    },
    /// The suffix cannot be applied; the client must re-pull a snapshot.
    RefetchRequired {
        /// Why an incremental catch-up is impossible.
        reason: RefetchReason,
        /// The server's current head, which the client should prefer over its own.
        next: Cursor,
    },
}

impl Recovery {
    /// The cursor the client should persist either way.
    pub fn next_cursor(&self) -> Cursor {
        match self {
            Self::Events { next, .. } | Self::RefetchRequired { next, .. } => *next,
        }
    }

    /// Whether the client has to rebuild instead of patching.
    pub const fn is_refetch_required(&self) -> bool {
        matches!(self, Self::RefetchRequired { .. })
    }
}

/// A complete, self-consistent view of one session's timeline at one sequence.
///
/// `items` is sorted by `(order_seq, id)`; the id tiebreak exists because two items
/// may legitimately share an `order_seq` (a runtime replaying its own history), and an
/// unstable order would make two identical snapshots compare unequal.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineSnapshot {
    /// Session the snapshot belongs to.
    pub session_id: String,
    /// The committed head the items were read at.
    pub cursor: Cursor,
    /// The item list, sorted by `(order_seq, id)`.
    pub items: Vec<TimelineItem>,
}

impl TimelineSnapshot {
    /// Find one item by id.
    pub fn item(&self, id: &str) -> Option<&TimelineItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Whether the snapshot holds no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Decide whether [ItemInput] can be delivered as a per-item delta.
///
/// A create is always incremental: there is nothing on the client to contradict.
/// An update is incremental as long as the item keeps its kind — status, role,
/// payload, and even display position are carried whole by the update event, so the
/// client can replace the row. A kind change is not: the client's handling for that
/// id (which component renders it, whether it can be streamed) depends on the kind,
/// and a row that morphs from `message` to `tool` while keeping its id and revision
/// chain is exactly the case where "send the new row" produces a UI that no snapshot
/// would ever have produced. Refetch instead.
pub fn classify_change(existing: Option<&TimelineItem>, incoming: &ItemInput) -> ChangeKind {
    match existing {
        None => ChangeKind::Incremental,
        Some(existing) if existing.kind == incoming.kind => ChangeKind::Incremental,
        Some(_) => ChangeKind::RefetchRequired,
    }
}

/// One session timeline: the item projection plus the event log that produced it.
///
/// Invariants the type maintains:
///
/// * `events` holds exactly the sequences `1..=sequence`, each once, in order.
///   One commit consumes exactly one sequence, so a client can verify completeness by
///   counting — that is what `revisions_are_complete` does upstream.
/// * `items` is sorted by `(order_seq, id)` at all times.
/// * `refetch_floor` never decreases; it is the "history below here is not
///   trustworthy" watermark raised by [Timeline::reset] and [Timeline::prune_events_below].
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    session_id: String,
    sequence: i64,
    next_order_seq: i64,
    refetch_floor: i64,
    items: Vec<TimelineItem>,
    events: Vec<EventEnvelope>,
}

impl Timeline {
    /// A timeline for a session with no items and no events.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            sequence: 0,
            next_order_seq: 1,
            refetch_floor: 0,
            items: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Session this timeline belongs to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The head sequence, which is also the number of commits ever made.
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// The head as a cursor.
    pub fn cursor(&self) -> Cursor {
        Cursor::from_validated(self.sequence)
    }

    /// Lowest sequence whose successor events are still applicable.
    pub const fn refetch_floor(&self) -> i64 {
        self.refetch_floor
    }

    /// The committed items, sorted by `(order_seq, id)`.
    pub fn items(&self) -> &[TimelineItem] {
        &self.items
    }

    /// The committed event log, dense from sequence 1.
    pub fn events(&self) -> &[EventEnvelope] {
        &self.events
    }

    /// Find one committed item by id.
    pub fn item(&self, id: &str) -> Option<&TimelineItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// A whole, self-consistent snapshot at the current head.
    pub fn snapshot(&self) -> TimelineSnapshot {
        TimelineSnapshot {
            session_id: self.session_id.clone(),
            cursor: self.cursor(),
            items: self.items.clone(),
        }
    }

    /// The events a client holding `cursor` is missing.
    ///
    /// Dense and duplicate-free by construction: the log is exactly `1..=sequence`,
    /// so the suffix is `cursor+1..=sequence` with no gaps to paper over.
    ///
    /// # Errors
    ///
    /// Only [TimelineError::SequenceOutOfRange], which cannot happen for a live
    /// timeline; the signature stays fallible so a backend built on this type does not
    /// have to change shape.
    pub fn events_after(&self, cursor: Cursor) -> Result<Recovery, TimelineError> {
        let requested = cursor.sequence();
        let next = Cursor::new(self.sequence)?;
        if requested > self.sequence {
            return Ok(Recovery::RefetchRequired {
                reason: RefetchReason::CursorAhead {
                    requested,
                    current: self.sequence,
                },
                next,
            });
        }
        if requested < self.refetch_floor {
            return Ok(Recovery::RefetchRequired {
                reason: RefetchReason::TimelineReset {
                    floor: self.refetch_floor,
                },
                next,
            });
        }
        Ok(Recovery::Events {
            events: self
                .events
                .iter()
                .filter(|event| event.sequence > requested)
                .cloned()
                .collect(),
            next,
        })
    }

    /// Insert a new item, refusing to overwrite an existing one.
    ///
    /// # Errors
    ///
    /// [TimelineError::DuplicateItem] if the id exists, and
    /// [TimelineError::InvalidOrderSeq] for an explicit order below 1.
    pub fn append(&mut self, input: ItemInput, now: &str) -> Result<ApplyOutcome, TimelineError> {
        if self.item(&input.id).is_some() {
            return Err(TimelineError::DuplicateItem {
                session_id: self.session_id.clone(),
                id: input.id,
            });
        }
        self.insert_new(input, now)
    }

    /// Report an item state: create it if absent, update it if present.
    ///
    /// This is the path a runtime normally drives. Re-reporting identical state
    /// returns [ApplyOutcome::NoChange] without consuming a sequence.
    ///
    /// # Errors
    ///
    /// [TimelineError::InvalidOrderSeq] for an explicit order below 1.
    pub fn upsert(&mut self, input: ItemInput, now: &str) -> Result<ApplyOutcome, TimelineError> {
        if self.item(&input.id).is_none() {
            return self.insert_new(input, now);
        }
        self.update_existing(input, now)
    }

    /// Report a new state for an item that must already exist.
    ///
    /// # Errors
    ///
    /// [TimelineError::UnknownItem] if the id is absent, and
    /// [TimelineError::InvalidOrderSeq] for an explicit order below 1.
    pub fn apply_update(
        &mut self,
        input: ItemInput,
        now: &str,
    ) -> Result<ApplyOutcome, TimelineError> {
        if self.item(&input.id).is_none() {
            return Err(TimelineError::UnknownItem {
                session_id: self.session_id.clone(),
                id: input.id,
            });
        }
        self.update_existing(input, now)
    }

    /// Replace the whole projection in one commit.
    ///
    /// Used when a runtime replays its history or when items disappear: neither is
    /// expressible as a per-item delta, so the result is one `timeline.snapshot`
    /// event and the refetch floor rises to that sequence. After that, a client whose
    /// cursor predates the rebuild is told to reload instead of being handed
    /// pre-rebuild events that describe a projection that no longer exists.
    ///
    /// Repeated ids keep the last value, matching upstream
    /// `latest_timeline_items_by_id`. A fully identical replay returns
    /// [ReplaceOutcome::NoChange] and commits nothing.
    ///
    /// # Errors
    ///
    /// [TimelineError::InvalidOrderSeq] for an explicit order below 1.
    pub fn replace_all(
        &mut self,
        inputs: Vec<ItemInput>,
        now: &str,
    ) -> Result<ReplaceOutcome, TimelineError> {
        let mut deduped: Vec<ItemInput> = Vec::with_capacity(inputs.len());
        for input in inputs {
            match deduped.iter().position(|kept| kept.id == input.id) {
                Some(index) => deduped[index] = input,
                None => deduped.push(input),
            }
        }

        let mut allocate = self.next_order_seq;
        let mut resolved: Vec<(ItemInput, i64)> = Vec::with_capacity(deduped.len());
        for input in deduped {
            let order_seq = match input.order_seq {
                Some(order_seq) => {
                    if order_seq < 1 {
                        return Err(TimelineError::InvalidOrderSeq {
                            id: input.id,
                            order_seq,
                        });
                    }
                    order_seq
                }
                None => match self.item(&input.id) {
                    Some(existing) => existing.order_seq,
                    None => {
                        let order_seq = allocate;
                        allocate += 1;
                        order_seq
                    }
                },
            };
            resolved.push((input, order_seq));
        }

        let identical = resolved.len() == self.items.len()
            && resolved.iter().all(|(input, order_seq)| {
                self.item(&input.id).is_some_and(|existing| {
                    existing.content_hash == input.state_hash() && existing.order_seq == *order_seq
                })
            });
        if identical {
            return Ok(ReplaceOutcome::NoChange {
                snapshot: self.snapshot(),
            });
        }

        let sequence = self.next_sequence()?;
        let mut items: Vec<TimelineItem> = Vec::with_capacity(resolved.len());
        for (input, order_seq) in resolved {
            let hash = input.state_hash();
            let existing = self.item(&input.id).cloned();
            let item = match existing {
                Some(existing)
                    if existing.content_hash == hash && existing.order_seq == order_seq =>
                {
                    existing
                }
                Some(existing) => TimelineItem {
                    kind: input.kind,
                    status: input.status,
                    role: input.role,
                    order_seq,
                    updated_seq: sequence,
                    revision: existing.revision + 1,
                    content_hash: hash,
                    payload: input.payload,
                    updated_at: input.updated_at.unwrap_or_else(|| now.to_owned()),
                    ..existing
                },
                None => TimelineItem {
                    id: input.id,
                    session_id: self.session_id.clone(),
                    kind: input.kind,
                    status: input.status,
                    role: input.role,
                    order_seq,
                    updated_seq: sequence,
                    revision: 1,
                    content_hash: hash,
                    payload: input.payload,
                    created_at: now.to_owned(),
                    updated_at: input.updated_at.unwrap_or_else(|| now.to_owned()),
                },
            };
            items.push(item);
        }
        items.sort_by(compare_items);
        self.next_order_seq = self.next_order_seq.max(
            items
                .iter()
                .map(|item| item.order_seq + 1)
                .max()
                .unwrap_or(1),
        );
        self.items = items;
        self.refetch_floor = sequence;
        let payload = json!({ "items": &self.items });
        let event = self.push_event(sequence, EventType::TimelineSnapshot, payload, now)?;
        Ok(ReplaceOutcome::Rebuilt {
            snapshot: self.snapshot(),
            event,
        })
    }

    /// Drop every item and demand a rebuild from clients.
    ///
    /// For the case where the projection cannot even be described: the runtime
    /// reconnected and its history is gone. Emitting a `timeline.snapshot` with an
    /// empty list would be a lie about a lossy event, so this signals a refetch.
    /// `next_order_seq` deliberately keeps counting: reusing slots could make an old
    /// and a new item indistinguishable to a client that only compares `order_seq`.
    ///
    /// # Errors
    ///
    /// Only [TimelineError::SequenceOutOfRange], at the protocol ceiling.
    pub fn reset(&mut self, now: &str) -> Result<EventEnvelope, TimelineError> {
        let sequence = self.next_sequence()?;
        self.items.clear();
        self.refetch_floor = sequence;
        let reason = RefetchReason::TimelineReset { floor: sequence };
        self.push_refetch(sequence, &reason, now)
    }

    /// Ask clients to rebuild without changing the projection.
    ///
    /// # Errors
    ///
    /// Only [TimelineError::SequenceOutOfRange], at the protocol ceiling.
    pub fn request_refetch(
        &mut self,
        reason: RefetchReason,
        now: &str,
    ) -> Result<EventEnvelope, TimelineError> {
        let sequence = self.next_sequence()?;
        self.push_refetch(sequence, &reason, now)
    }

    /// Forget the events below `floor`, modelling log compaction.
    ///
    /// Any backend that prunes its log must raise the same watermark, otherwise a
    /// client with an old cursor receives a suffix that looks complete and is not.
    ///
    /// # Errors
    ///
    /// [TimelineError::InvalidFloor] when `floor` is outside `0..=sequence`.
    pub fn prune_events_below(&mut self, floor: i64) -> Result<(), TimelineError> {
        if floor < 0 || floor > self.sequence {
            return Err(TimelineError::InvalidFloor {
                floor,
                sequence: self.sequence,
            });
        }
        self.events.retain(|event| event.sequence >= floor);
        self.refetch_floor = self.refetch_floor.max(floor);
        Ok(())
    }

    fn insert_new(&mut self, input: ItemInput, now: &str) -> Result<ApplyOutcome, TimelineError> {
        let order_seq = match input.order_seq {
            Some(order_seq) => {
                if order_seq < 1 {
                    return Err(TimelineError::InvalidOrderSeq {
                        id: input.id,
                        order_seq,
                    });
                }
                order_seq
            }
            None => self.next_order_seq,
        };
        let sequence = self.next_sequence()?;
        let item = TimelineItem {
            content_hash: input.state_hash(),
            id: input.id,
            session_id: self.session_id.clone(),
            kind: input.kind,
            status: input.status,
            role: input.role,
            order_seq,
            updated_seq: sequence,
            revision: 1,
            payload: input.payload,
            created_at: now.to_owned(),
            updated_at: input.updated_at.unwrap_or_else(|| now.to_owned()),
        };
        self.next_order_seq = self.next_order_seq.max(order_seq + 1);
        self.insert_item(item.clone());
        let payload = json!({ "item": &item });
        let event = self.push_event(sequence, EventType::TimelineItemCreated, payload, now)?;
        Ok(ApplyOutcome::Created { item, event })
    }

    fn update_existing(
        &mut self,
        input: ItemInput,
        now: &str,
    ) -> Result<ApplyOutcome, TimelineError> {
        let existing = self
            .item(&input.id)
            .cloned()
            .ok_or_else(|| TimelineError::UnknownItem {
                session_id: self.session_id.clone(),
                id: input.id.clone(),
            })?;
        let order_seq = match input.order_seq {
            Some(order_seq) => {
                if order_seq < 1 {
                    return Err(TimelineError::InvalidOrderSeq {
                        id: input.id,
                        order_seq,
                    });
                }
                order_seq
            }
            None => existing.order_seq,
        };
        let hash = input.state_hash();
        if hash == existing.content_hash && order_seq == existing.order_seq {
            return Ok(ApplyOutcome::NoChange { item: existing });
        }

        let change = classify_change(Some(&existing), &input);
        let sequence = self.next_sequence()?;
        let mut item = existing.clone();
        item.kind = input.kind;
        item.status = input.status;
        item.role = input.role;
        item.order_seq = order_seq;
        item.updated_seq = sequence;
        item.revision = existing.revision + 1;
        item.content_hash = hash;
        item.payload = input.payload;
        item.updated_at = input.updated_at.unwrap_or_else(|| now.to_owned());
        self.next_order_seq = self.next_order_seq.max(order_seq + 1);
        self.replace_item(item.clone());

        let reason = match change {
            ChangeKind::Incremental => None,
            ChangeKind::RefetchRequired => Some(RefetchReason::IncompatibleItemChange {
                item_id: item.id.clone(),
                previous: existing.kind,
                next: item.kind,
            }),
        };
        let event = match &reason {
            None => {
                let payload = json!({ "item": &item });
                self.push_event(sequence, EventType::TimelineItemUpdated, payload, now)?
            }
            Some(reason) => self.push_refetch(sequence, reason, now)?,
        };
        Ok(match reason {
            None => ApplyOutcome::Updated { item, event },
            Some(reason) => ApplyOutcome::RefetchRequired {
                item,
                reason,
                event,
            },
        })
    }

    fn next_sequence(&self) -> Result<i64, TimelineError> {
        let sequence = self.sequence + 1;
        if sequence > PROTOCOL_MAX_REVISION {
            return Err(TimelineError::SequenceOutOfRange { sequence });
        }
        Ok(sequence)
    }

    fn insert_item(&mut self, item: TimelineItem) {
        let position = self
            .items
            .partition_point(|existing| compare_items(existing, &item).is_lt());
        self.items.insert(position, item);
    }

    fn replace_item(&mut self, item: TimelineItem) {
        let position = self
            .items
            .iter()
            .position(|existing| existing.id == item.id);
        match position {
            Some(index) => self.items[index] = item,
            None => self.insert_item(item),
        }
        self.items.sort_by(compare_items);
    }

    fn push_event(
        &mut self,
        sequence: i64,
        kind: EventType,
        payload: Value,
        emitted_at: &str,
    ) -> Result<EventEnvelope, TimelineError> {
        let event = EventEnvelope::new(
            &self.session_id,
            sequence,
            kind,
            payload,
            emitted_at.to_owned(),
        )?;
        self.sequence = sequence;
        self.events.push(event.clone());
        Ok(event)
    }

    fn push_refetch(
        &mut self,
        sequence: i64,
        reason: &RefetchReason,
        emitted_at: &str,
    ) -> Result<EventEnvelope, TimelineError> {
        let mut payload = reason.to_payload();
        if let Value::Object(map) = &mut payload {
            map.insert(
                "eventCursor".to_owned(),
                Value::String(Cursor::from_validated(sequence).to_wire()),
            );
        }
        self.push_event(
            sequence,
            EventType::SessionRefetchRequired,
            payload,
            emitted_at,
        )
    }
}

/// Display order: by `order_seq`, ties broken by id for determinism.
fn compare_items(left: &TimelineItem, right: &TimelineItem) -> std::cmp::Ordering {
    (left.order_seq, left.id.as_str()).cmp(&(right.order_seq, right.id.as_str()))
}
