use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::{config::Config, error::AppError, models::Gif};

/// Lifetime of a minted session token. The client transparently re-exchanges
/// a fresh Matrix OpenID token when this expires.
pub const SESSION_TTL_SECS: i64 = 3600;

/// The object a client obtains from `mx.getOpenIdToken()` and posts to
/// `/auth/matrix`. `token_type`/`expires_in` are accepted but unused.
#[derive(Debug, Deserialize)]
pub struct OpenIdToken {
    pub access_token: String,
    pub matrix_server_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    admin: bool,
    exp: usize,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    sub: String,
}

/// An authenticated request principal, injected by the session middleware.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub mxid: String,
    pub is_admin: bool,
}

impl AuthUser {
    /// A GIF is visible if it is shared, or the requester uploaded it.
    pub fn can_view(&self, gif: &Gif) -> bool {
        gif.visibility == "shared" || gif.uploader_id == self.mxid
    }

    /// A GIF may be mutated (delete, retag) by its uploader, or by an admin
    /// when it is shared. Admins never reach other users' private GIFs.
    pub fn can_mutate(&self, gif: &Gif) -> bool {
        gif.uploader_id == self.mxid || (self.is_admin && gif.visibility == "shared")
    }
}

/// Verify a Matrix OpenID token against the configured homeserver's
/// federation API and return the verified Matrix user ID.
pub async fn verify_openid_token(
    config: &Config,
    http: &reqwest::Client,
    token: &OpenIdToken,
) -> Result<String, AppError> {
    if token.matrix_server_name != config.matrix_server_name {
        return Err(AppError::Unauthorized);
    }

    let url = format!(
        "{}/_matrix/federation/v1/openid/userinfo",
        config.matrix_federation_url
    );

    let resp = http
        .get(&url)
        .query(&[("access_token", token.access_token.as_str())])
        .send()
        .await
        // without_url() strips the URL, which carries the access_token in a
        // query param, out of the error before it reaches the logs.
        .map_err(|e| AppError::Internal(format!(
            "openid verification request failed: {}", e.without_url()
        )))?;

    if !resp.status().is_success() {
        return Err(AppError::Unauthorized);
    }

    let info: UserInfo = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!(
            "openid userinfo decode failed: {}", e.without_url()
        )))?;

    let expected_suffix = format!(":{}", config.matrix_server_name);
    if !info.sub.starts_with('@') || !info.sub.ends_with(&expected_suffix) {
        return Err(AppError::Unauthorized);
    }

    Ok(info.sub)
}

/// Mint a signed session token for a verified mxid. Returns the token and
/// its lifetime in seconds.
pub fn mint_session(config: &Config, mxid: &str) -> Result<(String, i64), AppError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let claims = Claims {
        sub: mxid.to_string(),
        admin: config.is_admin(mxid),
        exp: (now + SESSION_TTL_SECS) as usize,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.session_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("failed to sign session: {e}")))?;
    Ok((token, SESSION_TTL_SECS))
}

/// Verify a session token's signature and expiry, returning the principal.
pub fn verify_session(config: &Config, token: &str) -> Option<AuthUser> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.session_secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    Some(AuthUser {
        mxid: data.claims.sub,
        is_admin: data.claims.admin,
    })
}
