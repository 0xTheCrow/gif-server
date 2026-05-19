use axum::{extract::State, Json};
use serde::Serialize;

use crate::{
    auth::{mint_session, verify_openid_token, OpenIdToken},
    error::AppError,
    AppState,
};

#[derive(Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub mxid: String,
    pub expires_in: i64,
}

/// Exchange a Matrix OpenID token for a gif-server session token.
/// Unauthenticated by design — this is how a client authenticates.
pub async fn matrix_login(
    State(state): State<AppState>,
    Json(token): Json<OpenIdToken>,
) -> Result<Json<SessionResponse>, AppError> {
    let mxid = verify_openid_token(&state.config, &state.http, &token).await?;
    let (session, expires_in) = mint_session(&state.config, &mxid)?;
    Ok(Json(SessionResponse {
        token: session,
        mxid,
        expires_in,
    }))
}
