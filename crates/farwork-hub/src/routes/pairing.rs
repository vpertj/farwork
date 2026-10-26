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
use crate::models::ConnectorRow;
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
    /// The device's own stable id, from its `agent.json`. Optional so an agent
    /// that predates it keeps pairing: such a claim simply cannot be deduplicated
    /// (a name would be a guess, and a wrong guess merges two machines), which is
    /// why the unique index on it is partial.
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
}

/// The one index that means "this machine already paired": a violation is a race
/// to retry as a reuse, never a 500.
const AGENT_ID_INDEX: &str = "uq_connectors_user_agent_id";

/// What a finished claim produced: the row, its new token, and whether the device
/// already had it.
struct Claimed {
    connector: ConnectorRow,
    token: String,
    reused: bool,
}

/// One claim attempt's result, or the race the caller must resolve.
///
/// The row is boxed because [Claimed] dwarfs the race marker: an enum is as large
/// as its widest variant, and every "the index beat us" value would otherwise
/// carry a whole connector row it does not have.
enum Attempt {
    Done(Box<Claimed>),
    /// Another claim inserted this device between our lookup and our insert. The
    /// transaction is dead (Postgres aborts it), so the caller rolls back and
    /// retries as a reuse.
    AgentTaken,
}

fn agent_taken(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            database.code().as_deref() == Some("23505")
                && database.constraint() == Some(AGENT_ID_INDEX)
        }
        _ => false,
    }
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
    // An empty string is "not reported", exactly like a missing key: a device that
    // has no identity must not be able to occupy the unique index with "".
    let agent_id = payload
        .agent_id
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
    // row, and the row must not be marked claimed if the connector failed. A
    // device that pairs again reuses its own connector, so a second pair on one
    // machine rotates a token instead of adding a row.
    let mut tx = state.pool().begin().await?;
    let (connector, token, reused) = match attempt_claim(
        &mut tx,
        &pairing_row.user_id,
        &name,
        device_os.as_deref(),
        agent_id.as_deref(),
    )
    .await?
    {
        Attempt::Done(done) => {
            let Claimed {
                connector,
                token,
                reused,
            } = *done;
            mark_claimed(&mut tx, &pairing_row.id, &connector.id, &source).await?;
            tx.commit().await?;
            (connector, token, reused)
        }
        Attempt::AgentTaken => {
            // Rolling back is mandatory (the failed insert aborted the
            // transaction) *and* correct: this request must not keep a connector
            // of its own. Postgres reported the violation only after the other
            // claim committed, so that row is readable now.
            tx.rollback().await?;
            let mut tx = state.pool().begin().await?;
            let agent_id = agent_id
                .as_deref()
                .ok_or_else(|| ApiError::internal("device identity lost"))?;
            let existing =
                connectors::find_connector_by_agent(&mut *tx, &pairing_row.user_id, agent_id)
                    .await?
                    // The index made room for exactly one row holding this id, and
                    // it is not usable (revoked), so there is nothing to reuse and
                    // nothing this request may create. Not a 500: the user has an
                    // action to take.
                    .ok_or_else(|| {
                        ApiError::conflict(json!({
                            "code": "device_identity_taken",
                            "message": "this device identity belongs to an unusable connector; delete it in the web UI and pair again",
                        }))
                    })?;
            let (connector, token) =
                rotate_and_refresh(&mut tx, &existing, &name, device_os.as_deref()).await?;
            mark_claimed(&mut tx, &pairing_row.id, &connector.id, &source).await?;
            tx.commit().await?;
            (connector, token, true)
        }
    };

    Ok(Json(json!({
        "connector": {
            "id": connector.id,
            "name": connector.name,
            "deviceOs": connector.device_os,
            "status": connector.status,
        },
        "connectorToken": token,
        // The device logs this: "re-paired" and "paired" mean different things to
        // a user trying to get rid of a duplicate, and only the hub knows which.
        "reused": reused,
        "serverTime": timeutil::utc_now(),
    })))
}

/// Resolve the connector this claim owns, creating one only when the device is new
/// to this account.
async fn attempt_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: &str,
    name: &str,
    device_os: Option<&str>,
    agent_id: Option<&str>,
) -> ApiResult<Attempt> {
    if let Some(agent_id) = agent_id {
        if let Some(existing) =
            connectors::find_connector_by_agent(&mut **tx, user_id, agent_id).await?
        {
            let (connector, token) = rotate_and_refresh(tx, &existing, name, device_os).await?;
            return Ok(Attempt::Done(Box::new(Claimed {
                connector,
                token,
                reused: true,
            })));
        }
    }
    match connectors::insert_connector(
        &mut **tx,
        &ids::connector_id(),
        user_id,
        name,
        "cli",
        device_os,
        agent_id,
    )
    .await
    {
        Ok((connector, token)) => Ok(Attempt::Done(Box::new(Claimed {
            connector,
            token,
            reused: false,
        }))),
        Err(error) if agent_taken(&error) => Ok(Attempt::AgentTaken),
        Err(error) => Err(error.into()),
    }
}

/// A device that already has a connector keeps it: the token is rotated (the old
/// one stops working the moment this commits, which is what makes a leaked
/// credential useless) and the row restates the name/platform it now reports.
async fn rotate_and_refresh(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    connector: &ConnectorRow,
    name: &str,
    device_os: Option<&str>,
) -> ApiResult<(ConnectorRow, String)> {
    let (token, _prefix) = connectors::rotate_token(&mut **tx, &connector.id).await?;
    let connector = connectors::refresh_device(&mut **tx, &connector.id, name, device_os).await?;
    Ok((connector, token))
}

/// The one-time switch, still conditional on `claimed_at is null`: a second claim
/// of the same code loses here and rolls back whatever it did to a connector.
async fn mark_claimed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    pairing_id: &str,
    connector_id: &str,
    source: &str,
) -> ApiResult<()> {
    let claimed = sqlx::query(
        "update pairings
            set claimed_at = $2, connector_id = $3, attempt_ip = $4
          where id = $1 and claimed_at is null",
    )
    .bind(pairing_id)
    .bind(timeutil::utc_now())
    .bind(connector_id)
    .bind(source)
    .execute(&mut **tx)
    .await?;
    if claimed.rows_affected() == 0 {
        return Err(already_claimed());
    }
    Ok(())
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
