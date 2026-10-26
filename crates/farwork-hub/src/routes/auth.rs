//! `/auth/*` — config, password salt, register, login, me.
//!
//! Contract source: upstream `server/agent_server/api/auth.py` plus the
//! browser-side verifier in `web-next/src/features/auth/password-verifier.ts`.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::auth::{
    create_user_access_token, derive_password_verifier, hash_password_verifier,
    password_salt_from_hash, random_salt, verify_password, verify_password_verifier,
};
use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::UserRow;
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

pub const SETTING_REGISTRATION_OPEN: &str = "registration_open";
pub const SETTING_EMAIL_VERIFICATION_REQUIRED: &str = "email_verification_required";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/config", get(auth_config))
        .route("/auth/password-salt", post(password_salt))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/me", get(me))
}

#[derive(Debug, Deserialize)]
pub struct EmailRequest {
    #[serde(default)]
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthRequest {
    #[serde(default)]
    pub email: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default, rename = "passwordVerifier")]
    pub password_verifier: Option<String>,
    #[serde(default, rename = "passwordSalt")]
    pub password_salt: Option<String>,
    #[serde(default, rename = "setupToken")]
    pub setup_token: Option<String>,
}

pub fn normalize_email(raw: &str) -> ApiResult<String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() || !value.contains('@') || value.len() > 320 {
        return Err(ApiError::unprocessable("invalid email address"));
    }
    Ok(value)
}

pub fn normalize_display_name(raw: Option<&str>, email: &str) -> String {
    let candidate = raw.map(|value| value.trim()).unwrap_or_default();
    let fallback = email.split('@').next().unwrap_or(email);
    let value = if candidate.is_empty() {
        fallback
    } else {
        candidate
    };
    value.chars().take(64).collect()
}

pub async fn count_users(pool: &PgPool) -> Result<i64, sqlx::Error> {
    use sqlx::Row as _;
    let row = sqlx::query("select count(*) as total from users")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("total"))
}

/// Boolean instance switch stored in `app_settings` as the string `"true"`/`"false"`.
pub async fn bool_setting(pool: &PgPool, key: &str, default: bool) -> Result<bool, sqlx::Error> {
    use sqlx::Row as _;
    let row = sqlx::query("select value from app_settings where key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row
        .map(|row| row.get::<String, _>("value") == "true")
        .unwrap_or(default))
}

pub async fn set_bool_setting(pool: &PgPool, key: &str, value: bool) -> Result<(), sqlx::Error> {
    let now = timeutil::utc_now();
    sqlx::query(
        "insert into app_settings (key, value, updated_at) values ($1, $2, $3)
         on conflict (key) do update set value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(if value { "true" } else { "false" })
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn registration_open(pool: &PgPool) -> Result<bool, sqlx::Error> {
    // A fresh instance keeps self-service registration closed until an explicit
    // admin action; bootstrap still works through the setup token.
    bool_setting(pool, SETTING_REGISTRATION_OPEN, false).await
}

/// Reported by `/auth/config` and `/admin/settings`. This build has no mailer, so
/// the flag is stored and reported but cannot gate registration yet — see the
/// hub README §7.
pub async fn email_verification_required(pool: &PgPool) -> Result<bool, sqlx::Error> {
    bool_setting(pool, SETTING_EMAIL_VERIFICATION_REQUIRED, false).await
}

pub async fn find_user_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>(
        "select id, email, email_verified_at, display_name, password_hash, role, disabled, avatar,
                created_at, updated_at
           from users
          where lower(email) = $1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
}

pub async fn find_user_by_id(pool: &PgPool, user_id: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>(
        "select id, email, email_verified_at, display_name, password_hash, role, disabled, avatar,
                created_at, updated_at
           from users
          where id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Resolve the stored password hash from an `AuthRequest` (verifier preferred,
/// plaintext fallback), mirroring upstream `_password_hash_from_payload`.
fn password_hash_from_payload(payload: &AuthRequest) -> ApiResult<Option<String>> {
    if let Some(verifier) = payload.password_verifier.as_deref() {
        let salt = payload
            .password_salt
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::unprocessable("password salt is required"))?;
        return Ok(Some(hash_password_verifier(verifier, salt)?));
    }
    if let Some(password) = payload.password.as_deref() {
        return Ok(Some(password_hash_from_plaintext(password)?));
    }
    Ok(None)
}

/// Server-side derivation for a plaintext password: the same PBKDF2 verifier
/// the browser computes, with a salt minted here. `POST /admin/users` reuses it,
/// so an admin-created account logs in through the identical path.
pub fn password_hash_from_plaintext(password: &str) -> ApiResult<String> {
    let salt = random_salt();
    let verifier = derive_password_verifier(password, &salt);
    hash_password_verifier(&verifier, &salt)
}

async fn auth_config(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let pool = state.pool();
    let needs_bootstrap = count_users(pool).await? == 0;
    let registration_open = registration_open(pool).await?;
    // Touching the setup token here is intentional: a stale token rotates now,
    // so the operator sees the fresh one in the log on the next refresh.
    let setup_token_expires_at = if needs_bootstrap {
        let (_token, expires_epoch) = state.setup_token().current();
        Some(timeutil::iso_from_epoch(expires_epoch))
    } else {
        None
    };
    Ok(Json(json!({
        "emailVerificationRequired": email_verification_required(pool).await?,
        "needsBootstrap": needs_bootstrap,
        "registrationOpen": registration_open,
        "oauthRegistrationOpen": false,
        "oauthEnabled": false,
        "oauthProviderLabel": null,
        "passwordResetEnabled": false,
        "setupTokenExpiresAt": setup_token_expires_at,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn password_salt(
    State(state): State<AppState>,
    Json(payload): Json<EmailRequest>,
) -> ApiResult<Json<Value>> {
    let email = normalize_email(&payload.email)?;
    let salt = match find_user_by_email(state.pool(), &email).await? {
        Some(user) => password_salt_from_hash(&user.password_hash)
            .map(|value| value.to_string())
            .unwrap_or_else(random_salt),
        // Registration path: the client picks the salt it will submit with.
        None => random_salt(),
    };
    Ok(Json(
        json!({ "salt": salt, "serverTime": timeutil::utc_now() }),
    ))
}

async fn register(
    State(state): State<AppState>,
    Json(payload): Json<AuthRequest>,
) -> ApiResult<Json<Value>> {
    let pool = state.pool();
    let email = normalize_email(&payload.email)?;
    let display_name = normalize_display_name(payload.display_name.as_deref(), &email);
    let password_hash = password_hash_from_payload(&payload)?
        .ok_or_else(|| ApiError::unprocessable("password is required"))?;

    let is_bootstrap = count_users(pool).await? == 0;
    if is_bootstrap {
        let supplied = payload.setup_token.as_deref().unwrap_or_default();
        if state.setup_token().verify(supplied).is_none() {
            return Err(ApiError::unauthorized(
                "invalid or expired setup token — find the current token in the server log",
            ));
        }
    } else if !registration_open(pool).await? {
        return Err(ApiError::forbidden("registration is closed"));
    }

    let role = if is_bootstrap { "admin" } else { "member" };
    let now = timeutil::utc_now();
    let user_id = ids::user_id();

    let mut tx = pool.begin().await?;
    if is_bootstrap {
        // Serialize concurrent first-account creation, as upstream does.
        sqlx::query("select pg_advisory_xact_lock(4141454)")
            .execute(&mut *tx)
            .await?;
    }
    let inserted = sqlx::query_as::<_, UserRow>(
        "insert into users
            (id, email, display_name, password_hash, role, disabled, created_at, updated_at)
         values ($1, $2, $3, $4, $5, false, $6, $6)
         returning id, email, email_verified_at, display_name, password_hash, role, disabled,
                   avatar, created_at, updated_at",
    )
    .bind(&user_id)
    .bind(&email)
    .bind(&display_name)
    .bind(&password_hash)
    .bind(role)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await;

    let user = match inserted {
        Ok(user) => user,
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(ApiError::conflict("email is already in use"));
        }
        Err(error) => return Err(error.into()),
    };
    tx.commit().await?;

    if is_bootstrap {
        state.setup_token().consume();
        // Upstream leaves registration closed after bootstrap; mirror that.
        set_registration_open(pool, false).await?;
    }

    let token = create_user_access_token(&state.config().secret, &user.id);
    Ok(Json(user.auth_response(&token)))
}

pub async fn set_registration_open(pool: &PgPool, open: bool) -> Result<(), sqlx::Error> {
    set_bool_setting(pool, SETTING_REGISTRATION_OPEN, open).await
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<AuthRequest>,
) -> ApiResult<Json<Value>> {
    let email = normalize_email(&payload.email)?;
    let user = find_user_by_email(state.pool(), &email)
        .await?
        .ok_or_else(|| ApiError::unauthorized("invalid credentials"))?;
    if user.disabled {
        return Err(ApiError::forbidden("account disabled"));
    }
    let verified = match payload.password_verifier.as_deref() {
        Some(verifier) if !verifier.is_empty() => {
            verify_password_verifier(&user.password_hash, verifier)
        }
        _ => match payload.password.as_deref() {
            Some(password) if !password.is_empty() => {
                verify_password(&user.password_hash, password)
            }
            _ => false,
        },
    };
    if !verified {
        return Err(ApiError::unauthorized("invalid credentials"));
    }
    let token = create_user_access_token(&state.config().secret, &user.id);
    Ok(Json(user.auth_response(&token)))
}

async fn me(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
) -> ApiResult<Json<Value>> {
    let user = find_user_by_id(state.pool(), &user_id)
        .await?
        .ok_or_else(|| ApiError::unauthorized("user no longer exists"))?;
    if user.disabled {
        return Err(ApiError::forbidden("account disabled"));
    }
    Ok(Json(user.me_response()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalization_rejects_garbage() {
        assert_eq!(
            normalize_email("  Foo@Bar.COM ").expect("ok"),
            "foo@bar.com"
        );
        assert!(normalize_email("nope").is_err());
        assert!(normalize_email("").is_err());
    }

    #[test]
    fn display_name_defaults_to_the_local_part() {
        assert_eq!(normalize_display_name(None, "ada@example.com"), "ada");
        assert_eq!(
            normalize_display_name(Some("  Ada  "), "ada@example.com"),
            "Ada"
        );
    }
}
