use axum::http::{header, HeaderValue, Method};
use gif_server::{config::Config, create_router, storage::files, AppState};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "gif_server=debug,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env());

    if !config.base_url.starts_with("https://") && !config.base_url.starts_with("http://localhost") {
        tracing::warn!("BASE_URL is not HTTPS — session tokens will be transmitted in plaintext");
    }

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");

    // sqlx owns the schema: apply any pending migrations from ./migrations,
    // tracked in _sqlx_migrations. Fail fast if a migration can't apply.
    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("failed to apply database migrations");

    // Reconcile on-disk bytes against the DB total so orphaned/missing files
    // surface in logs at startup.
    match files::dir_size(&config.storage_path) {
        Ok(on_disk) => {
            let in_db = gif_server::storage::db::total_storage_bytes(&pool)
                .await
                .unwrap_or(0) as u64;
            if on_disk != in_db {
                tracing::warn!(
                    "storage reconciliation: {} bytes on disk vs {} bytes in DB",
                    on_disk, in_db
                );
            }
            tracing::info!(
                "storage: {} bytes used of {} byte cap",
                in_db, config.storage_max_bytes
            );
        }
        Err(e) => tracing::warn!("could not measure storage directory: {}", e),
    }

    // OpenID verification is reachable unauthenticated via POST /auth/matrix;
    // bound it so a slow/hung homeserver can't tie up workers indefinitely.
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        pool,
        config: config.clone(),
        http,
    };

    let cors = if config.cors_allowed_origins.is_empty() {
        tracing::warn!(
            "CORS_ALLOWED_ORIGINS is unset — allowing any origin. Set it in production."
        );
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = config
            .cors_allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
    };

    let app = create_router(state).layer(cors);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    // Serve with ConnectInfo so the rate limiter's SmartIpKeyExtractor can fall
    // back to the TCP peer address when no X-Forwarded-For header is present
    // (e.g. direct local requests). Without it, key extraction fails and the
    // auth per-IP limiter 500s every request.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}
