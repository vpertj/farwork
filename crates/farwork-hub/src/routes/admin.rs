//! `/admin/*` — instance administration: the user directory and
//! the instance-wide switches.
//!
//! ## Who may call this
//!
//! Every handler takes [AuthedUser], so a request without a bearer token is a
//! 401 raised by the extractor itself. [require_admin] then re-reads the row:
//! the caller must still exist, must not be disabled, and must have
//! `role = 'admin'`. There is deliberately no `is_admin` column — `users.role`
//! stays the single source of truth, so an account cannot be half-admin.
//!
//! ## Never lock the instance out of its own administration
//!
//! [lockout_reason] decides all of it, and every rejection is a 409 whose body
//! is `{"detail":{"code":"admin_lockout","reason":...,"message":...}}`:
//!
//! * `self_disable` — an admin may not set `disabled = true` on itself;
//! * `last_admin`   — nobody may demote **or** disable the last admin that can
//!   still log in, whichever account that is;
//! * `self_demote`  — an admin may not demote itself to `member` either (with
//!   only one admin left, `last_admin` is reported first because it is the
//!   stronger statement).
//!
//! The active-admin count is read inside the same transaction that holds
//! `for update` locks on those admin rows, so two concurrent demotions cannot
//! both observe "2 admins" and both succeed. The invariant is enforced by
//! Postgres, not by a check-then-write race in a single process.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::UserRow;
use crate::routes::auth::{
    find_user_by_id, normalize_display_name, normalize_email, password_hash_from_plaintext,
    registration_open, set_bool_setting, SETTING_EMAIL_VERIFICATION_REQUIRED,
};
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

/// `admin_lockout` is the machine-readable code for every 409 this module
/// returns; `reason` narrows it to the rule that fired.
const CODE_LOCKOUT: &str = "admin_lockout";

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;

/// [UserRow] needs every column, `password_hash` included; the response is
/// built by [user_view], which never reads it.
const USER_COLUMNS: &str = "id, email, email_verified_at, display_name, password_hash, role, disabled, avatar, created_at, updated_at";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(list_users).post(create_user))
        .route("/admin/users/{user_id}", patch(update_user))
        .route("/admin/settings", get(read_settings).patch(update_settings))
}

// --------------------------------------------------------------- guard

fn forbidden(message: &str) -> ApiError {
    ApiError::forbidden(json!({ "code": "forbidden", "message": message }))
}

async fn require_admin(state: &AppState, caller_id: &str) -> ApiResult<UserRow> {
    let caller = find_user_by_id(state.pool(), caller_id)
        .await?
        .ok_or_else(|| ApiError::unauthorized("user no longer exists"))?;
    if caller.disabled {
        return Err(forbidden("account disabled"));
    }
    if caller.role != ROLE_ADMIN {
        return Err(forbidden("admin role required"));
    }
    Ok(caller)
}

fn validate_role(raw: &str) -> ApiResult<String> {
    let role = raw.trim();
    match role {
        ROLE_ADMIN | ROLE_MEMBER => Ok(role.to_string()),
        other => Err(ApiError::unprocessable(format!(
            "unknown role {other:?} — expected \"admin\" or \"member\""
        ))),
    }
}

// ------------------------------------------------------------ projection

/// Wire projection of a user. A whitelist on purpose: deriving `Serialize` on
/// [UserRow] would publish `password_hash` (and with it the PBKDF2 salt) the day
/// someone adds a column to it.
fn user_view(row: &UserRow) -> Value {
    json!({
        "id": row.id,
        "email": row.email,
        "displayName": row.display_name,
        "role": row.role,
        "disabled": row.disabled,
        "createdAt": row.created_at,
        "emailVerified": row.email_verified_at.is_some(),
    })
}

// -------------------------------------------------------------- cursor

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserCursor {
    created_at: String,
    id: String,
}

fn encode_cursor(row: &UserRow) -> String {
    let cursor = UserCursor {
        created_at: row.created_at.clone(),
        id: row.id.clone(),
    };
    crate::auth::b64url_encode(&serde_json::to_vec(&cursor).unwrap_or_default())
}

fn decode_cursor(raw: &str) -> ApiResult<UserCursor> {
    let bytes =
        crate::auth::b64url_decode(raw).ok_or_else(|| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice::<UserCursor>(&bytes)
        .map_err(|_| ApiError::bad_request("invalid cursor"))
}

// ------------------------------------------------------------- lockout

/// Why this PATCH would lock the instance out of administration, if it would.
///
/// Pure so the rule is unit-testable; the live active-admin count is supplied
/// by the caller, which reads it under a row lock. Inputs are the target's
/// **current** role/disabled state plus the state the request would write.
pub fn lockout_reason(
    caller_id: &str,
    target_id: &str,
    target_role: &str,
    target_disabled: bool,
    next_role: &str,
    next_disabled: bool,
    active_admin_count: i64,
) -> Option<&'static str> {
    let is_self = caller_id == target_id;
    let loses_admin =
        target_role == ROLE_ADMIN && !target_disabled && (next_role != ROLE_ADMIN || next_disabled);

    if is_self && next_disabled {
        return Some("self_disable");
    }
    if loses_admin && active_admin_count <= 1 {
        return Some("last_admin");
    }
    if is_self && target_role == ROLE_ADMIN && next_role != ROLE_ADMIN {
        return Some("self_demote");
    }
    None
}

fn lockout_conflict(reason: &'static str) -> ApiError {
    let message = match reason {
        "self_disable" => "an admin cannot disable its own account; ask another admin",
        "last_admin" => "the last active admin cannot be demoted or disabled",
        "self_demote" => "an admin cannot demote its own account to member; ask another admin",
        _ => "this change would lock the instance out of administration",
    };
    ApiError::conflict(json!({
        "code": CODE_LOCKOUT,
        "reason": reason,
        "message": message,
    }))
}

// ------------------------------------------------------------ handlers

#[derive(Debug, Deserialize)]
pub struct UsersQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

async fn list_users(
    State(state): State<AppState>,
    AuthedUser(caller_id): AuthedUser,
    Query(query): Query<UsersQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &caller_id).await?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let cursor = match query.cursor.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(decode_cursor(raw)?),
        _ => None,
    };
    let has_cursor = cursor.is_some();
    let cursor = cursor.unwrap_or(UserCursor {
        created_at: String::new(),
        id: String::new(),
    });

    let statement = format!(
        "select {USER_COLUMNS} from users
          where ($1::boolean = false or (created_at, id) < ($2, $3))
          order by created_at desc, id desc
          limit $4"
    );
    let rows = sqlx::query_as::<_, UserRow>(&statement)
        .bind(has_cursor)
        .bind(&cursor.created_at)
        .bind(&cursor.id)
        .bind(limit + 1)
        .fetch_all(state.pool())
        .await?;

    let has_more = rows.len() as i64 > limit;
    let page: Vec<UserRow> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = has_more.then(|| page.last().map(encode_cursor)).flatten();
    let users: Vec<Value> = page.iter().map(user_view).collect();

    Ok(Json(json!({
        "users": users,
        "nextCursor": next_cursor,
        "serverTime": timeutil::utc_now(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

async fn create_user(
    State(state): State<AppState>,
    AuthedUser(caller_id): AuthedUser,
    Json(payload): Json<CreateUserRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&state, &caller_id).await?;
    let email = normalize_email(&payload.email)?;
    let role =
        validate_role(payload.role.as_deref().ok_or_else(|| {
            ApiError::unprocessable("role is required (\"admin\" or \"member\")")
        })?)?;
    let password = payload
        .password
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::unprocessable("password is required"))?;
    // Same derivation the register/plaintext-fallback path uses, so the account
    // logs in through either verifier or plaintext without a second convention.
    let password_hash = password_hash_from_plaintext(password)?;
    let display_name = normalize_display_name(payload.display_name.as_deref(), &email);
    let now = timeutil::utc_now();

    let inserted = sqlx::query_as::<_, UserRow>(&format!(
        "insert into users
            (id, email, display_name, password_hash, role, disabled, created_at, updated_at)
         values ($1, $2, $3, $4, $5, false, $6, $6)
         returning {USER_COLUMNS}"
    ))
    .bind(ids::user_id())
    .bind(&email)
    .bind(&display_name)
    .bind(&password_hash)
    .bind(&role)
    .bind(&now)
    .fetch_one(state.pool())
    .await;

    let user = match inserted {
        Ok(user) => user,
        // The unique index on lower(email) is the authority; a pre-check would
        // still race with a concurrent register.
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(ApiError::conflict("email is already in use"));
        }
        Err(error) => return Err(error.into()),
    };
    Ok((
        StatusCode::CREATED,
        Json(json!({ "user": user_view(&user), "serverTime": timeutil::utc_now() })),
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

async fn update_user(
    State(state): State<AppState>,
    AuthedUser(caller_id): AuthedUser,
    Path(target_id): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &caller_id).await?;
    let target = find_user_by_id(state.pool(), &target_id)
        .await?
        .ok_or_else(|| ApiError::not_found("user not found"))?;

    let next_role = match payload.role.as_deref() {
        Some(raw) => validate_role(raw)?,
        None => target.role.clone(),
    };
    let next_disabled = payload.disabled.unwrap_or(target.disabled);
    let next_display_name = match payload.display_name.as_deref() {
        Some(raw) if raw.trim().is_empty() => {
            return Err(ApiError::unprocessable("displayName cannot be empty"));
        }
        Some(raw) => Some(normalize_display_name(
            Some(raw),
            target.email.as_deref().unwrap_or_default(),
        )),
        None => None,
    };

    let mut tx = state.pool().begin().await?;
    // Lock every admin that can still log in. A concurrent demotion blocks here
    // instead of reading the same count and also passing the guard.
    let active_admins =
        sqlx::query("select id from users where role = $1 and disabled = false for update")
            .bind(ROLE_ADMIN)
            .fetch_all(&mut *tx)
            .await?;
    if let Some(reason) = lockout_reason(
        &caller_id,
        &target.id,
        &target.role,
        target.disabled,
        &next_role,
        next_disabled,
        active_admins.len() as i64,
    ) {
        return Err(lockout_conflict(reason));
    }

    let updated = sqlx::query_as::<_, UserRow>(&format!(
        "update users
            set role = $2,
                disabled = $3,
                display_name = coalesce($4, display_name),
                updated_at = $5
          where id = $1
          returning {USER_COLUMNS}"
    ))
    .bind(&target.id)
    .bind(&next_role)
    .bind(next_disabled)
    .bind(next_display_name)
    .bind(timeutil::utc_now())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "user": user_view(&updated),
        "serverTime": timeutil::utc_now(),
    })))
}

// ------------------------------------------------------------ settings

#[derive(Debug, Default, Deserialize)]
pub struct SettingsPatch {
    #[serde(default, rename = "registrationOpen")]
    pub registration_open: Option<bool>,
    #[serde(default, rename = "emailVerificationRequired")]
    pub email_verification_required: Option<bool>,
}

async fn settings_body(state: &AppState) -> ApiResult<Value> {
    let pool = state.pool();
    Ok(json!({
        "settings": {
            "registrationOpen": registration_open(pool).await?,
            "emailVerificationRequired":
                crate::routes::auth::email_verification_required(pool).await?,
        },
        "serverTime": timeutil::utc_now(),
    }))
}

async fn read_settings(
    State(state): State<AppState>,
    AuthedUser(caller_id): AuthedUser,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &caller_id).await?;
    Ok(Json(settings_body(&state).await?))
}

async fn update_settings(
    State(state): State<AppState>,
    AuthedUser(caller_id): AuthedUser,
    Json(payload): Json<SettingsPatch>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &caller_id).await?;
    let pool = state.pool();
    if let Some(open) = payload.registration_open {
        set_bool_setting(pool, crate::routes::auth::SETTING_REGISTRATION_OPEN, open).await?;
    }
    if let Some(required) = payload.email_verification_required {
        set_bool_setting(pool, SETTING_EMAIL_VERIFICATION_REQUIRED, required).await?;
    }
    Ok(Json(settings_body(&state).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One active admin: both self-changes are refused, and the last-admin rule
    // is what gets reported for the demotion.
    #[test]
    fn self_changes_and_the_last_admin_are_refused() {
        assert_eq!(
            lockout_reason("usr_a", "usr_a", ROLE_ADMIN, false, ROLE_ADMIN, true, 3),
            Some("self_disable")
        );
        assert_eq!(
            lockout_reason("usr_a", "usr_a", ROLE_ADMIN, false, ROLE_MEMBER, false, 1),
            Some("last_admin")
        );
        // Two active admins: the stronger statement is the self rule.
        assert_eq!(
            lockout_reason("usr_a", "usr_a", ROLE_ADMIN, false, ROLE_MEMBER, false, 2),
            Some("self_demote")
        );
        // Somebody else's admin row is demotable while another admin remains.
        assert_eq!(
            lockout_reason("usr_a", "usr_b", ROLE_ADMIN, false, ROLE_MEMBER, false, 2),
            None
        );
        // The concurrency case the row lock exists for: only one admin left.
        assert_eq!(
            lockout_reason("usr_a", "usr_b", ROLE_ADMIN, false, ROLE_MEMBER, false, 1),
            Some("last_admin")
        );
        // A disabled admin is not an active one, and a role-equal PATCH changes nothing.
        assert_eq!(
            lockout_reason("usr_a", "usr_b", ROLE_ADMIN, true, ROLE_MEMBER, true, 1),
            None
        );
        assert_eq!(
            lockout_reason("usr_a", "usr_a", ROLE_ADMIN, false, ROLE_ADMIN, false, 1),
            None
        );
    }

    #[test]
    fn invalid_roles_are_rejected() {
        assert_eq!(validate_role(" admin ").expect("ok"), ROLE_ADMIN);
        assert_eq!(validate_role("member").expect("ok"), ROLE_MEMBER);
        assert!(validate_role("owner").is_err());
        assert!(validate_role("").is_err());
    }
}
