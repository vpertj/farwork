//! Row types plus the JSON projections the HTTP/WS contract promises.
//!
//! Field names and enum spellings are taken from upstream
//! `server/agent_server/core/models.py` and `core/protocol.py`.

use serde_json::{json, Value};

use crate::timeutil;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub email: Option<String>,
    pub email_verified_at: Option<String>,
    pub display_name: String,
    pub password_hash: String,
    pub role: String,
    pub disabled: bool,
    pub avatar: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl UserRow {
    /// `AuthResponse` from upstream `core/models.py`.
    pub fn auth_response(&self, access_token: &str) -> Value {
        json!({
            "userId": self.id,
            "email": self.email,
            "emailVerified": self.email_verified_at.is_some(),
            "displayName": self.display_name,
            "role": self.role,
            "accessToken": access_token,
            "tokenType": "bearer",
            "serverTime": timeutil::utc_now(),
        })
    }

    /// `AuthMeResponse`.
    pub fn me_response(&self) -> Value {
        json!({
            "userId": self.id,
            "email": self.email,
            "emailVerified": self.email_verified_at.is_some(),
            "displayName": self.display_name,
            "role": self.role,
            "disabled": self.disabled,
            "avatar": self.avatar,
            "serverTime": timeutil::utc_now(),
        })
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConnectorRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub connector_kind: String,
    pub device_os: Option<String>,
    pub status: String,
    pub last_seen_at: Option<String>,
    pub token_hash: String,
    pub token_prefix: String,
    pub revoked: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl ConnectorRow {
    /// `ConnectorView`.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "userId": self.user_id,
            "name": self.name,
            "connectorKind": self.connector_kind,
            "deviceOs": self.device_os,
            "status": self.status,
            "lastSeenAt": self.last_seen_at,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProjectRow {
    pub id: String,
    pub user_id: String,
    pub connector_id: String,
    pub name: String,
    pub workspace_path: String,
    pub workspace_key: String,
    pub manually_created: bool,
    pub pinned: bool,
    pub pinned_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ProjectRow {
    /// `ProjectView`; session counts are supplied by the caller's query.
    pub fn view(&self, active: i64, archived: i64, last_activity_at: Option<String>) -> Value {
        json!({
            "id": self.id,
            "userId": self.user_id,
            "connectorId": self.connector_id,
            "name": self.name,
            "workspacePath": self.workspace_path,
            "manuallyCreated": self.manually_created,
            "pinned": self.pinned,
            "pinnedAt": self.pinned_at,
            "activeSessionCount": active,
            "sidebarSessionCounts": {
                "active": active,
                "archived": archived,
            },
            "lastActivityAt": last_activity_at,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: String,
    pub user_id: String,
    pub connector_id: String,
    pub project_id: Option<String>,
    pub runtime: String,
    pub runtime_id: String,
    pub origin: String,
    pub external_session_id: Option<String>,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub cwd: Option<String>,
    pub status: String,
    pub takeover: bool,
    pub pinned: bool,
    pub pinned_at: Option<String>,
    pub archived: bool,
    pub archived_at: Option<String>,
    pub source_state: String,
    pub last_read_seq: i64,
    pub latest_turn_end_seq: i64,
    pub last_synced_at: Option<String>,
    pub source_observed_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub sort_at: String,
    pub seq: i64,
    pub seq_allocated_high: i64,
    pub updated_seq: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl SessionRow {
    /// `SessionView` from upstream `core/protocol.py`. Fields the P0 hub does not
    /// track (item-level projections) are reported with their neutral defaults
    /// rather than invented values.
    pub fn view(&self, connector_status: &str) -> Value {
        json!({
            "id": self.id,
            "connectorId": self.connector_id,
            "projectId": self.project_id,
            "connectorStatus": connector_status,
            "runtime": self.runtime,
            "runtimeId": self.runtime_id,
            "externalSessionId": self.external_session_id,
            "title": self.title,
            "cwd": self.cwd,
            "status": self.status,
            "takeover": self.takeover,
            "pinned": self.pinned,
            "pinnedAt": self.pinned_at,
            "archived": self.archived,
            "archivedAt": self.archived_at,
            "userArchived": self.archived,
            "sourceAvailability": if self.archived { "archived" } else { "available" },
            "sourceAvailabilityReason": null,
            "sourceAvailabilityUpdatedAt": null,
            "sourceObservationOrigin": null,
            "archiveSource": if self.archived { Value::String("user".into()) } else { Value::Null },
            "unread": self.last_read_seq < self.seq,
            "lastReadSeq": self.last_read_seq,
            "latestTurnEndSeq": self.latest_turn_end_seq,
            "lastSyncedAt": self.last_synced_at,
            "sourceObservedAt": self.source_observed_at,
            "lastActivityAt": self.last_activity_at,
            "lastItemAt": self.last_activity_at,
            "lastItemOrderSeq": null,
            "sortAt": self.sort_at,
            "updatedSeq": self.updated_seq,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

/// `SessionRuntimeState` (upstream `core/protocol.py`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuntimeStateRow {
    pub session_id: String,
    pub runtime: String,
    pub external_session_id: Option<String>,
    pub status: String,
    pub selections_json: String,
    pub status_reason: Option<String>,
    pub error_json: Option<String>,
    pub metadata_json: String,
    pub updated_seq: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl RuntimeStateRow {
    pub fn view(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "runtime": self.runtime,
            "externalSessionId": self.external_session_id,
            "status": self.status,
            "selections": parse_json_object(&self.selections_json),
            "statusReason": self.status_reason,
            "error": self.error_json.as_deref().and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
            "metadata": parse_json_object(&self.metadata_json),
            "updatedSeq": self.updated_seq,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

fn parse_json_object(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({}))
}
