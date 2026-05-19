pub mod auth;
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
use tower_http::trace::TraceLayer;

use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool:   sqlx::PgPool,
    pub config: Arc<Config>,
    pub http:   reqwest::Client,
}

/// Build the application router. Rate limiting is not included here so tests
/// are not throttled — main.rs wraps this with GovernorLayer for production.
pub fn create_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/gifs",                post(routes::upload::upload))
        .route("/gifs/{id}",           get(routes::fetch::get_gif)
                                       .patch(routes::fetch::patch_gif)
                                       .delete(routes::fetch::delete_gif))
        .route("/gifs/{id}/file",      get(routes::fetch::serve_file))
        .route("/gifs/{id}/select",    post(routes::fetch::select_gif))
        .route("/gifs/{id}/favorite",  put(routes::favorites::add_favorite)
                                       .delete(routes::favorites::remove_favorite))
        .route("/gifs/{id}/tags",      put(routes::tags::put_tags)
                                       .patch(routes::tags::patch_tags)
                                       .delete(routes::tags::delete_tags))
        .route("/gifs/search",         get(routes::search::search))
        .route("/gifs/featured",       get(routes::search::featured))
        .route("/gifs/recent",         get(routes::search::recent))
        .route("/gifs/favorites",      get(routes::favorites::list_favorites))
        .route("/gifs/history",        get(routes::favorites::list_history))
        .route("/gifs/tags/suggest",   get(routes::search::suggest))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_session,
        ));

    let public = Router::new()
        .route("/health", get(routes::health::health))
        .route("/auth/matrix", post(routes::auth::matrix_login));

    protected
        .merge(public)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
