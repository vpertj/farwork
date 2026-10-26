//! Timeline glue between `farwork-timeline` (domain semantics) and Postgres
//! (durability) plus the live fan-out.
//!
//! Division of labour, after the domain crate landed:
//!
//! * `farwork-timeline` owns *semantics*: sequence allocation, dedupe by content
//!   hash, order_seq assignment, cursor recovery and the refuse-instead-of-patch
//!   rule. Nothing in this file re-decides those.
//! * this module owns *persistence and transport*: hydrating an engine from
//!   Postgres, writing committed items back, the wire adapters (upstream adds
//!   `source`, `completedAt` and `protocolVersion`, the crate does not), and the
//!   per-session engine cache.
//!
//! Durability model: the committed projection and the head sequence live in
//! Postgres (`timeline_items` + `sessions.seq`); the engine is hydrated from
//! those rows on first touch in a process. Replay is exact because every commit
//! in this build is an item commit, so stored `updated_seq` values are dense and
//! replaying in that order reproduces the same sequence numbers. A stored item
//! with `revision > 1` cannot be reproduced (replay re-creates it at revision 1),
//! so hydration raises a refetch signal instead of pretending.

use std::collections::HashMap;
use std::sync::Arc;

use farwork_timeline::{
    ApplyOutcome, Cursor, EventEnvelope, EventType, ItemInput, ItemStatus, Recovery, RefetchReason,
    Role, Timeline, TimelineItem, TimelineItemKind,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::SessionRow;
use crate::timeutil;

/// Upstream `ProtocolEventEnvelope.protocolVersion`.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Result of a hub-side write. `Unchanged` is a success, not a failure: the
/// engine deduped the report or the client replayed its own idempotency key.
pub enum WriteOutcome {
    Committed { item: TimelineItem, event: Value },
    Unchanged { item: TimelineItem },
}

struct SessionEngine {
    timeline: Timeline,
    /// Per-item provenance, which upstream carries in `source` and the domain
    /// crate deliberately does not model.
    sources: HashMap<String, Value>,
    hydrated: bool,
}

impl SessionEngine {
    fn new(session_id: &str) -> Self {
        Self {
            timeline: Timeline::new(session_id),
            sources: HashMap::new(),
            hydrated: false,
        }
    }
}

#[derive(Default)]
pub struct TimelineHub {
    engines: Mutex<HashMap<String, Arc<Mutex<SessionEngine>>>>,
}

impl TimelineHub {
    pub fn new() -> Self {
        Self::default()
    }

    async fn engine(&self, session_id: &str) -> Arc<Mutex<SessionEngine>> {
        let mut engines = self.engines.lock().await;
        engines
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(SessionEngine::new(session_id))))
            .clone()
    }

    async fn locked(
        &self,
        pool: &PgPool,
        session_id: &str,
    ) -> ApiResult<tokio::sync::OwnedMutexGuard<SessionEngine>> {
        // Take an owned guard so hydration can await without borrowing the map.
        let engine = self.engine(session_id).await;
        let mut guard = engine.lock_owned().await;
        hydrate_if_needed(&mut guard, pool, session_id).await?;
        Ok(guard)
    }

    /// Head cursor for the session.
    pub async fn head(&self, pool: &PgPool, session_id: &str) -> ApiResult<Cursor> {
        let guard = self.locked(pool, session_id).await?;
        Ok(guard.timeline.cursor())
    }

    /// Whole projection as wire items.
    pub async fn snapshot(
        &self,
        pool: &PgPool,
        session_id: &str,
    ) -> ApiResult<(Cursor, Vec<Value>)> {
        let guard = self.locked(pool, session_id).await?;
        Ok((
            guard.timeline.cursor(),
            wire_items(&guard, guard.timeline.items()),
        ))
    }

    /// `mode=latest`: the newest `limit` items, ascending.
    pub async fn items_latest(
        &self,
        pool: &PgPool,
        session_id: &str,
        limit: i64,
    ) -> ApiResult<(Cursor, Vec<Value>, bool)> {
        let guard = self.locked(pool, session_id).await?;
        let items = guard.timeline.items();
        let limit = limit.max(0) as usize;
        let has_more = items.len() > limit;
        let start = items.len().saturating_sub(limit);
        Ok((
            guard.timeline.cursor(),
            wire_items(&guard, &items[start..]),
            has_more,
        ))
    }

    /// `mode=changes`: everything committed after `after_seq`.
    pub async fn items_since(
        &self,
        pool: &PgPool,
        session_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> ApiResult<(Cursor, Vec<Value>, bool)> {
        let guard = self.locked(pool, session_id).await?;
        let mut selected: Vec<TimelineItem> = guard
            .timeline
            .items()
            .iter()
            .filter(|item| item.updated_seq > after_seq)
            .cloned()
            .collect();
        selected.sort_by(|left, right| {
            (left.updated_seq, left.id.as_str()).cmp(&(right.updated_seq, right.id.as_str()))
        });
        let limit = limit.max(0) as usize;
        let has_more = selected.len() > limit;
        selected.truncate(limit);
        Ok((
            guard.timeline.cursor(),
            wire_items(&guard, &selected),
            has_more,
        ))
    }

    /// `mode=history`: items before `before_order_seq`, oldest of the page first.
    pub async fn items_before_order_seq(
        &self,
        pool: &PgPool,
        session_id: &str,
        before_order_seq: i64,
        limit: i64,
    ) -> ApiResult<(Cursor, Vec<Value>, bool)> {
        let guard = self.locked(pool, session_id).await?;
        let mut selected: Vec<TimelineItem> = guard
            .timeline
            .items()
            .iter()
            .filter(|item| item.order_seq < before_order_seq)
            .cloned()
            .collect();
        selected.sort_by(|left, right| {
            (left.order_seq, left.id.as_str()).cmp(&(right.order_seq, right.id.as_str()))
        });
        let limit = limit.max(0) as usize;
        let has_more = selected.len() > limit;
        let start = selected.len().saturating_sub(limit);
        let page = selected[start..].to_vec();
        Ok((guard.timeline.cursor(), wire_items(&guard, &page), has_more))
    }

    /// Cursor catch-up. The domain crate decides whether the suffix is
    /// deliverable; a cursor we cannot serve becomes an explicit refetch, never
    /// an empty "you are current" list.
    pub async fn recover(
        &self,
        pool: &PgPool,
        session_id: &str,
        cursor: Cursor,
    ) -> ApiResult<Recovery> {
        let guard = self.locked(pool, session_id).await?;
        guard
            .timeline
            .events_after(cursor)
            .map_err(|error| ApiError::internal(error.to_string()))
    }

    /// Persist a user message as a timeline item.
    pub async fn write_message(
        &self,
        pool: &PgPool,
        session: &SessionRow,
        text: &str,
        client_message_id: Option<&str>,
    ) -> ApiResult<WriteOutcome> {
        let engine = self.engine(&session.id).await;
        let mut guard = engine.lock().await;
        hydrate_if_needed(&mut guard, pool, &session.id).await?;

        // Idempotency keys are a storage concern: the domain engine would treat a
        // second report with different text as a revision of the same item.
        if let Some(client_message_id) = client_message_id {
            if let Some(existing) =
                find_by_client_message_id(pool, &session.id, client_message_id).await?
            {
                return Ok(WriteOutcome::Unchanged { item: existing });
            }
        }

        let now = timeutil::utc_now();
        let item_id = ids::timeline_item_id();
        let payload = json!({ "text": text, "format": "markdown" });
        let input = ItemInput::new(item_id, TimelineItemKind::Message, payload)
            .with_status(ItemStatus::Done)
            .with_role(Role::User);
        let outcome = guard
            .timeline
            .upsert(input, &now)
            .map_err(|error| ApiError::internal(error.to_string()))?;

        match outcome {
            ApplyOutcome::NoChange { item } => Ok(WriteOutcome::Unchanged { item }),
            ApplyOutcome::Created { item, event }
            | ApplyOutcome::Updated { item, event }
            | ApplyOutcome::RefetchRequired { item, event, .. } => {
                let source =
                    item_source(&session.runtime, &session.id, &item.id, client_message_id);
                guard.sources.insert(item.id.clone(), source.clone());
                let head = guard.timeline.sequence();
                persist_item(pool, session, &item, &source, client_message_id, head, &now).await?;
                Ok(WriteOutcome::Committed {
                    item,
                    event: wire_event(&event),
                })
            }
        }
    }

    /// Apply an item reported by a connector (`item.upsert`).
    ///
    /// The engine stays the only allocator of `order_seq`/`updated_seq`; the hub
    /// persists the committed item and hands back the wire event, or `None` when
    /// the runtime re-reported identical state (dedupe) and nothing was committed.
    pub async fn apply_runtime_item(
        &self,
        pool: &PgPool,
        session: &SessionRow,
        input: ItemInput,
        source: Value,
    ) -> ApiResult<(TimelineItem, Option<Value>)> {
        let engine = self.engine(&session.id).await;
        let mut guard = engine.lock().await;
        hydrate_if_needed(&mut guard, pool, &session.id).await?;

        let now = timeutil::utc_now();
        let outcome = guard
            .timeline
            .upsert(input, &now)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        match outcome {
            ApplyOutcome::NoChange { item } => Ok((item, None)),
            ApplyOutcome::Created { item, event }
            | ApplyOutcome::Updated { item, event }
            | ApplyOutcome::RefetchRequired { item, event, .. } => {
                guard.sources.insert(item.id.clone(), source.clone());
                let head = guard.timeline.sequence();
                persist_item(pool, session, &item, &source, None, head, &now).await?;
                Ok((item, Some(wire_event(&event))))
            }
        }
    }

    /// Live `session.meta.updated` projection event at the current head.
    ///
    /// A meta edit does not commit timeline content, so it consumes no sequence:
    /// clients render it from the payload and re-read a snapshot on reconnect.
    pub async fn meta_event(
        &self,
        pool: &PgPool,
        session_id: &str,
        view: &Value,
    ) -> ApiResult<Value> {
        let head = self.head(pool, session_id).await?;
        EventEnvelope::new(
            session_id,
            head.sequence(),
            EventType::SessionMetaUpdated,
            json!({ "session": view }),
            timeutil::utc_now(),
        )
        .map(|event| wire_event(&event))
        .map_err(|error| ApiError::internal(error.to_string()))
    }

    /// First frame on a session socket.
    pub async fn subscribed_event(
        &self,
        pool: &PgPool,
        session_id: &str,
        client_id: &str,
    ) -> ApiResult<Value> {
        let head = self.head(pool, session_id).await?;
        EventEnvelope::new(
            session_id,
            head.sequence(),
            EventType::SessionSubscribed,
            json!({ "clientId": client_id, "eventCursor": head.to_wire() }),
            timeutil::utc_now(),
        )
        .map(|event| wire_event(&event))
        .map_err(|error| ApiError::internal(error.to_string()))
    }

    /// Refetch signal, used when a live subscriber lagged behind the fan-out.
    pub async fn refetch_event(
        &self,
        pool: &PgPool,
        session_id: &str,
        reason: RefetchReason,
    ) -> ApiResult<Value> {
        let head = self.head(pool, session_id).await?;
        let mut payload = reason.to_payload();
        if let Some(map) = payload.as_object_mut() {
            map.insert(String::from("eventCursor"), Value::String(head.to_wire()));
        }
        EventEnvelope::new(
            session_id,
            head.sequence(),
            EventType::SessionRefetchRequired,
            payload,
            timeutil::utc_now(),
        )
        .map(|event| wire_event(&event))
        .map_err(|error| ApiError::internal(error.to_string()))
    }
}

// ------------------------------------------------------------ wire adapters

/// Upstream item shape: the crate's projection plus `source`, `completedAt` and
/// the `sha256:` algorithm prefix upstream puts on `contentHash`.
pub fn wire_item(item: &TimelineItem, source: &Value) -> Value {
    let mut value = serde_json::to_value(item).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert(String::from("source"), source.clone());
        map.insert(String::from("completedAt"), Value::Null);
        map.insert(
            String::from("contentHash"),
            Value::String(format!("sha256:{}", item.content_hash)),
        );
    }
    value
}

fn wire_items(engine: &SessionEngine, items: &[TimelineItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            let source = engine
                .sources
                .get(&item.id)
                .cloned()
                .unwrap_or_else(|| item_source("platform", &item.session_id, &item.id, None));
            wire_item(item, &source)
        })
        .collect()
}

/// Upstream envelope shape: the crate envelope plus `protocolVersion`.
pub fn wire_event(event: &EventEnvelope) -> Value {
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert(
            String::from("protocolVersion"),
            Value::String(PROTOCOL_VERSION.to_string()),
        );
        map.insert(
            String::from("cursor"),
            Value::String(event.cursor.to_wire()),
        );
    }
    value
}

pub fn item_source(
    runtime: &str,
    session_id: &str,
    item_id: &str,
    client_message_id: Option<&str>,
) -> Value {
    let mut source = json!({
        "runtime": runtime,
        "sessionId": session_id,
        "itemId": item_id,
    });
    if let Some(client_message_id) = client_message_id {
        if let Some(map) = source.as_object_mut() {
            map.insert(
                String::from("clientMessageId"),
                Value::String(client_message_id.to_string()),
            );
        }
    }
    source
}

// ------------------------------------------------------ persistence layer

/// Stored shape: the crate item plus the provenance this build adds.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredItem {
    item: TimelineItem,
    #[serde(default)]
    source: Value,
}

async fn hydrate_if_needed(
    engine: &mut SessionEngine,
    pool: &PgPool,
    session_id: &str,
) -> ApiResult<()> {
    if engine.hydrated {
        return Ok(());
    }
    use sqlx::Row as _;
    let rows = sqlx::query(
        "select payload_json from timeline_items where session_id = $1 order by updated_seq, id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    let mut needs_refetch = false;
    for row in &rows {
        let raw: String = row.get("payload_json");
        let stored: StoredItem = serde_json::from_str(&raw).map_err(|error| {
            ApiError::internal(format!(
                "corrupt timeline row for session {session_id}: {error}"
            ))
        })?;
        let stored_item = &stored.item;
        if stored_item.revision > 1 {
            // Replay reproduces only the current state, at revision 1.
            needs_refetch = true;
        }
        let mut input = ItemInput::new(
            stored_item.id.clone(),
            stored_item.kind,
            stored_item.payload.clone(),
        )
        .with_status(stored_item.status)
        .with_order_seq(stored_item.order_seq)
        .with_updated_at(stored_item.updated_at.clone());
        if let Some(role) = stored_item.role {
            input = input.with_role(role);
        }
        engine
            .timeline
            .upsert(input, &stored_item.updated_at)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        engine
            .sources
            .insert(stored_item.id.clone(), stored.source.clone());
    }

    engine.hydrated = true;
    if needs_refetch {
        // Refuse to let a client patch across a replay it cannot trust.
        engine
            .timeline
            .request_refetch(RefetchReason::Requested, &timeutil::utc_now())
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn find_by_client_message_id(
    pool: &PgPool,
    session_id: &str,
    client_message_id: &str,
) -> ApiResult<Option<TimelineItem>> {
    use sqlx::Row as _;
    let row = sqlx::query(
        "select payload_json from timeline_items
          where session_id = $1 and client_message_id = $2",
    )
    .bind(session_id)
    .bind(client_message_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let raw: String = row.get("payload_json");
    let stored: StoredItem = serde_json::from_str(&raw)
        .map_err(|error| ApiError::internal(format!("corrupt timeline row: {error}")))?;
    Ok(Some(stored.item))
}

async fn persist_item(
    pool: &PgPool,
    session: &SessionRow,
    item: &TimelineItem,
    source: &Value,
    client_message_id: Option<&str>,
    head: i64,
    now: &str,
) -> ApiResult<()> {
    let stored = StoredItem {
        item: item.clone(),
        source: source.clone(),
    };
    let payload_json = serde_json::to_string(&stored)
        .map_err(|error| ApiError::internal(format!("cannot encode timeline item: {error}")))?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "insert into timeline_items
            (session_id, id, type, status, role, order_seq, updated_seq, revision,
             content_hash, client_message_id, item_time, payload_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         on conflict (session_id, id) do update
            set type = excluded.type,
                status = excluded.status,
                role = excluded.role,
                order_seq = excluded.order_seq,
                updated_seq = excluded.updated_seq,
                revision = excluded.revision,
                content_hash = excluded.content_hash,
                item_time = excluded.item_time,
                payload_json = excluded.payload_json",
    )
    .bind(&item.session_id)
    .bind(&item.id)
    .bind(item.kind.as_str())
    .bind(item.status.as_str())
    .bind(item.role.map(Role::as_str))
    .bind(item.order_seq as i32)
    .bind(item.updated_seq)
    .bind(item.revision as i32)
    .bind(&item.content_hash)
    .bind(client_message_id)
    .bind(&item.updated_at)
    .bind(&payload_json)
    .execute(&mut *tx)
    .await?;

    // sessions.seq mirrors the engine head so list views and the dashboard see
    // progress without hydrating the engine.
    sqlx::query(
        "update sessions
            set seq = $2,
                updated_seq = $2,
                seq_allocated_high = $2,
                last_activity_at = $3,
                sort_at = $3
          where id = $1",
    )
    .bind(&session.id)
    .bind(head)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "update session_runtime_states set updated_seq = $2, updated_at = $3 where session_id = $1",
    )
    .bind(&session.id)
    .bind(head)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> TimelineItem {
        TimelineItem {
            id: String::from("itm_1"),
            session_id: String::from("sess_1"),
            kind: TimelineItemKind::Message,
            status: ItemStatus::Done,
            role: Some(Role::User),
            order_seq: 3,
            updated_seq: 4,
            revision: 1,
            content_hash: String::from("deadbeef"),
            payload: json!({"text": "hi", "format": "markdown"}),
            created_at: String::from("2025-01-01T00:00:00.000000Z"),
            updated_at: String::from("2025-01-01T00:00:00.000000Z"),
        }
    }

    #[test]
    fn wire_item_adds_source_completed_at_and_hash_prefix() {
        let value = wire_item(
            &sample_item(),
            &item_source("codex", "sess_1", "itm_1", Some("cm_1")),
        );
        assert_eq!(value["id"], "itm_1");
        assert_eq!(value["type"], "message");
        assert_eq!(value["role"], "user");
        assert_eq!(value["orderSeq"], 3);
        assert_eq!(value["updatedSeq"], 4);
        assert_eq!(value["contentHash"], "sha256:deadbeef");
        assert_eq!(value["content"]["text"], "hi");
        assert_eq!(value["source"]["clientMessageId"], "cm_1");
        assert!(value.get("completedAt").is_some());
    }

    #[test]
    fn wire_event_carries_protocol_version_and_wire_cursor() {
        let event = EventEnvelope::new(
            "sess_1",
            7,
            EventType::TimelineItemCreated,
            json!({"item": {"id": "itm_1"}}),
            "2025-01-01T00:00:00.000000Z",
        )
        .expect("event");
        let value = wire_event(&event);
        assert_eq!(value["protocolVersion"], "1.0");
        assert_eq!(value["cursor"], "seq:7");
        assert_eq!(value["type"], "timeline.item_created");
        assert_eq!(value["sessionId"], "sess_1");
        assert_eq!(value["sequence"], 7);
    }

    #[test]
    fn stored_item_round_trips_through_json() {
        let stored = StoredItem {
            item: sample_item(),
            source: item_source("codex", "sess_1", "itm_1", None),
        };
        let raw = serde_json::to_string(&stored).expect("encode");
        let parsed: StoredItem = serde_json::from_str(&raw).expect("decode");
        assert_eq!(parsed.item.id, "itm_1");
        assert_eq!(parsed.item.order_seq, 3);
        assert_eq!(parsed.source["runtime"], "codex");
    }
}
