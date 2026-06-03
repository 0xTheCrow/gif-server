use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    Extension,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use crate::{
    auth::AuthUser,
    error::AppError,
    models::{Gif, GifPatch, GifResponse},
    storage::{db, files, parse_gif_info, resize_gif, extract_thumbnail},
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
    let renditions = build_renditions_pub(&state, &id, &gif.hash).await?;
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

/// Replace an existing GIF's file with a newly uploaded one. The id, tags,
/// visibility, favorites, and use count are preserved; only the bytes and the
/// content-derived metadata (hash, dimensions, frame count, duration) change.
/// Authorized like the other mutations: the uploader, or an admin on a shared
/// GIF (load_viewable 404s other users' private GIFs first).
pub async fn replace_file(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<GifResponse>, AppError> {
    let gif = load_viewable(&state, &id, &user).await?;
    if !user.can_mutate(&gif) {
        return Err(AppError::Forbidden);
    }
    let uploader_id = gif.uploader_id.clone();

    let mut upload: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await
        .map_err(|e| AppError::BadRequest(e.to_string()))? {
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or("upload.gif").to_string();
            let data = field.bytes().await
                .map_err(|e| AppError::BadRequest(e.to_string()))?
                .to_vec();
            upload = Some((filename, data));
        }
    }
    let (filename, data) = upload
        .ok_or_else(|| AppError::BadRequest("missing file field".into()))?;

    let hash = hex::encode(Sha256::digest(&data));

    // Decode + resize is CPU-bound; keep it off the async runtime.
    let data = Arc::new(data);
    let proc = Arc::clone(&data);
    let (info, preview_data, thumb_data, thumb_w, thumb_h) =
        tokio::task::spawn_blocking(move || -> Result<_, AppError> {
            let info = parse_gif_info(&proc)?;
            let preview = resize_gif(&proc, 220)?;
            let (thumb, tw, th) = extract_thumbnail(&proc, 220)?;
            Ok((info, preview, thumb, tw, th))
        })
        .await
        .map_err(|e| AppError::Internal(format!("image processing task failed: {e}")))??;

    // Overwrite the row's existing rendition files in place, so all references
    // stay valid. The filenames are whatever the row already stores.
    let rows = db::get_renditions(&state.pool, &id).await?;
    let filename_for = |rendition: &str| -> Result<String, AppError> {
        rows.iter()
            .find(|r| r.rendition == rendition)
            .map(|r| r.filename.clone())
            .ok_or(AppError::NotFound)
    };
    let original_filename = filename_for("original")?;
    let preview_filename = filename_for("preview")?;
    let thumbnail_filename = filename_for("thumbnail")?;

    let preview_w = if info.width <= 220 { info.width } else { 220 };
    let preview_h = if info.width <= 220 {
        info.height
    } else {
        (info.height as f32 * (220.0 / info.width as f32)) as u32
    };

    let specs = vec![
        db::RenditionSpec {
            rendition: "original",
            filename: original_filename.clone(),
            width: info.width as i32,
            height: info.height as i32,
            size_bytes: data.len() as i32,
        },
        db::RenditionSpec {
            rendition: "preview",
            filename: preview_filename.clone(),
            width: preview_w as i32,
            height: preview_h as i32,
            size_bytes: preview_data.len() as i32,
        },
        db::RenditionSpec {
            rendition: "thumbnail",
            filename: thumbnail_filename.clone(),
            width: thumb_w as i32,
            height: thumb_h as i32,
            size_bytes: thumb_data.len() as i32,
        },
    ];

    let gif = match db::replace_gif_quota(
        &state.pool, &id, &uploader_id, &filename, &hash,
        info.frame_count, info.duration_ms, &specs,
        state.config.storage_max_bytes, state.config.per_user_storage_bytes,
    ).await? {
        db::ReplaceOutcome::QuotaExceeded => return Err(AppError::InsufficientStorage),
        db::ReplaceOutcome::Duplicate => {
            return Err(AppError::Conflict("you already have a GIF with this content".into()))
        }
        db::ReplaceOutcome::Updated(gif) => gif,
    };

    // Row is committed; overwrite the files atomically.
    let storage = &state.config.storage_path;
    files::write_file_atomic(storage, &original_filename, data.as_slice()).await?;
    files::write_file_atomic(storage, &preview_filename, &preview_data).await?;
    files::write_file_atomic(storage, &thumbnail_filename, &thumb_data).await?;

    let tags = db::get_tags(&state.pool, &id).await?;
    let renditions = build_renditions_pub(&state, &id, &gif.hash).await?;
    Ok(Json(to_response(gif, tags, renditions, &state.config.base_url)))
}

pub async fn select_gif(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    load_viewable(&state, &id, &user).await?;
    if db::record_selection(&state.pool, &user.mxid, &id).await? {
        db::increment_uses(&state.pool, &id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
