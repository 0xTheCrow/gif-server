use axum::{extract::{Query, State}, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use crate::{
    error::AppError,
    models::{GifResponse, ListResponse, PaginationParams, SearchParams, SuggestParams, SuggestResponse},
    storage::db,
    routes::upload::{to_response, build_renditions_pub},
    AppState,
};

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;

fn decode_offset(pos: Option<&str>) -> i64 {
    pos.and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn encode_offset(offset: i64) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

async fn gif_to_response(state: &AppState, gif: crate::models::Gif) -> Result<GifResponse, AppError> {
    let tags = db::get_tags(&state.pool, &gif.id).await?;
    let renditions = build_renditions_pub(state, &gif.id).await?;
    Ok(to_response(gif, tags, renditions, &state.config.base_url))
}

pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());

    let gifs = match params.q.as_deref().filter(|q| !q.is_empty()) {
        Some(q) => {
            let terms: Vec<String> = q.split_whitespace()
                .map(|t| t.to_lowercase())
                .collect();
            let mut results = db::search_by_tags(&state.pool, &terms, limit, offset).await?;
            if (results.len() as i64) < limit {
                let seen: std::collections::HashSet<String> = results.iter().map(|g| g.id.clone()).collect();
                let fts = db::search_by_filename(&state.pool, q, limit, offset).await?;
                for g in fts {
                    if !seen.contains(&g.id) {
                        results.push(g);
                    }
                }
                results.truncate(limit as usize);
            }
            results
        }
        None => db::list_recent(&state.pool, limit, offset).await?,
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
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());

    let gifs = db::list_featured(&state.pool, limit, offset).await?;
    let next = if gifs.len() as i64 == limit { Some(encode_offset(offset + limit)) } else { None };

    let mut results = Vec::with_capacity(gifs.len());
    for gif in gifs {
        results.push(gif_to_response(&state, gif).await?);
    }

    Ok(Json(ListResponse { results, next }))
}

pub async fn recent(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse>, AppError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = decode_offset(params.pos.as_deref());

    let gifs = db::list_recent(&state.pool, limit, offset).await?;
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
    let suggestions = db::suggest_tags(&state.pool, &params.q.to_lowercase(), limit).await?;
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
}
