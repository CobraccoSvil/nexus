use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

// --- JWT Claims ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String, // user_id
    pub role: String,
    pub exp: usize,
}

// --- Helpers ---

pub async fn get_setting(db: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
}

pub async fn get_or_create_jwt_secret(db: &PgPool) -> anyhow::Result<String> {
    if let Some(secret) = get_setting(db, "jwt_secret").await {
        return Ok(secret);
    }
    let secret: String = (0..64)
        .map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>()))
        .collect();
    sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = 'jwt_secret'")
        .bind(&secret)
        .execute(db)
        .await?;
    Ok(secret)
}

pub fn frontend_url() -> String {
    std::env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string())
}

pub fn backend_url() -> String {
    std::env::var("PUBLIC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:4000".to_string())
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn extract_token_from_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| {
            let c = c.trim();
            c.strip_prefix("token=").map(|v| v.to_string())
        })
}

// --- Token validation ---

pub async fn validate_token(
    db: &PgPool,
    headers: &axum::http::HeaderMap,
) -> Result<Claims, StatusCode> {
    let token = extract_token_from_cookie(headers).ok_or(StatusCode::UNAUTHORIZED)?;

    let jwt_secret = get_setting(db, "jwt_secret")
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token_data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Verify session exists and not expired
    let token_hash = hash_token(&token);
    let session_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE token_hash = $1 AND expires_at > NOW())",
    )
    .bind(&token_hash)
    .fetch_one(db)
    .await
    .unwrap_or(false);

    if !session_exists {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(token_data.claims)
}

// --- Middleware ---

/// Middleware that requires a valid JWT token.
/// Inserts Claims into request extensions on success.
pub async fn require_auth<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = validate_token(&db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Middleware that requires a valid JWT token with admin role.
pub async fn require_admin<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    match validate_token(&db, req.headers()).await {
        Ok(claims) => {
            if claims.role != "admin" {
                tracing::warn!(
                    "require_admin: access denied - role={} is not admin, path={}",
                    claims.role,
                    req.uri()
                );
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            tracing::warn!("require_admin: token validation failed: {:?}, path={}", e, req.uri());
            Err(e)
        }
    }
}
