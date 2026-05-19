use axum::http::{header, HeaderValue, Method};
use gif_server::{config::Config, create_router, storage::files, AppState};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
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

    let state = AppState {
        pool,
        config: config.clone(),
        http: reqwest::Client::new(),
    };

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(60)
            .burst_size(30)
            .finish()
            .unwrap(),
    );

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

    let app = create_router(state)
        .layer(GovernorLayer::new(governor_conf))
        .layer(cors);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
}
