pub mod auth;
pub mod config;
pub mod error;
pub mod middleware;
pub mod models;
pub mod routes;
pub mod storage;

use axum::{
    extract::DefaultBodyLimit,
    http::Request,
    middleware as axum_middleware,
    routing::{get, post, put},
    Router,
};
use std::sync::Arc;
use tower_governor::{
    governor::GovernorConfigBuilder,
    key_extractor::{GlobalKeyExtractor, KeyExtractor},
    GovernorError, GovernorLayer,
};
use tower_http::trace::TraceLayer;

use auth::AuthUser;
use config::Config;

/// Rate-limit key = the authenticated Matrix user ID. Effective behind a
/// reverse proxy (unlike peer-IP keying). Only used on routes that run
/// after the session middleware, so `AuthUser` is always present.
#[derive(Clone)]
struct MxidKeyExtractor;

impl KeyExtractor for MxidKeyExtractor {
    type Key = String;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        req.extensions()
            .get::<AuthUser>()
            .map(|u| u.mxid.clone())
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool:   sqlx::PgPool,
    pub config: Arc<Config>,
    pub http:   reqwest::Client,
}

/// Build the application router, including rate limiting. Protected routes
/// are limited per authenticated user; `/auth/matrix` has a strict global
/// limit (it's unauthenticated and triggers an outbound homeserver call).
/// Limits are generous enough not to throttle the test suite.
pub fn create_router(state: AppState) -> Router {
    let per_user_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(MxidKeyExtractor)
            .per_second(30)
            .burst_size(60)
            .finish()
            .expect("invalid per-user rate-limit config"),
    );
    let auth_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(GlobalKeyExtractor)
            .per_second(1)
            .burst_size(10)
            .finish()
            .expect("invalid auth rate-limit config"),
    );

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
        // Governor is added before the session layer so it ends up *inside*
        // it: the session middleware runs first and populates `AuthUser`,
        // then the per-user limiter reads it.
        .layer(GovernorLayer::new(per_user_rate_limit))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_session,
        ));

    let public = Router::new()
        .route("/health", get(routes::health::health))
        .route(
            "/auth/matrix",
            post(routes::auth::matrix_login).layer(GovernorLayer::new(auth_rate_limit)),
        );

    protected
        .merge(public)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
