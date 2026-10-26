//! `/auth/mobile-login/*` — QR login for the mobile client.
//!
//! Contract source: upstream `server/agent_server/api/auth.py`
//! (`create_mobile_login_qr` / `exchange_mobile_login` /
//! `refresh_mobile_login`).
//!
//! Two deliberate divergences, both in the stricter direction:
//!
//! * Every token carries a `kind` claim and each endpoint accepts only its own
//!   (see `crate::auth`). Upstream signs QR tokens and refresh tokens with the
//!   same HMAC and separates them by inspecting the claims at each call site;
//!   doing only that here would leave access, QR and refresh tokens on the same
//!   footing for `AuthedUser`, which is the privilege escalation this module
//!   must not have.
//! * The QR token is a row that is *deleted* when spent, so a database cannot be
//!   replayed into a login and "already used" has no separate flag to forget.
//!
//! The QR payload itself is a Web concern: the hub only ever hands back the
//! token and its deadline.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{
    create_mobile_login_token, create_mobile_refresh_token, create_user_access_token, sha256_hex,
    verify_mobile_refresh_token, verify_signed_token, MOBILE_REFRESH_TTL_SECONDS,
    TOKEN_KIND_MOBILE_LOGIN,
};
use crate::error::{ApiError, ApiResult};
use crate::models::UserRow;
use crate::routes::auth::find_user_by_id;
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

/// The single rejection every failure mode of one endpoint shares. Distinct
/// wording per failure would tell an attacker which half of "expired" vs "wrong
/// kind" vs "already spent" they got wrong.
const INVALID_LOGIN_TOKEN: &str = "invalid or expired mobile login token";
const INVALID_REFRESH_TOKEN: &str = "invalid or expired mobile refresh token";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/mobile-login/qr", post(create_qr))
        .route("/auth/mobile-login/exchange", post(exchange))
        .route("/auth/mobile-login/refresh", post(refresh))
}

async fn create_qr(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // A disabled account must fail here, not later at exchange: `/auth/me`, exchange
    // and refresh all reject it, so letting the QR mint makes the UI show "your
    // code is ready" for an account that cannot sign in.
    let user = load_active_user(&state, &user_id).await?;
    let (login_token, expires_epoch) = create_mobile_login_token(&state.config().secret, &user.id);
    // Hashed at rest, exactly like ws_tickets: the plaintext leaves through the
    // response body and nowhere else — not into a row, not into the log.
    sqlx::query(
        "insert into mobile_login_tokens (token_hash, user_id, expires_at_epoch, created_at)
         values ($1, $2, $3, $4)",
    )
    .bind(sha256_hex(login_token.as_bytes()))
    .bind(&user_id)
    .bind(expires_epoch)
    .bind(timeutil::utc_now())
    .execute(state.pool())
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "userId": user_id,
            "loginToken": login_token,
            "expiresAt": timeutil::iso_from_epoch(expires_epoch),
            "serverTime": timeutil::utc_now(),
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ExchangeRequest {
    #[serde(default, rename = "loginToken")]
    pub login_token: String,
    #[serde(default, rename = "userId")]
    pub user_id: String,
}

/// Unauthenticated by design: the phone holds no account credentials, the QR
/// token is the credential. It names the account twice — in its own `sub` claim
/// and in the body's `userId` — and both must name the same user.
async fn exchange(
    State(state): State<AppState>,
    Json(payload): Json<ExchangeRequest>,
) -> ApiResult<Json<Value>> {
    let subject = verify_signed_token(
        &state.config().secret,
        TOKEN_KIND_MOBILE_LOGIN,
        &payload.login_token,
    )
    .ok_or_else(|| ApiError::unauthorized(INVALID_LOGIN_TOKEN))?;
    if subject != payload.user_id {
        return Err(ApiError::unauthorized(INVALID_LOGIN_TOKEN));
    }

    // Single use, enforced by the database: `delete ... returning` makes the
    // consumption the same statement as the lookup, so two phones racing the same
    // QR cannot both get a session.
    let consumed = sqlx::query(
        "delete from mobile_login_tokens
          where token_hash = $1 and user_id = $2 and expires_at_epoch >= $3
      returning token_hash",
    )
    .bind(sha256_hex(payload.login_token.as_bytes()))
    .bind(&subject)
    .bind(timeutil::unix_now())
    .fetch_optional(state.pool())
    .await?;
    if consumed.is_none() {
        return Err(ApiError::unauthorized(INVALID_LOGIN_TOKEN));
    }

    let user = load_active_user(&state, &subject).await?;
    let secret = &state.config().secret;
    Ok(Json(json!({
        "auth": user.auth_response(&create_user_access_token(secret, &user.id)),
        "refreshToken": create_mobile_refresh_token(secret, &user.id),
        "expiresAt": timeutil::iso_from_epoch(timeutil::unix_now() + MOBILE_REFRESH_TTL_SECONDS),
        "serverTime": timeutil::utc_now(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    #[serde(default, rename = "refreshToken")]
    pub refresh_token: String,
}

/// Unauthenticated: an expired access token is precisely when a client needs
/// this, so it cannot require one. The refresh token is stateless — the account
/// row is the only revocation switch, which is why a disabled user is rejected
/// here too.
async fn refresh(
    State(state): State<AppState>,
    Json(payload): Json<RefreshRequest>,
) -> ApiResult<Json<Value>> {
    let subject = verify_mobile_refresh_token(&state.config().secret, &payload.refresh_token)
        .ok_or_else(|| ApiError::unauthorized(INVALID_REFRESH_TOKEN))?;
    let user = load_active_user(&state, &subject).await?;
    let access = create_user_access_token(&state.config().secret, &user.id);
    Ok(Json(user.auth_response(&access)))
}

async fn load_active_user(state: &AppState, user_id: &str) -> ApiResult<UserRow> {
    let user = find_user_by_id(state.pool(), user_id)
        .await?
        .ok_or_else(|| ApiError::unauthorized("user no longer exists"))?;
    if user.disabled {
        return Err(ApiError::forbidden("account disabled"));
    }
    Ok(user)
}
