pub mod config;
pub mod error;
pub mod middleware;
pub mod models;
pub mod routes;
pub mod storage;

use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{get, post, put},
    Router,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;

use config::Config;

pub struct KeyCache {
    pub hashes:    Vec<String>,
    pub loaded_at: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub pool:      sqlx::PgPool,
    pub config:    Arc<Config>,
    pub key_cache: Arc<RwLock<Option<KeyCache>>>,
}

/// Build the application router. Rate limiting is not included here so tests
/// are not throttled — main.rs wraps this with GovernorLayer for production.
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/gifs",                post(routes::upload::upload))
        .route("/gifs/{id}",           get(routes::fetch::get_gif).delete(routes::fetch::delete_gif))
        .route("/gifs/{id}/file",      get(routes::fetch::serve_file))
        .route("/gifs/{id}/select",    post(routes::fetch::select_gif))
        .route("/gifs/{id}/tags",      put(routes::tags::put_tags)
                                       .patch(routes::tags::patch_tags)
                                       .delete(routes::tags::delete_tags))
        .route("/gifs/search",         get(routes::search::search))
        .route("/gifs/featured",       get(routes::search::featured))
        .route("/gifs/recent",         get(routes::search::recent))
        .route("/gifs/tags/suggest",   get(routes::search::suggest))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_api_key,
        ))
        .with_state(state)
}
