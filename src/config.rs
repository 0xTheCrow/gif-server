pub struct Config {
    pub database_url: String,
    pub storage_path: String,
    pub host:         String,
    pub port:         u16,
    pub base_url:     String,

    /// The Matrix homeserver name this server trusts, e.g. "example.com".
    /// Only OpenID tokens issued by this server are accepted, and only
    /// users whose mxid lives on this server can authenticate.
    pub matrix_server_name: String,
    /// Base URL where the homeserver's federation API is reachable for
    /// OpenID token verification, e.g. "https://example.com:8448" or a
    /// delegated host. No trailing slash.
    pub matrix_federation_url: String,
    /// Matrix user IDs granted admin rights (delete/edit any shared GIF).
    pub admin_mxids: Vec<String>,
    /// HMAC secret used to sign session JWTs.
    pub session_secret: String,
    /// Maximum total bytes the stored renditions may occupy. Uploads that
    /// would exceed this are rejected with 507.
    pub storage_max_bytes: u64,
    /// Per-uploader storage ceiling. A user's uploads may not exceed this.
    pub per_user_storage_bytes: u64,
    /// Allowed CORS origins (exact, e.g. https://app.example.com).
    /// Empty means "allow any origin" (with a startup warning).
    pub cors_allowed_origins: Vec<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let port: u16 = std::env::var("PORT")
            .unwrap_or_else(|_| "8847".to_string())
            .parse()
            .expect("PORT must be a number");

        let base_url = std::env::var("BASE_URL")
            .unwrap_or_else(|_| format!("http://localhost:{}", port));

        let admin_mxids = std::env::var("MATRIX_ADMIN_MXIDS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let storage_max_bytes = parse_size(
            &std::env::var("STORAGE_MAX_BYTES").unwrap_or_else(|_| "10GB".to_string()),
        )
        .expect("STORAGE_MAX_BYTES must be a byte count, optionally suffixed with KB/MB/GB/TB");

        let per_user_storage_bytes = parse_size(
            &std::env::var("PER_USER_STORAGE_BYTES").unwrap_or_else(|_| "1GB".to_string()),
        )
        .expect("PER_USER_STORAGE_BYTES must be a byte count, optionally suffixed with KB/MB/GB/TB");

        const MIN_SESSION_SECRET_LEN: usize = 32;
        let session_secret = std::env::var("SESSION_SECRET")
            .expect("SESSION_SECRET must be set");
        if session_secret.len() < MIN_SESSION_SECRET_LEN {
            panic!(
                "SESSION_SECRET must be at least {} characters; use a long random value",
                MIN_SESSION_SECRET_LEN
            );
        }

        Self {
            database_url: std::env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            storage_path: std::env::var("STORAGE_PATH")
                .unwrap_or_else(|_| "storage".to_string()),
            host: std::env::var("HOST")
                .unwrap_or_else(|_| "0.0.0.0".to_string()),
            port,
            base_url,
            matrix_server_name: std::env::var("MATRIX_SERVER_NAME")
                .expect("MATRIX_SERVER_NAME must be set (your homeserver name, e.g. example.com)"),
            matrix_federation_url: std::env::var("MATRIX_FEDERATION_URL")
                .expect("MATRIX_FEDERATION_URL must be set (where the federation API is reachable)")
                .trim_end_matches('/')
                .to_string(),
            admin_mxids,
            session_secret,
            storage_max_bytes,
            per_user_storage_bytes,
            cors_allowed_origins: std::env::var("CORS_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    pub fn is_admin(&self, mxid: &str) -> bool {
        self.admin_mxids.iter().any(|a| a == mxid)
    }
}

/// Parse a byte size: a plain integer (bytes) or an integer with a
/// case-insensitive KB/MB/GB/TB suffix (1024-based). "0" disables the cap
/// by mapping to u64::MAX.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let upper = s.to_uppercase();
    let (num, mult): (&str, u64) = if let Some(n) = upper.strip_suffix("TB") {
        (n, 1024u64.pow(4))
    } else if let Some(n) = upper.strip_suffix("GB") {
        (n, 1024u64.pow(3))
    } else if let Some(n) = upper.strip_suffix("MB") {
        (n, 1024u64.pow(2))
    } else if let Some(n) = upper.strip_suffix("KB") {
        (n, 1024)
    } else {
        (upper.as_str(), 1)
    };
    let value: u64 = num.trim().parse().ok()?;
    if value == 0 {
        return Some(u64::MAX);
    }
    value.checked_mul(mult)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn parses_plain_bytes() {
        assert_eq!(parse_size("1024"), Some(1024));
    }

    #[test]
    fn parses_suffixes_case_insensitively() {
        assert_eq!(parse_size("1KB"), Some(1024));
        assert_eq!(parse_size("2mb"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("3 GB"), Some(3 * 1024 * 1024 * 1024));
    }

    #[test]
    fn zero_disables_cap() {
        assert_eq!(parse_size("0"), Some(u64::MAX));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("1.5GB"), None);
    }
}
