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
    key_extractor::{GlobalKeyExtractor, KeyExtractor, SmartIpKeyExtractor},
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
    // NOTE: tower_governor's `per_second(n)` sets the *replenish interval* to n
    // seconds (one token every n seconds), NOT n requests/second. To express a
    // sustained rate of R req/s, use `per_millisecond(1000 / R)`. `burst_size`
    // is the bucket capacity (max instantaneous burst).
    let per_user_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(MxidKeyExtractor)
            .per_millisecond(33) // ~30 req/s sustained
            .burst_size(60)
            .finish()
            .expect("invalid per-user rate-limit config"),
    );
    // Looser limit for file serving: one grid page can fan out to dozens of
    // `<img src>` fetches per user-initiated action, so we don't want browse
    // patterns competing for the same bucket as API/mutation calls.
    let file_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(MxidKeyExtractor)
            .per_millisecond(5) // 200 req/s sustained
            .burst_size(1000)
            .finish()
            .expect("invalid file rate-limit config"),
    );
    // Autocomplete fires per keystroke; even with UI debounce, a few quick
    // queries plus the resulting grid load shouldn't compete for the same
    // tokens as mutations.
    let suggest_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(MxidKeyExtractor)
            .per_millisecond(20) // 50 req/s sustained
            .burst_size(200)
            .finish()
            .expect("invalid suggest rate-limit config"),
    );
    // /auth/matrix is unauthenticated and triggers an outbound homeserver
    // call, so we layer two limits:
    //   * a global cap to bound total backend load
    //   * a per-IP cap so one attacker can't drain the global bucket and
    //     deny login to everyone else
    // Per-IP rate (1/s) is strictly less than global rate (5/s), so a single
    // attacker leaves headroom for legitimate users.
    // SmartIpKeyExtractor reads X-Forwarded-For / X-Real-IP / Forwarded;
    // **only safe when the app is reached exclusively through a trusted
    // reverse proxy that strips/sets these headers** — direct exposure would
    // let a client spoof their key and bypass the per-IP limit.
    let auth_global_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(GlobalKeyExtractor)
            .per_millisecond(200) // 5 req/s sustained
            .burst_size(20)
            .finish()
            .expect("invalid auth global rate-limit config"),
    );
    let auth_per_ip_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(SmartIpKeyExtractor)
            .per_second(1) // 1 req/s sustained (per_second(1) == per_millisecond(1000))
            .burst_size(5)
            .finish()
            .expect("invalid auth per-IP rate-limit config"),
    );

    let files = Router::new()
        .route("/gifs/{id}/file", get(routes::fetch::serve_file))
        .layer(GovernorLayer::new(file_rate_limit))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_session,
        ));

    let suggest = Router::new()
        .route("/gifs/tags/suggest", get(routes::search::suggest))
        .layer(GovernorLayer::new(suggest_rate_limit))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_session,
        ));

    let protected = Router::new()
        .route("/gifs",                post(routes::upload::upload))
        .route("/gifs/{id}",           get(routes::fetch::get_gif)
                                       .patch(routes::fetch::patch_gif)
                                       .delete(routes::fetch::delete_gif))
        .route("/gifs/{id}/select",    post(routes::fetch::select_gif))
        .route("/gifs/{id}/favorite",  put(routes::favorites::add_favorite)
                                       .delete(routes::favorites::remove_favorite))
        .route("/gifs/{id}/hide",      put(routes::favorites::add_hidden)
                                       .delete(routes::favorites::remove_hidden))
        .route("/gifs/{id}/tags",      put(routes::tags::put_tags)
                                       .patch(routes::tags::patch_tags)
                                       .delete(routes::tags::delete_tags))
        .route("/gifs/search",         get(routes::search::search))
        .route("/gifs/featured",       get(routes::search::featured))
        .route("/gifs/recent",         get(routes::search::recent))
        .route("/gifs/favorites",      get(routes::favorites::list_favorites))
        .route("/gifs/hidden",         get(routes::favorites::list_hidden))
        .route("/gifs/history",        get(routes::favorites::list_history))
        // Governor is added before the session layer so it ends up *inside*
        // it: the session middleware runs first and populates `AuthUser`,
        // then the per-user limiter reads it.
        .layer(GovernorLayer::new(per_user_rate_limit))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::require_session,
        ));

    let auth = Router::new()
        .route("/auth/matrix", post(routes::auth::matrix_login))
        .layer(GovernorLayer::new(auth_per_ip_rate_limit))
        .layer(GovernorLayer::new(auth_global_rate_limit));

    let public = Router::new().route("/health", get(routes::health::health));

    protected
        .merge(files)
        .merge(suggest)
        .merge(auth)
        .merge(public)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
