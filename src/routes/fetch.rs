use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    Extension,
};
use serde::Deserialize;
use crate::{
    auth::AuthUser,
    error::AppError,
    models::{Gif, GifPatch, GifResponse},
    storage::{db, files},
    routes::upload::{to_response, build_renditions_pub},
    AppState,
};

#[derive(Deserialize)]
pub struct RenditionQuery {
    pub rendition: Option<String>,
}

/// Load a GIF the user is allowed to see. A GIF the user cannot view is
/// reported as not found so its existence isn't disclosed.
async fn load_viewable(state: &AppState, id: &str, user: &AuthUser) -> Result<Gif, AppError> {
    let gif = db::get_gif(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    if !user.can_view(&gif) {
        return Err(AppError::NotFound);
    }
    Ok(gif)
}

pub async fn get_gif(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<GifResponse>, AppError> {
    let gif = load_viewable(&state, &id, &user).await?;
    let tags = db::get_tags(&state.pool, &id).await?;
    let renditions = build_renditions_pub(&state, &id).await?;
    Ok(Json(to_response(gif, tags, renditions, &state.config.base_url)))
}

pub async fn serve_file(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(params): Query<RenditionQuery>,
) -> Result<Response, AppError> {
    load_viewable(&state, &id, &user).await?;

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
            (header::CACHE_CONTROL, "private, max-age=31536000"),
            (header::CONTENT_LENGTH, &data.len().to_string()),
        ],
        Body::from(data),
    ).into_response())
}

pub async fn delete_gif(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let gif = load_viewable(&state, &id, &user).await?;
    if !user.can_mutate(&gif) {
        return Err(AppError::Forbidden);
    }

    let renditions = db::get_renditions(&state.pool, &id).await?;
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

pub async fn patch_gif(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(body): Json<GifPatch>,
) -> Result<StatusCode, AppError> {
    if body.visibility.is_none() && body.is_nsfw.is_none() {
        return Err(AppError::BadRequest(
            "provide at least one of: visibility, is_nsfw".into(),
        ));
    }

    let visibility = match body.visibility {
        Some(v) => {
            let v = v.trim().to_lowercase();
            if v != "shared" && v != "private" {
                return Err(AppError::BadRequest(
                    "visibility must be 'shared' or 'private'".into(),
                ));
            }
            Some(v)
        }
        None => None,
    };

    let gif = load_viewable(&state, &id, &user).await?;
    // Uploader, or an admin on a shared GIF (admins never reach other
    // users' private GIFs — load_viewable already 404s those).
    if !user.can_mutate(&gif) {
        return Err(AppError::Forbidden);
    }

    if let Some(v) = visibility {
        db::set_visibility(&state.pool, &id, &v).await?;
    }
    if let Some(nsfw) = body.is_nsfw {
        db::set_nsfw(&state.pool, &id, nsfw).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn select_gif(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    load_viewable(&state, &id, &user).await?;
    db::increment_uses(&state.pool, &id).await?;
    db::record_selection(&state.pool, &user.mxid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
