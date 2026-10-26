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
/// separators are stripped so `/w/app` and `/w/app/` share one project, and
/// case is folded so a case-only difference cannot fork one workspace into two
/// projects: macOS and Windows default to case-insensitive paths, and a session
/// silently landing in a twin project is worse than merging two directories the
/// user typed differently.
pub fn workspace_key(path: &str) -> String {
    let trimmed = path.trim();
    let stripped = trimmed.trim_end_matches('/');
    let key = if stripped.is_empty() { "/" } else { stripped };
    key.to_lowercase()
}

/// Absolute on every platform a connector can report: a POSIX `/...`, a
/// Windows `C:\...`, or a UNC share. A relative cwd names no workspace, so
/// it must never be promoted into a project.
pub fn is_absolute_workspace(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with("\\\\") {
        return true;
    }
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(letter), Some(':'), Some(sep)) => {
            letter.is_ascii_alphabetic() && (sep == '\\' || sep == '/')
        }
        _ => false,
    }
}

/// Project name derived from a workspace path (upstream `workspaceName`): the
/// last path segment, or `Workspace` when the path has none.
pub fn workspace_name(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => String::from("Workspace"),
    }
}

/// `base`, or the first free `base (n)` — `uq_projects_user_name` is per user,
/// so an auto-created project must not steal a name the user already chose.
async fn available_project_name(state: &AppState, user_id: &str, base: &str) -> ApiResult<String> {
    let base: String = base.trim().chars().take(255).collect();
    let base = if base.is_empty() {
        String::from("Workspace")
    } else {
        base
    };
    let taken: Vec<String> = sqlx::query_scalar("select name from projects where user_id = $1")
        .bind(user_id)
        .fetch_all(state.pool())
        .await?;
    if !taken.iter().any(|name| name == &base) {
        return Ok(base);
    }
    for suffix in 1..=1000u32 {
        let ending = format!(" ({suffix})");
        let head: String = base.chars().take(255 - ending.chars().count()).collect();
        let candidate = format!("{head}{ending}");
        if !taken.iter().any(|name| name == &candidate) {
            return Ok(candidate);
        }
    }
    Err(ApiError::conflict(json!({
        "code": "project_name_conflict",
        "message": "cannot derive a unique project name for this workspace",
    })))
}

pub async fn find_workspace_project(
    pool: &sqlx::PgPool,
    user_id: &str,
    connector_id: &str,
    workspace_key: &str,
) -> Result<Option<ProjectRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(&format!(
        "select {PROJECT_COLUMNS} from projects
          where user_id = $1 and connector_id = $2 and workspace_key = $3"
    ))
    .bind(user_id)
    .bind(connector_id)
    .bind(workspace_key)
    .fetch_optional(pool)
    .await
}

/// The project that owns `cwd`, created on first sight (upstream
/// `findWorkspaceProject`). A DSH session arrives with only a cwd; without this
/// step the dashboard shows it as an orphan that belongs to no project.
///
/// `None` is the honest answer for a cwd that names no workspace (empty or
/// relative): that session stays project-less instead of being filed under a
/// directory the user never created.
pub async fn ensure_workspace_project(
    state: &AppState,
    user_id: &str,
    connector_id: &str,
    cwd: &str,
) -> ApiResult<Option<ProjectRow>> {
    let cwd = cwd.trim();
    if !is_absolute_workspace(cwd) {
        return Ok(None);
    }
    let key = workspace_key(cwd);
    // At most three rounds: one to find-or-create, one to re-read after losing
    // the insert race (or the name race), one spare. Losing both twice in a row
    // means something other than concurrency is wrong.
    for _ in 0..3 {
        if let Some(existing) =
            find_workspace_project(state.pool(), user_id, connector_id, &key).await?
        {
            return Ok(Some(existing));
        }
        let name = available_project_name(state, user_id, &workspace_name(cwd)).await?;
        let now = timeutil::utc_now();
        let inserted = sqlx::query_as::<_, ProjectRow>(&format!(
            "insert into projects
                (id, user_id, connector_id, name, workspace_path, workspace_key,
                 manually_created, pinned, created_at, updated_at)
             values ($1, $2, $3, $4, $5, $6, false, false, $7, $7)
             returning {PROJECT_COLUMNS}"
        ))
        .bind(ids::project_id())
        .bind(user_id)
        .bind(connector_id)
        .bind(&name)
        .bind(cwd)
        .bind(&key)
        .bind(&now)
        .fetch_one(state.pool())
        .await;
        match inserted {
            Ok(row) => {
                publish_dashboard_change(state, user_id);
                return Ok(Some(row));
            }
            // Concurrent sessions for one workspace both find nothing and both
            // insert; `uq_projects_user_connector_workspace` lets exactly one
            // win, and the loser re-reads that row instead of forking a twin.
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => continue,
            Err(error) => return Err(error.into()),
        }
    }
    find_workspace_project(state.pool(), user_id, connector_id, &key)
        .await?
        .map(Some)
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "project_name_conflict",
                "message": "cannot bind this workspace to a project",
            }))
        })
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

    #[test]
    fn workspace_key_folds_case_so_one_directory_is_one_project() {
        assert_eq!(workspace_key("/w/App/"), workspace_key("/w/app"));
        assert_eq!(workspace_key("/W/APP"), "/w/app");
    }

    #[test]
    fn only_absolute_paths_name_a_workspace() {
        assert!(is_absolute_workspace("/tmp/fw"));
        assert!(is_absolute_workspace("C:\\work"));
        assert!(is_absolute_workspace("c:/work"));
        assert!(!is_absolute_workspace("relative/path"));
        assert!(!is_absolute_workspace("./x"));
        assert!(!is_absolute_workspace(""));
        assert!(!is_absolute_workspace("~/x"));
    }

    #[test]
    fn workspace_name_uses_the_last_segment() {
        assert_eq!(workspace_name("/tmp/fw-autoproj"), "fw-autoproj");
        assert_eq!(workspace_name("/tmp/fw-autoproj/"), "fw-autoproj");
        assert_eq!(workspace_name("/"), "Workspace");
    }
}
