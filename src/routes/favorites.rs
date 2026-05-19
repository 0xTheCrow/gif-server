use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};

use crate::{
    auth::AuthUser,
    error::AppError,
    models::{ListResponse, PaginationParams},
    routes::search::{decode_offset, encode_offset, gif_to_response, DEFAULT_LIMIT, MAX_LIMIT},
    storage::db,
    AppState,
};

/// Confirm the GIF exists and the user may see it (shared or their own).
async fn require_viewable(state: &AppState, id: &str, user: &AuthUser) -> Result<(), AppError> {
    let gif = db::get_gif(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    if !user.can_view(&gif) {
        return Err(AppError::NotFound);
    }
    Ok(())
}

pub async fn add_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_viewable(&state, &id, &user).await?;
    db::add_favorite(&state.pool, &user.mxid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    db::remove_favorite(&state.pool, &user.mxid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list(
    state: &AppState,
    user: &AuthUser,
    params: &PaginationParams,
    kind: ListKind,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());

    let gifs = match kind {
        ListKind::Favorites => {
            db::list_favorites(&state.pool, &user.mxid, params.grab_nsfw, limit, offset).await?
        }
        ListKind::History => {
            db::list_history(&state.pool, &user.mxid, params.grab_nsfw, limit, offset).await?
        }
    };

    let next = if gifs.len() as i64 == limit {
        Some(encode_offset(offset + limit))
    } else {
        None
    };

    let mut results = Vec::with_capacity(gifs.len());
    for gif in gifs {
        results.push(gif_to_response(state, gif).await?);
    }
    Ok(Json(ListResponse { results, next }))
}

enum ListKind {
    Favorites,
    History,
}

pub async fn list_favorites(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    list(&state, &user, &params, ListKind::Favorites).await
}

pub async fn list_history(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    list(&state, &user, &params, ListKind::History).await
}
