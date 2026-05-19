use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Gif {
    pub id:          String,
    pub filename:    String,
    pub hash:        String,
    pub uploader_id: String,
    pub visibility:  String,
    pub is_nsfw:     bool,
    pub frame_count: i32,
    pub duration_ms: i32,
    pub uses:        i64,
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct GifRendition {
    pub gif_id:     String,
    pub rendition:  String,
    pub filename:   String,
    pub width:      i32,
    pub height:     i32,
    pub size_bytes: i32,
}

#[derive(Debug, Serialize)]
pub struct RenditionInfo {
    pub url:        String,
    pub width:      i32,
    pub height:     i32,
    pub size_bytes: i32,
}

#[derive(Debug, Serialize)]
pub struct GifResponse {
    pub id:          String,
    pub filename:    String,
    pub uploader_id: String,
    pub visibility:  String,
    pub is_nsfw:     bool,
    pub tags:        Vec<String>,
    pub frame_count: i32,
    pub duration_ms: i32,
    pub uses:        i64,
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: time::OffsetDateTime,
    pub renditions:  HashMap<String, RenditionInfo>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub results: Vec<GifResponse>,
    pub next:    Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q:     Option<String>,
    pub limit: Option<i64>,
    pub pos:   Option<String>,
    #[serde(default)]
    pub mine:  bool,
    /// When false (default) GIFs flagged NSFW are excluded.
    #[serde(default)]
    pub grab_nsfw: bool,
    /// When false (default) GIFs the viewer has hidden are excluded.
    #[serde(default)]
    pub grab_hidden: bool,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub limit: Option<i64>,
    pub pos:   Option<String>,
    #[serde(default)]
    pub mine:  bool,
    /// When false (default) GIFs flagged NSFW are excluded.
    #[serde(default)]
    pub grab_nsfw: bool,
    /// When false (default) GIFs the viewer has hidden are excluded.
    #[serde(default)]
    pub grab_hidden: bool,
}

#[derive(Debug, Deserialize)]
pub struct SuggestParams {
    pub q:     String,
    pub limit: Option<i64>,
    /// When false (default) tags only on NSFW GIFs are excluded.
    #[serde(default)]
    pub grab_nsfw: bool,
}

#[derive(Debug, Deserialize)]
pub struct TagsBody {
    pub tags: Vec<String>,
}

/// Body for `PATCH /gifs/:id`. Each field is optional; only the provided
/// ones are updated.
#[derive(Debug, Deserialize)]
pub struct GifPatch {
    pub visibility: Option<String>,
    pub is_nsfw:    Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct SuggestResponse {
    pub suggestions: Vec<String>,
}
