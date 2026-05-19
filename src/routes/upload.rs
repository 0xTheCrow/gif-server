use axum::{extract::{Multipart, State}, http::StatusCode, Extension, Json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    auth::AuthUser,
    error::AppError,
    models::{GifResponse, RenditionInfo},
    storage::{
        db, files,
        parse_gif_info, resize_gif, extract_thumbnail,
    },
    AppState,
};

pub async fn upload(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<GifResponse>), AppError> {
    let mut gif_data: Option<(String, Vec<u8>)> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut visibility = String::from("shared");
    let mut is_nsfw = false;

    while let Some(field) = multipart.next_field().await
        .map_err(|e| AppError::BadRequest(e.to_string()))? {
        match field.name() {
            Some("file") => {
                let filename = field.file_name()
                    .unwrap_or("upload.gif")
                    .to_string();
                let data = field.bytes().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?
                    .to_vec();
                gif_data = Some((filename, data));
            }
            Some("tags") => {
                let text = field.text().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                tags = text.split(',')
                    .map(|t| t.trim().to_lowercase())
                    .filter(|t| !t.is_empty())
                    .take(20)
                    .map(|t| t.chars().take(100).collect::<String>())
                    .collect();
            }
            Some("visibility") => {
                visibility = field.text().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?
                    .trim()
                    .to_lowercase();
            }
            Some("nsfw") => {
                let v = field.text().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?
                    .trim()
                    .to_lowercase();
                is_nsfw = matches!(v.as_str(), "true" | "1" | "yes");
            }
            _ => {}
        }
    }

    if visibility != "shared" && visibility != "private" {
        return Err(AppError::BadRequest(
            "visibility must be 'shared' or 'private'".into(),
        ));
    }

    let (original_filename, data) = gif_data
        .ok_or_else(|| AppError::BadRequest("missing file field".into()))?;

    // Hash for deduplication
    let hash = hex::encode(Sha256::digest(&data));

    // Per-uploader dedup: a hit is always the requester's own earlier upload,
    // so there's nothing to authorize and nothing leaked about other users.
    if let Some(existing) =
        db::get_gif_by_hash_for_uploader(&state.pool, &hash, &user.mxid).await?
    {
        let renditions = build_renditions(&state, &existing.id).await?;
        let tags = db::get_tags(&state.pool, &existing.id).await?;
        return Ok((StatusCode::OK, Json(to_response(existing, tags, renditions, &state.config.base_url))));
    }

    // Decode + resize is CPU-bound and unbounded by request concurrency, so
    // run it off the async runtime to avoid stalling other requests.
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

    let id = Uuid::new_v4().to_string();
    let original_filename_stored = format!("{}.gif", id);

    let preview_w = if info.width <= 220 { info.width } else { 220 };
    let preview_h = if info.width <= 220 {
        info.height
    } else {
        (info.height as f32 * (220.0 / info.width as f32)) as u32
    };

    let preview_filename   = format!("{}_preview.gif", id);
    let thumbnail_filename  = format!("{}_thumb.png", id);

    let specs = vec![
        db::RenditionSpec {
            rendition: "original",
            filename: original_filename_stored.clone(),
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

    // Cap check + insert is atomic (advisory-locked tx). Nothing is written
    // to disk until the row is committed, so a rejected upload leaves no
    // orphaned files.
    let gif = match db::insert_gif_quota(
        &state.pool, &id, &original_filename, &hash, &user.mxid,
        &visibility, is_nsfw, info.frame_count, info.duration_ms,
        &specs, &tags,
        state.config.storage_max_bytes, state.config.per_user_storage_bytes,
    ).await? {
        db::UploadOutcome::QuotaExceeded => return Err(AppError::InsufficientStorage),
        db::UploadOutcome::Duplicate(existing) => {
            let renditions = build_renditions(&state, &existing.id).await?;
            let tags = db::get_tags(&state.pool, &existing.id).await?;
            return Ok((StatusCode::OK, Json(to_response(existing, tags, renditions, &state.config.base_url))));
        }
        db::UploadOutcome::Inserted(gif) => gif,
    };

    // Row is committed; write the files. If any write fails, roll the row
    // back and remove whatever landed so we don't leave a half-stored GIF.
    let storage = &state.config.storage_path;
    let write_all = async {
        files::write_file(storage, &original_filename_stored, data.as_slice()).await?;
        files::write_file(storage, &preview_filename, &preview_data).await?;
        files::write_file(storage, &thumbnail_filename, &thumb_data).await?;
        Ok::<(), AppError>(())
    };
    if let Err(e) = write_all.await {
        let _ = db::delete_gif(&state.pool, &id).await;
        for f in [&original_filename_stored, &preview_filename, &thumbnail_filename] {
            let _ = files::delete_file(storage, f).await;
        }
        return Err(e);
    }

    let renditions = build_renditions(&state, &id).await?;
    Ok((StatusCode::CREATED, Json(to_response(gif, tags, renditions, &state.config.base_url))))
}

pub async fn build_renditions_pub(state: &AppState, gif_id: &str) -> Result<HashMap<String, RenditionInfo>, AppError> {
    build_renditions(state, gif_id).await
}

async fn build_renditions(state: &AppState, gif_id: &str) -> Result<HashMap<String, RenditionInfo>, AppError> {
    let rows = db::get_renditions(&state.pool, gif_id).await?;
    Ok(rows.into_iter().map(|r| {
        let url = rendition_url(&state.config.base_url, gif_id, &r.rendition);
        (r.rendition, RenditionInfo { url, width: r.width, height: r.height, size_bytes: r.size_bytes })
    }).collect())
}

fn rendition_url(base_url: &str, gif_id: &str, rendition: &str) -> String {
    if rendition == "original" {
        format!("{}/gifs/{}/file", base_url, gif_id)
    } else {
        format!("{}/gifs/{}/file?rendition={}", base_url, gif_id, rendition)
    }
}

pub fn to_response(gif: crate::models::Gif, tags: Vec<String>, renditions: HashMap<String, RenditionInfo>, _host: &str) -> GifResponse {
    GifResponse {
        id:          gif.id,
        filename:    gif.filename,
        uploader_id: gif.uploader_id,
        visibility:  gif.visibility,
        is_nsfw:     gif.is_nsfw,
        tags,
        frame_count: gif.frame_count,
        duration_ms: gif.duration_ms,
        uses:        gif.uses,
        uploaded_at: gif.uploaded_at,
        renditions,
    }
}
