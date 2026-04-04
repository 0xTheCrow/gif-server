use axum::{extract::{Path, State}, http::StatusCode, Json};

use crate::{error::AppError, models::TagsBody, storage::db, AppState};

pub async fn put_tags(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    db::get_gif(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::set_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_tags(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    db::get_gif(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::add_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_tags(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, AppError> {
    db::get_gif(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    let tags: Vec<String> = body.tags.into_iter().map(|t| t.to_lowercase()).collect();
    db::remove_tags(&state.pool, &id, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}
