//! `/connectors/*` — device registration, listing, rename, delete, token
//! rotation.
//!
//! P0 note: this build has no connector ingress (`/ws/connector`), so every
//! connector is reported `offline` from its stored status. That is the truthful
//! value; no synthetic presence is invented.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::sha256_hex;
use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::{ConnectorRow, SessionRow};
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connectors", get(list_connectors).post(create_connector))
        .route(
            "/connectors/{connector_id}",
            get(get_connector)
                .patch(patch_connector)
                .delete(delete_connector),
        )
        .route("/connectors/{connector_id}/revoke", post(revoke_connector))
        .route(
            "/connectors/{connector_id}/preferences",
            get(get_connector_preferences),
        )
        .route(
            "/connectors/{connector_id}/sessions/archive-all",
            post(archive_all_sessions),
        )
}

const CONNECTOR_COLUMNS: &str =
    "id, user_id, name, connector_kind, device_os, status, last_seen_at,
            token_hash, token_prefix, revoked, created_at, updated_at";

#[derive(Debug, Deserialize)]
pub struct ConnectorCreateRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "connectorKind")]
    pub connector_kind: Option<String>,
    #[serde(default, rename = "installationId")]
    pub installation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectorUpdateRequest {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveAllRequest {
    pub archived: bool,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn find_connector(
    pool: &sqlx::PgPool,
    connector_id: &str,
) -> Result<Option<ConnectorRow>, sqlx::Error> {
    sqlx::query_as::<_, ConnectorRow>(&format!(
        "select {CONNECTOR_COLUMNS} from connectors where id = $1"
    ))
    .bind(connector_id)
    .fetch_optional(pool)
    .await
}

async fn owned_connector(
    state: &AppState,
    user_id: &str,
    connector_id: &str,
) -> ApiResult<ConnectorRow> {
    let connector = find_connector(state.pool(), connector_id)
        .await?
        .filter(|row| row.user_id == user_id && !row.revoked)
        .ok_or_else(|| ApiError::not_found("connector not found"))?;
    Ok(connector)
}

pub async fn connector_views(state: &AppState, user_id: &str) -> ApiResult<Vec<Value>> {
    let rows = sqlx::query_as::<_, ConnectorRow>(&format!(
        "select {CONNECTOR_COLUMNS} from connectors where user_id = $1 order by created_at, id"
    ))
    .bind(user_id)
    .fetch_all(state.pool())
    .await?;
    Ok(rows.iter().map(|row| row.view()).collect())
}

async fn list_connectors(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "connectors": connector_views(&state, &user_id).await?,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn create_connector(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<ConnectorCreateRequest>,
) -> ApiResult<Json<Value>> {
    let kind = payload
        .connector_kind
        .unwrap_or_else(|| String::from("cli"));
    if kind != "cli" && kind != "desktop" {
        return Err(ApiError::unprocessable("invalid connectorKind"));
    }
    let name = payload
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| String::from("Codex Connector"));

    // An installation id makes registration idempotent per user, as upstream
    // does; a retried registration reissues credentials for the same device.
    let connector_id = match payload.installation_id.as_deref() {
        Some(installation_id) if !installation_id.is_empty() => {
            let digest = sha256_hex(format!("{user_id}:{installation_id}").as_bytes());
            format!("conn_{}", &digest[..24])
        }
        _ => ids::connector_id(),
    };

    if let Some(existing) = find_connector(state.pool(), &connector_id).await? {
        if existing.user_id != user_id {
            return Err(ApiError::conflict("connector id already in use"));
        }
        let (token, prefix) = rotate_token(state.pool(), &connector_id).await?;
        let connector = find_connector(state.pool(), &connector_id)
            .await?
            .ok_or_else(|| ApiError::not_found("connector not found"))?;
        return Ok(Json(json!({
            "connector": connector.view(),
            "connectorToken": token,
            "tokenPrefix": prefix,
        })));
    }

    let token = ids::connector_token();
    let prefix: String = token.chars().take(12).collect();
    let now = timeutil::utc_now();
    let connector = sqlx::query_as::<_, ConnectorRow>(&format!(
        "insert into connectors
            (id, user_id, name, connector_kind, status, token_hash, token_prefix, revoked,
             created_at, updated_at)
         values ($1, $2, $3, $4, 'offline', $5, $6, false, $7, $7)
         returning {CONNECTOR_COLUMNS}"
    ))
    .bind(&connector_id)
    .bind(&user_id)
    .bind(&name)
    .bind(&kind)
    .bind(sha256_hex(token.as_bytes()))
    .bind(&prefix)
    .bind(&now)
    .fetch_one(state.pool())
    .await?;

    Ok(Json(json!({
        "connector": connector.view(),
        "connectorToken": token,
        "tokenPrefix": prefix,
    })))
}

async fn rotate_token(pool: &sqlx::PgPool, connector_id: &str) -> ApiResult<(String, String)> {
    let token = ids::connector_token();
    let prefix: String = token.chars().take(12).collect();
    let now = timeutil::utc_now();
    let row = sqlx::query(
        "update connectors
            set token_hash = $2, token_prefix = $3, updated_at = $4
          where id = $1 and revoked = false",
    )
    .bind(connector_id)
    .bind(sha256_hex(token.as_bytes()))
    .bind(&prefix)
    .bind(&now)
    .execute(pool)
    .await?;
    if row.rows_affected() == 0 {
        return Err(ApiError::not_found("connector not found"));
    }
    Ok((token, prefix))
}

async fn get_connector(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let connector = owned_connector(&state, &user_id, &connector_id).await?;
    Ok(Json(json!({
        "connector": connector.view(),
        "serverTime": timeutil::utc_now(),
    })))
}

async fn patch_connector(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
    Json(payload): Json<ConnectorUpdateRequest>,
) -> ApiResult<Json<Value>> {
    owned_connector(&state, &user_id, &connector_id).await?;
    if let Some(name) = payload.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::unprocessable("name must not be empty"));
        }
        sqlx::query("update connectors set name = $2, updated_at = $3 where id = $1")
            .bind(&connector_id)
            .bind(name)
            .bind(timeutil::utc_now())
            .execute(state.pool())
            .await?;
    }
    let connector = owned_connector(&state, &user_id, &connector_id).await?;
    Ok(Json(json!({
        "connector": connector.view(),
        "serverTime": timeutil::utc_now(),
    })))
}

async fn delete_connector(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
) -> ApiResult<StatusCode> {
    owned_connector(&state, &user_id, &connector_id).await?;
    let mut tx = state.pool().begin().await?;
    // Explicit order: sessions restrict project deletion, so clear children
    // before their parents instead of relying on cascade ordering.
    sqlx::query("delete from sessions where connector_id = $1")
        .bind(&connector_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from projects where connector_id = $1")
        .bind(&connector_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from connectors where id = $1")
        .bind(&connector_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_connector(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_connector(&state, &user_id, &connector_id).await?;
    let (token, prefix) = rotate_token(state.pool(), &connector_id).await?;
    let connector = find_connector(state.pool(), &connector_id)
        .await?
        .ok_or_else(|| ApiError::not_found("connector not found"))?;
    Ok(Json(json!({
        "connector": connector.view(),
        "connectorToken": token,
        "tokenPrefix": prefix,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn get_connector_preferences(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_connector(&state, &user_id, &connector_id).await?;
    // Preferences are pushed by the daemon; until an ingress exists there is
    // nothing stored, and an empty object says exactly that.
    Ok(Json(json!({
        "connectorId": connector_id,
        "preferences": {},
        "serverTime": timeutil::utc_now(),
    })))
}

async fn archive_all_sessions(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
    Json(payload): Json<ArchiveAllRequest>,
) -> ApiResult<Json<Value>> {
    owned_connector(&state, &user_id, &connector_id).await?;
    let scope = payload.scope.unwrap_or_else(|| String::from("active"));
    if !matches!(scope.as_str(), "active" | "archived" | "all") {
        return Err(ApiError::unprocessable("invalid scope"));
    }
    let now = timeutil::utc_now();
    let scope_filter = match scope.as_str() {
        "active" => "and archived = false",
        "archived" => "and archived = true",
        _ => "",
    };
    let statement = format!(
        "update sessions
            set archived = $2,
                archived_at = case when $2 then $3 else null end,
                updated_at = $3
          where connector_id = $1 and user_id = $4 {scope_filter}
      returning id, user_id, connector_id, project_id, runtime, runtime_id, origin,
                external_session_id, title, title_source, cwd, status, takeover, pinned,
                pinned_at, archived, archived_at, source_state, last_read_seq,
                latest_turn_end_seq, last_synced_at, source_observed_at, last_activity_at,
                sort_at, seq, seq_allocated_high, updated_seq, created_at, updated_at"
    );
    let rows = sqlx::query_as::<_, SessionRow>(&statement)
        .bind(&connector_id)
        .bind(payload.archived)
        .bind(&now)
        .bind(&user_id)
        .fetch_all(state.pool())
        .await?;
    let sessions: Vec<Value> = rows.iter().map(|row| row.view("offline")).collect();
    Ok(Json(json!({
        "sessions": sessions,
        "affected": sessions.len(),
        "serverTime": timeutil::utc_now(),
    })))
}
