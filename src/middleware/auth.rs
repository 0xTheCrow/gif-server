use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

use crate::{auth::verify_session, AppState};

/// Require a valid session token (minted by `POST /auth/matrix`) on the
/// `Authorization: Bearer` header, and inject the `AuthUser` into request
/// extensions for handlers to read.
pub async fn require_session(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);

    let Some(token) = token else {
        return unauthorized("missing Authorization: Bearer token");
    };

    let Some(user) = verify_session(&state.config, &token) else {
        return unauthorized("invalid or expired session token");
    };

    req.extensions_mut().insert(user);
    next.run(req).await
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": message })),
    )
        .into_response()
}
