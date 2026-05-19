use axum::{extract::State, http::StatusCode};

use crate::AppState;

/// Unauthenticated liveness/readiness probe. Returns 200 only if the
/// database is reachable, 503 otherwise.
pub async fn health(State(state): State<AppState>) -> StatusCode {
    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
