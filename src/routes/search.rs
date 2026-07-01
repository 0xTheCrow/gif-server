use axum::{extract::{Query, State}, Extension, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use crate::{
    auth::AuthUser,
    error::AppError,
    models::{GifResponse, ListResponse, PaginationParams, SearchParams, SuggestParams, SuggestResponse},
    storage::db,
    routes::upload::{to_response, build_renditions_pub},
    AppState,
};

pub(crate) const DEFAULT_LIMIT: i64 = 20;
pub(crate) const MAX_LIMIT: i64 = 50;

pub(crate) fn decode_offset(pos: Option<&str>) -> i64 {
    pos.and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

pub(crate) fn encode_offset(offset: i64) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

/// Random-mode cursor: carries the shuffle seed alongside the offset (encoded
/// `seed:offset`) so the seeded ordering stays stable as the client pages. The
/// client treats this as an opaque token, echoing it back verbatim as `pos`.
pub(crate) fn encode_seeded_offset(seed: i64, offset: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{seed}:{offset}"))
}

/// Decodes a seeded cursor from `encode_seeded_offset`. Returns `None` for an
/// absent or non-seeded (bare offset) cursor, so the handler mints a fresh seed
/// for the first random page.
pub(crate) fn decode_seeded_offset(pos: Option<&str>) -> Option<(i64, i64)> {
    let raw = pos
        .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        .and_then(|b| String::from_utf8(b).ok())?;
    let (seed, offset) = raw.split_once(':')?;
    Some((seed.parse().ok()?, offset.parse().ok()?))
}

/// A fresh random seed for the first page of a random-ordered feed.
fn fresh_seed() -> i64 {
    i64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap())
}

pub(crate) async fn gif_to_response(state: &AppState, gif: crate::models::Gif) -> Result<GifResponse, AppError> {
    let tags = db::get_tags(&state.pool, &gif.id).await?;
    let renditions = build_renditions_pub(state, &gif.id, &gif.hash).await?;
    Ok(to_response(gif, tags, renditions, &state.config.base_url))
}

pub async fn search(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<SearchParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());
    let viewer = user.mxid.as_str();

    let gifs = match params.q.as_deref().filter(|q| !q.is_empty()) {
        Some(q) => {
            let terms: Vec<String> = q.split_whitespace()
                .map(|t| t.to_lowercase())
                .collect();
            let mut results = db::search_by_tags(&state.pool, &terms, viewer, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset).await?;
            if (results.len() as i64) < limit {
                let seen: std::collections::HashSet<String> = results.iter().map(|g| g.id.clone()).collect();
                let fts = db::search_by_filename(&state.pool, q, viewer, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset).await?;
                for g in fts {
                    if !seen.contains(&g.id) {
                        results.push(g);
                    }
                }
                results.truncate(limit as usize);
            }
            results
        }
        None => db::list_recent(&state.pool, viewer, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset).await?,
    };

    let next = if gifs.len() as i64 == limit {
        Some(encode_offset(offset + limit))
    } else {
        None
    };

    let mut results = Vec::with_capacity(gifs.len());
    for gif in gifs {
        results.push(gif_to_response(&state, gif).await?);
    }

    Ok(Json(ListResponse { results, next }))
}

pub async fn featured(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);

    let (gifs, next) = if params.random == Some(true) {
        // Recover the seed from the cursor so the shuffle stays stable across
        // pages; the first page (no seeded cursor) mints a fresh seed.
        let (seed, offset) = decode_seeded_offset(params.pos.as_deref())
            .unwrap_or_else(|| (fresh_seed(), 0));
        let gifs = db::list_featured_random(&state.pool, &user.mxid, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset, seed).await?;
        let next = if gifs.len() as i64 == limit { Some(encode_seeded_offset(seed, offset + limit)) } else { None };
        (gifs, next)
    } else {
        let offset = decode_offset(params.pos.as_deref());
        let gifs = db::list_featured(&state.pool, &user.mxid, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset).await?;
        let next = if gifs.len() as i64 == limit { Some(encode_offset(offset + limit)) } else { None };
        (gifs, next)
    };

    let mut results = Vec::with_capacity(gifs.len());
    for gif in gifs {
        results.push(gif_to_response(&state, gif).await?);
    }

    Ok(Json(ListResponse { results, next }))
}

pub async fn recent(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());

    let gifs = db::list_recent(&state.pool, &user.mxid, params.mine, params.grab_nsfw, params.grab_hidden, limit, offset).await?;
    let next = if gifs.len() as i64 == limit { Some(encode_offset(offset + limit)) } else { None };

    let mut results = Vec::with_capacity(gifs.len());
    for gif in gifs {
        results.push(gif_to_response(&state, gif).await?);
    }

    Ok(Json(ListResponse { results, next }))
}

pub async fn suggest(
    State(state): State<AppState>,
    Query(params): Query<SuggestParams>,
) -> Result<Json<SuggestResponse>, AppError> {
    let limit = params.limit.unwrap_or(10).min(20);
    let suggestions = db::suggest_tags(&state.pool, &params.q.to_lowercase(), params.grab_nsfw, limit).await?;
    Ok(Json(SuggestResponse { suggestions }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        let offset = 42i64;
        let encoded = encode_offset(offset);
        let decoded = decode_offset(Some(&encoded));
        assert_eq!(decoded, offset);
    }

    #[test]
    fn cursor_none_decodes_to_zero() {
        assert_eq!(decode_offset(None), 0);
    }

    #[test]
    fn cursor_garbage_decodes_to_zero() {
        assert_eq!(decode_offset(Some("not_valid_base64!!!")), 0);
    }

    #[test]
    fn seeded_cursor_roundtrip() {
        let encoded = encode_seeded_offset(-987654321, 40);
        assert_eq!(decode_seeded_offset(Some(&encoded)), Some((-987654321, 40)));
    }

    #[test]
    fn seeded_cursor_rejects_bare_offset() {
        // A non-random (bare offset) cursor must not parse as seeded, so the
        // handler mints a fresh seed for the first random page.
        let bare = encode_offset(20);
        assert_eq!(decode_seeded_offset(Some(&bare)), None);
        assert_eq!(decode_seeded_offset(None), None);
    }

    #[test]
    fn bare_decode_tolerates_seeded_cursor() {
        // The non-random path stays robust if handed a seeded cursor.
        let seeded = encode_seeded_offset(123, 60);
        assert_eq!(decode_offset(Some(&seeded)), 0);
    }
}
