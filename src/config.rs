pub struct Config {
    pub database_url: String,
    pub storage_path: String,
    pub host:         String,
    pub port:         u16,
    pub base_url:     String,
}

impl Config {
    pub fn from_env() -> Self {
        let port: u16 = std::env::var("PORT")
            .unwrap_or_else(|_| "8847".to_string())
            .parse()
            .expect("PORT must be a number");

        let base_url = std::env::var("BASE_URL")
            .unwrap_or_else(|_| format!("http://localhost:{}", port));

        Self {
            database_url: std::env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            storage_path: std::env::var("STORAGE_PATH")
                .unwrap_or_else(|_| "storage".to_string()),
            host: std::env::var("HOST")
                .unwrap_or_else(|_| "0.0.0.0".to_string()),
            port,
            base_url,
        }
    }
}
