//! Pairing codes: how a device joins an account without a hand-copied token.
//!
//! * `POST /pairing/start` (logged in) mints a one-time code and returns it in
//!   plaintext exactly once; only its digest is stored.
//! * `POST /pairing/claim` (**no** credentials) trades that code for a connector
//!   token — the same object `POST /connectors` returns, so everything the agent
//!   does afterwards (connecting, running sessions) is unchanged.
//! * `GET /pairing/{id}` is the browser's poll for "has my device claimed it yet".
//!
//! ## Why claim is a separate, unauthenticated router
//!
//! The device that claims a code has no account yet: the code *is* its credential
//! (that is the whole point of pairing, and why it is short-lived and one-time).
//! Like `/public/shares`, it is mounted by [public_router] with no [AuthedUser],
//! and its response is a four-key connector view rather than the full
//! [crate::models::ConnectorRow] projection — an anonymous caller learns the
//! device it just created and nothing else about the account.
//!
//! ## Why the limiter keys on the TCP peer address only
//!
//! `X-Forwarded-For` is caller-controlled unless a trusted proxy rewrites it, and
//! this build has no notion of a trusted proxy; honouring the header would let a
//! direct caller rotate the key and dodge the limit completely. The cost is that
//! behind a reverse proxy every claim shares the proxy's bucket. That is the
//! honest failure mode (too strict) instead of an exploitable one (no limit).

use std::convert::Infallible;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::pairing;
use crate::routes::connectors;
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

/// Logged-in surface: mint a code, read its status.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pairing/start", post(start_pairing))
        .route("/pairing/{pairing_id}", get(get_pairing))
}

/// The unauthenticated surface. Nothing here may take an [AuthedUser].
pub fn public_router() -> Router<AppState> {
    Router::new().route("/pairing/claim", post(claim_pairing))
}

/// Source address for rate limiting.
///
/// `ConnectInfo` is only in the request extensions when the server is served
/// with connect info (see `main`); an embedded router has no peer address at all,
/// so those calls all share [pairing::UNKNOWN_SOURCE] and stay rate-limited.
struct ClientIp(String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let source = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip().to_string())
            .unwrap_or_else(|| String::from(pairing::UNKNOWN_SOURCE));
        Ok(ClientIp(source))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest {
    #[serde(default, alias = "ttl_seconds")]
    ttl_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ClaimRequest {
    code: String,
    name: String,
    /// What the device calls its operating system. Optional: an agent that does
    /// not know must be able to pair anyway, so it is reported as `null`
    /// (`connectors.device_os` is nullable) instead of being invented here.
    #[serde(default)]
    platform: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct PairingRow {
    id: String,
    user_id: String,
    expires_at_epoch: i64,
    claimed_at: Option<String>,
    connector_id: Option<String>,
}

fn invalid_code() -> ApiError {
    ApiError::not_found(json!({
        "code": "invalid_code",
        "message": "pairing code is not valid",
    }))
}

fn code_expired() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        json!({
            "code": "code_expired",
            "message": "pairing code has expired",
        }),
    )
}

fn already_claimed() -> ApiError {
    ApiError::conflict(json!({
        "code": "code_already_claimed",
        "message": "pairing code has already been used",
    }))
}

fn too_many_attempts() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        json!({
            "code": "too_many_attempts",
            "message": format!(
                "more than {} pairing claims in a minute from this address",
                pairing::CLAIM_ATTEMPTS_PER_MINUTE
            ),
        }),
    )
}

async fn start_pairing(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<StartRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ttl_seconds = payload.ttl_seconds.unwrap_or(pairing::DEFAULT_TTL_SECONDS);
    if !(pairing::MIN_TTL_SECONDS..=pairing::MAX_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(ApiError::unprocessable(format!(
            "ttlSeconds must be between {} and {}",
            pairing::MIN_TTL_SECONDS,
            pairing::MAX_TTL_SECONDS
        )));
    }

    let pairing_id = ids::pairing_id();
    let expires_at_epoch = timeutil::unix_now() + ttl_seconds;
    // A digest collision would shadow a live code, so the unique index rejects it
    // and a fresh code is tried instead of failing the request.
    let mut code = pairing::generate_code();
    let mut stored = false;
    for _ in 0..5 {
        let code_hash = pairing::hash_code(&state.config().secret, &pairing::normalize_code(&code));
        let result = sqlx::query(
            "insert into pairings (id, user_id, code_hash, created_at, expires_at_epoch)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(&pairing_id)
        .bind(&user_id)
        .bind(&code_hash)
        .bind(timeutil::utc_now())
        .bind(expires_at_epoch)
        .execute(state.pool())
        .await;
        match result {
            Ok(_) => {
                stored = true;
                break;
            }
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
                code = pairing::generate_code();
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !stored {
        return Err(ApiError::internal("could not allocate a pairing code"));
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "pairing": {
                "id": pairing_id,
                // The only time this value is ever sent: the table holds a digest.
                "code": code,
                "expiresAt": timeutil::iso_from_epoch(expires_at_epoch),
            },
            "serverTime": timeutil::utc_now(),
        })),
    ))
}

/// No [AuthedUser]: the code is the credential. Everything the caller may see is
/// in the response — the connector it created, never the account behind it.
async fn claim_pairing(
    State(state): State<AppState>,
    ClientIp(source): ClientIp,
    Json(payload): Json<ClaimRequest>,
) -> ApiResult<Json<Value>> {
    if !state
        .pairing_limiter()
        .allows(&source, timeutil::unix_now())
    {
        return Err(too_many_attempts());
    }

    let code = pairing::normalize_code(&payload.code);
    if !pairing::is_well_formed(&code) {
        return Err(invalid_code());
    }
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::unprocessable("name must not be empty"));
    }
    let device_os = payload
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let code_hash = pairing::hash_code(&state.config().secret, &code);
    let pairing_row = sqlx::query_as::<_, PairingRow>(
        "select id, user_id, expires_at_epoch, claimed_at, connector_id
           from pairings where code_hash = $1",
    )
    .bind(&code_hash)
    .fetch_optional(state.pool())
    .await?
    .ok_or_else(invalid_code)?;

    // "Already claimed" outranks "expired": once a code is spent, answering
    // "expired" would invite a new code for a device that already exists.
    if pairing_row.claimed_at.is_some() {
        return Err(already_claimed());
    }
    if pairing_row.expires_at_epoch <= timeutil::unix_now() {
        return Err(code_expired());
    }

    // One transaction: the connector must not survive a lost race for the pairing
    // row, and the row must not be marked claimed if the connector failed.
    let mut tx = state.pool().begin().await?;
    let (connector, token) = connectors::insert_connector(
        &mut *tx,
        &ids::connector_id(),
        &pairing_row.user_id,
        &name,
        "cli",
        device_os.as_deref(),
    )
    .await?;
    let claimed = sqlx::query(
        "update pairings
            set claimed_at = $2, connector_id = $3, attempt_ip = $4
          where id = $1 and claimed_at is null",
    )
    .bind(&pairing_row.id)
    .bind(timeutil::utc_now())
    .bind(&connector.id)
    .bind(&source)
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 0 {
        // Dropping `tx` rolls the connector insert back; the other claim wins.
        return Err(already_claimed());
    }
    tx.commit().await?;

    Ok(Json(json!({
        "connector": {
            "id": connector.id,
            "name": connector.name,
            "deviceOs": connector.device_os,
            "status": connector.status,
        },
        "connectorToken": token,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn get_pairing(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(pairing_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let row = sqlx::query_as::<_, PairingRow>(
        "select id, user_id, expires_at_epoch, claimed_at, connector_id
           from pairings where id = $1 and user_id = $2",
    )
    .bind(&pairing_id)
    .bind(&user_id)
    .fetch_optional(state.pool())
    .await?;

    // An id that is unknown *or* someone else's is reported as "unknown" rather
    // than 404: a poller then has one code path for "no such pairing", and the two
    // cases stay indistinguishable, so this cannot probe other users.
    let Some(row) = row else {
        return Ok(Json(json!({
            "pairing": {
                "id": pairing_id,
                "status": "unknown",
                "expiresAt": Value::Null,
            },
            "serverTime": timeutil::utc_now(),
        })));
    };

    let status = if row.claimed_at.is_some() {
        "claimed"
    } else if row.expires_at_epoch <= timeutil::unix_now() {
        "expired"
    } else {
        "pending"
    };
    let mut pairing_view = json!({
        "id": row.id,
        "status": status,
        "expiresAt": timeutil::iso_from_epoch(row.expires_at_epoch),
    });
    if let Some(connector_id) = row.connector_id {
        // Present only once a device claimed the code: a pending pairing has no
        // connector to name, and a null key would read as "claimed by nobody".
        pairing_view["connectorId"] = Value::String(connector_id);
    }
    Ok(Json(json!({
        "pairing": pairing_view,
        "serverTime": timeutil::utc_now(),
    })))
}
