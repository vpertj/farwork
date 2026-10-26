//! Bearer-token extractor. Matches upstream `current_user_id`: the token alone
//! identifies the caller, disabled/deleted accounts are rejected by handlers
//! that actually load the user.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use crate::error::ApiError;
use crate::state::AppState;

pub struct AuthedUser(pub String);

impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        let user_id = crate::auth::user_id_from_authorization(&state.config().secret, header)?;
        Ok(AuthedUser(user_id))
    }
}
