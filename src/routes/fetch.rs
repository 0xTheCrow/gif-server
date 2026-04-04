use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use crate::{
    error::AppError,
    models::GifResponse,
    storage::{db, files},
    routes::upload::{to_response, build_renditions_pub},
    AppState,
};

#[derive(Deserialize)]
pub struct RenditionQuery {
    pub rendition: Option<String>,
}

pub async fn get_gif(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<GifResponse>, AppError> {
    let gif = db::get_gif(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    let tags = db::get_tags(&state.pool, &id).await?;
    let renditions = build_renditions_pub(&state, &id).await?;
    Ok(Json(to_response(gif, tags, renditions, &state.config.base_url)))
}

pub async fn serve_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<RenditionQuery>,
) -> Result<Response, AppError> {
    let rendition = params.rendition.as_deref().unwrap_or("original");

    let rows = db::get_renditions(&state.pool, &id).await?;
    if rows.is_empty() {
        return Err(AppError::NotFound);
    }

    let row = rows.iter()
        .find(|r| r.rendition == rendition)
        .ok_or(AppError::NotFound)?;

    let data = files::read_file(&state.config.storage_path, &row.filename).await?;

    let content_type = if rendition == "thumbnail" {
        "image/png"
    } else {
        "image/gif"
    };

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000"),
            (header::CONTENT_LENGTH, &data.len().to_string()),
        ],
        Body::from(data),
    ).into_response())
}

pub async fn delete_gif(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let renditions = db::get_renditions(&state.pool, &id).await?;
    if renditions.is_empty() {
        return Err(AppError::NotFound);
    }

    let deleted = db::delete_gif(&state.pool, &id).await?;
    if !deleted {
        return Err(AppError::NotFound);
    }

    for r in renditions {
        if let Err(e) = files::delete_file(&state.config.storage_path, &r.filename).await {
            tracing::error!("failed to delete file {}: {}", r.filename, e);
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

pub async fn select_gif(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    db::get_gif(&state.pool, &id).await?.ok_or(AppError::NotFound)?;
    db::increment_uses(&state.pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
