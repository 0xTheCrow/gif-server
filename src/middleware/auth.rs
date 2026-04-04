use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use serde_json::json;
use std::time::{Duration, Instant};

use crate::{AppState, KeyCache};

const CACHE_TTL: Duration = Duration::from_secs(30);

pub async fn require_api_key(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let key = req
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let Some(key) = key else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized", "message": "missing X-API-Key header" })),
        ).into_response();
    };

    let hashes = get_hashes(&state).await;
    let hashes = match hashes {
        Ok(h) => h,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error", "message": "an internal error occurred" })),
            ).into_response();
        }
    };

    let argon2 = Argon2::default();
    let valid = hashes.iter().any(|hash| {
        PasswordHash::new(hash)
            .map(|parsed| argon2.verify_password(key.as_bytes(), &parsed).is_ok())
            .unwrap_or(false)
    });

    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized", "message": "invalid API key" })),
        ).into_response();
    }

    next.run(req).await
}

async fn get_hashes(state: &AppState) -> Result<Vec<String>, sqlx::Error> {
    // Try cache first
    {
        let cache = state.key_cache.read().await;
        if let Some(ref c) = *cache {
            if c.loaded_at.elapsed() < CACHE_TTL {
                return Ok(c.hashes.clone());
            }
        }
    }

    // Cache miss or expired — fetch from DB
    let rows: Vec<(String,)> = sqlx::query_as("SELECT key_hash FROM api_keys")
        .fetch_all(&state.pool)
        .await?;

    let hashes: Vec<String> = rows.into_iter().map(|(h,)| h).collect();

    let mut cache = state.key_cache.write().await;
    *cache = Some(KeyCache { hashes: hashes.clone(), loaded_at: Instant::now() });

    Ok(hashes)
}
