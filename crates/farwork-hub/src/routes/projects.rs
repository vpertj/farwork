//! `/projects/*` — workspace bindings that sessions hang off.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::ProjectRow;
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).patch(patch_project).delete(delete_project),
        )
        .route(
            "/projects/{project_id}/sessions",
            get(list_project_sessions),
        )
}

const PROJECT_COLUMNS: &str = "id, user_id, connector_id, name, workspace_path, workspace_key,
            manually_created, pinned, pinned_at, created_at, updated_at";

#[derive(Debug, Deserialize)]
pub struct ProjectCreateRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "connectorId")]
    pub connector_id: String,
    #[serde(default, rename = "workspacePath")]
    pub workspace_path: String,
    #[serde(default, rename = "manuallyCreated")]
    pub manually_created: Option<bool>,
    #[serde(default, rename = "attachMatchingSessions")]
    pub attach_matching_sessions: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectPatchRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectSessionsQuery {
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Canonical workspace identity for the uniqueness constraint. Trailing
/// separators are stripped so `/w/app` and `/w/app/` share one project.
pub fn workspace_key(path: &str) -> String {
    let trimmed = path.trim();
    let stripped = trimmed.trim_end_matches('/');
    let key = if stripped.is_empty() { "/" } else { stripped };
    key.to_string()
}

pub async fn find_project(
    pool: &sqlx::PgPool,
    project_id: &str,
) -> Result<Option<ProjectRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(&format!(
        "select {PROJECT_COLUMNS} from projects where id = $1"
    ))
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

async fn owned_project(state: &AppState, user_id: &str, project_id: &str) -> ApiResult<ProjectRow> {
    find_project(state.pool(), project_id)
        .await?
        .filter(|row| row.user_id == user_id)
        .ok_or_else(|| ApiError::not_found("project not found"))
}

async fn list_projects(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
) -> ApiResult<Json<Value>> {
    let projects = project_views(&state, &user_id, None).await?;
    Ok(Json(json!({
        "projects": projects,
        "serverTime": timeutil::utc_now(),
    })))
}

pub async fn project_views(
    state: &AppState,
    user_id: &str,
    project_id: Option<&str>,
) -> ApiResult<Vec<Value>> {
    use sqlx::Row as _;
    let statement = format!(
        "select {PROJECT_COLUMNS},
                (select count(*) from sessions s
                  where s.project_id = projects.id and s.archived = false) as active_count,
                (select count(*) from sessions s
                  where s.project_id = projects.id and s.archived = true) as archived_count,
                (select max(s.last_activity_at) from sessions s
                  where s.project_id = projects.id) as last_activity_at
           from projects
          where user_id = $1 and ($2::text is null or id = $2)
          order by pinned desc, pinned_at desc nulls last, updated_at desc, id"
    );
    let rows = sqlx::query(&statement)
        .bind(user_id)
        .bind(project_id)
        .fetch_all(state.pool())
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let row_model = ProjectRow {
                id: row.get("id"),
                user_id: row.get("user_id"),
                connector_id: row.get("connector_id"),
                name: row.get("name"),
                workspace_path: row.get("workspace_path"),
                workspace_key: row.get("workspace_key"),
                manually_created: row.get("manually_created"),
                pinned: row.get("pinned"),
                pinned_at: row.get("pinned_at"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            };
            row_model.view(
                row.get::<i64, _>("active_count"),
                row.get::<i64, _>("archived_count"),
                row.get::<Option<String>, _>("last_activity_at"),
            )
        })
        .collect())
}

async fn get_project(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(project_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_project(&state, &user_id, &project_id).await?;
    let mut views = project_views(&state, &user_id, Some(&project_id)).await?;
    let project = views
        .pop()
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(json!({
        "project": project,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn create_project(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<ProjectCreateRequest>,
) -> ApiResult<Json<Value>> {
    let name = payload.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::unprocessable("name must be 1-255 characters"));
    }
    if payload.connector_id.trim().is_empty() {
        return Err(ApiError::unprocessable("connectorId is required"));
    }
    let workspace_path = payload.workspace_path.trim().to_string();
    if workspace_path.is_empty() || workspace_path.chars().count() > 4096 {
        return Err(ApiError::unprocessable(
            "workspacePath must be 1-4096 characters",
        ));
    }
    // attachMatchingSessions is accepted for compatibility and ignored: the
    // upstream v2 contract also never binds existing sessions at creation.
    let _ = payload.attach_matching_sessions;

    let connector = crate::routes::connectors::find_connector(state.pool(), &payload.connector_id)
        .await?
        .filter(|row| row.user_id == user_id && !row.revoked)
        .ok_or_else(|| ApiError::not_found("connector not found"))?;

    let now = timeutil::utc_now();
    let project_id = ids::project_id();
    let inserted = sqlx::query_as::<_, ProjectRow>(&format!(
        "insert into projects
            (id, user_id, connector_id, name, workspace_path, workspace_key, manually_created,
             pinned, created_at, updated_at)
         values ($1, $2, $3, $4, $5, $6, $7, false, $8, $8)
         returning {PROJECT_COLUMNS}"
    ))
    .bind(&project_id)
    .bind(&user_id)
    .bind(&connector.id)
    .bind(&name)
    .bind(&workspace_path)
    .bind(workspace_key(&workspace_path))
    .bind(payload.manually_created.unwrap_or(true))
    .bind(&now)
    .fetch_one(state.pool())
    .await;

    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            let constraint = error.constraint().unwrap_or_default().to_string();
            let message = if constraint.contains("workspace") {
                "this workspace already has a project"
            } else {
                "project name already exists"
            };
            return Err(ApiError::conflict(json!({
                "code": "project_name_conflict",
                "message": message,
            })));
        }
        Err(error) => return Err(error.into()),
    }

    publish_dashboard_change(&state, &user_id);
    let mut views = project_views(&state, &user_id, Some(&project_id)).await?;
    let project = views
        .pop()
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(json!({
        "project": project,
        "attachedSessions": 0,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn patch_project(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(project_id): Path<String>,
    Json(payload): Json<ProjectPatchRequest>,
) -> ApiResult<Json<Value>> {
    let existing = owned_project(&state, &user_id, &project_id).await?;
    if payload.name.is_none() && payload.pinned.is_none() {
        return Err(ApiError::unprocessable(
            "at least one project field is required",
        ));
    }
    let now = timeutil::utc_now();
    let name = match payload.name {
        Some(name) => {
            let trimmed = name.trim().to_string();
            if trimmed.is_empty() || trimmed.chars().count() > 255 {
                return Err(ApiError::unprocessable("name must be 1-255 characters"));
            }
            Some(trimmed)
        }
        None => None,
    };
    let pinned_at = match payload.pinned {
        Some(true) => existing.pinned_at.clone().or_else(|| Some(now.clone())),
        Some(false) => None,
        None => existing.pinned_at.clone(),
    };
    let updated = sqlx::query_as::<_, ProjectRow>(&format!(
        "update projects
            set name = coalesce($2, name),
                pinned = coalesce($3, pinned),
                pinned_at = case when $3 is null then pinned_at else $4 end,
                updated_at = $5
          where id = $1
      returning {PROJECT_COLUMNS}"
    ))
    .bind(&project_id)
    .bind(name.as_deref())
    .bind(payload.pinned)
    .bind(pinned_at.as_deref())
    .bind(&now)
    .fetch_one(state.pool())
    .await;

    match updated {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(ApiError::conflict(json!({
                "code": "project_name_conflict",
                "message": "project name already exists",
            })));
        }
        Err(error) => return Err(error.into()),
    }

    publish_dashboard_change(&state, &user_id);
    let mut views = project_views(&state, &user_id, Some(&project_id)).await?;
    let project = views
        .pop()
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(json!({
        "project": project,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn delete_project(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(project_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_project(&state, &user_id, &project_id).await?;
    let now = timeutil::utc_now();
    let mut tx = state.pool().begin().await?;
    // Sessions outlive their project binding: detach instead of cascading.
    let detached =
        sqlx::query("update sessions set project_id = null, updated_at = $2 where project_id = $1")
            .bind(&project_id)
            .bind(&now)
            .execute(&mut *tx)
            .await?
            .rows_affected() as i64;
    sqlx::query("delete from projects where id = $1")
        .bind(&project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    publish_dashboard_change(&state, &user_id);
    Ok(Json(json!({
        "projectId": project_id,
        "detachedSessions": detached,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn list_project_sessions(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(project_id): Path<String>,
    Query(query): Query<ProjectSessionsQuery>,
) -> ApiResult<Json<Value>> {
    owned_project(&state, &user_id, &project_id).await?;
    let archived = query.archived.unwrap_or(false);
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let cursor = query
        .cursor
        .as_deref()
        .map(crate::routes::sessions::decode_cursor)
        .transpose()?;
    let (sessions, has_more, next_cursor) = crate::routes::sessions::list_session_rows(
        &state,
        &user_id,
        archived,
        limit,
        cursor,
        Some(&project_id),
    )
    .await?;
    Ok(Json(json!({
        "sessions": sessions,
        "hasMore": has_more,
        "nextCursor": next_cursor,
        "serverTime": timeutil::utc_now(),
    })))
}

pub fn publish_dashboard_change(state: &AppState, user_id: &str) {
    state.hub().publish_json(
        user_id,
        None,
        json!({ "type": "dashboard.changed" }).to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_key_strips_trailing_separators() {
        assert_eq!(workspace_key("/w/app"), "/w/app");
        assert_eq!(workspace_key(" /w/app/ "), "/w/app");
        assert_eq!(workspace_key("/"), "/");
        assert_eq!(workspace_key("   "), "/");
    }
}
