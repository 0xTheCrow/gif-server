use gif_server::{AppState, config::Config, create_router};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("add-key") {
        let name = args.get(2).expect("usage: gif-server add-key <name>");
        add_api_key(name).await;
        return;
    }

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "gif_server=debug,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env());

    if !config.base_url.starts_with("https://") && !config.base_url.starts_with("http://localhost") {
        tracing::warn!("BASE_URL is not HTTPS — API keys will be transmitted in plaintext");
    }

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");

    let state = AppState {
        pool,
        config: config.clone(),
        key_cache: Arc::new(RwLock::new(None)),
    };

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(60)
            .burst_size(30)
            .finish()
            .unwrap(),
    );

    let app = create_router(state)
        .layer(GovernorLayer::new(governor_conf))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
}

async fn add_api_key(name: &str) {
    use argon2::{password_hash::{rand_core::OsRng, SaltString}, Argon2, PasswordHasher};

    let config = Config::from_env();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");

    let key = uuid::Uuid::new_v4().to_string().replace("-", "");
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(key.as_bytes(), &salt)
        .expect("hashing failed")
        .to_string();

    sqlx::query("INSERT INTO api_keys (key_hash, name) VALUES ($1, $2)")
        .bind(&hash)
        .bind(name)
        .execute(&pool)
        .await
        .expect("failed to insert key");

    println!("API key for '{}': {}", name, key);
    println!("Store this — it won't be shown again.");
}
