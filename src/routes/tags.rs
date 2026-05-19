use axum::{extract::{Path, State}, http::StatusCode, Extension, Json};

use crate::{auth::AuthUser, error::AppError, models::TagsBody, storage::db, AppState};

/// Load a GIF and confirm the user may modify it (uploader, or admin on a
/// shared GIF). Hidden GIFs are reported as not found.
async fn load_mutable(
    state: &AppState,
    id: &str,
    user: &AuthUser,
) -> Result<(), AppError> {
    let gif = db::get_gif(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    if !user.can_view(&gif) {
        return Err(AppError::NotFound);
    }
    if !user.can_mutate(&gif) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

pub async fn put_tags(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    load_mutable(&state, &id, &user).await?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::set_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_tags(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    load_mutable(&state, &id, &user).await?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::add_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_tags(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    load_mutable(&state, &id, &user).await?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::remove_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}
