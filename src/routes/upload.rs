use axum::{extract::{Multipart, State}, http::StatusCode, Json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use std::collections::HashMap;

use crate::{
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
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<GifResponse>), AppError> {
    let mut gif_data: Option<(String, Vec<u8>)> = None;
    let mut tags: Vec<String> = Vec::new();

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
            _ => {}
        }
    }

    let (original_filename, data) = gif_data
        .ok_or_else(|| AppError::BadRequest("missing file field".into()))?;

    // Hash for deduplication
    let hash = hex::encode(Sha256::digest(&data));

    // Check for duplicate
    if let Some(existing) = db::get_gif_by_hash(&state.pool, &hash).await? {
        let renditions = build_renditions(&state, &existing.id).await?;
        let tags = db::get_tags(&state.pool, &existing.id).await?;
        return Ok((StatusCode::OK, Json(to_response(existing, tags, renditions, &state.config.base_url))));
    }

    // Parse GIF metadata
    let info = parse_gif_info(&data)?;

    let id = Uuid::new_v4().to_string();
    let ext = if original_filename.ends_with(".gif") { "gif" } else { "gif" };
    let original_filename_stored = format!("{}.{}", id, ext);

    // Generate renditions
    let preview_data  = resize_gif(&data, 220)?;
    let (thumb_data, thumb_w, thumb_h) = extract_thumbnail(&data, 220)?;

    let preview_w = if info.width <= 220 { info.width } else { 220 };
    let preview_h = if info.width <= 220 {
        info.height
    } else {
        (info.height as f32 * (220.0 / info.width as f32)) as u32
    };

    // Store files
    files::write_file(&state.config.storage_path, &original_filename_stored, &data).await?;
    let preview_filename  = format!("{}_preview.gif",  id);
    let thumbnail_filename = format!("{}_thumb.png", id);
    files::write_file(&state.config.storage_path, &preview_filename,   &preview_data).await?;
    files::write_file(&state.config.storage_path, &thumbnail_filename, &thumb_data).await?;

    // Persist to DB
    let gif = db::insert_gif(
        &state.pool, &id, &original_filename,
        &hash, info.frame_count, info.duration_ms,
    ).await?;

    db::insert_rendition(&state.pool, &id, "original",  &original_filename_stored,
        info.width as i32, info.height as i32, data.len() as i32).await?;
    db::insert_rendition(&state.pool, &id, "preview",   &preview_filename,
        preview_w as i32, preview_h as i32, preview_data.len() as i32).await?;
    db::insert_rendition(&state.pool, &id, "thumbnail", &thumbnail_filename,
        thumb_w as i32, thumb_h as i32, thumb_data.len() as i32).await?;

    if !tags.is_empty() {
        db::set_tags(&state.pool, &id, &tags).await?;
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
        tags,
        frame_count: gif.frame_count,
        duration_ms: gif.duration_ms,
        uses:        gif.uses,
        uploaded_at: gif.uploaded_at,
        renditions,
    }
}
