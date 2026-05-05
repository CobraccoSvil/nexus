use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{github, AppState};

// Re-export shared auth types from nexus-auth crate
pub use nexus_auth::{
    Claims, backend_url, frontend_url, get_or_create_jwt_secret, get_setting, validate_token,
};

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: Option<String>,
}

// --- Local helpers ---

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn is_secure_context() -> bool {
    let frontend = frontend_url();
    frontend.starts_with("https://")
}

fn set_cookie_header(token: &str) -> String {
    let secure = if is_secure_context() { " Secure;" } else { "" };
    format!(
        "token={}; HttpOnly;{} Path=/; Max-Age=86400; SameSite=Lax",
        token, secure
    )
}

fn clear_cookie_header() -> String {
    let secure = if is_secure_context() { " Secure;" } else { "" };
    format!("token=; HttpOnly;{} Path=/; Max-Age=0; SameSite=Lax", secure)
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

// --- Route Handlers ---

pub async fn github_login(State(state): State<AppState>) -> Response {
    match github::build_github_oauth_url(&state.db, "login", None, Some("/")).await {
        Ok(url) => axum::response::Redirect::temporary(&url).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "GitHub OAuth not configured. Set github_client_id in admin settings.",
        )
            .into_response(),
    }
}

pub async fn github_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    match handle_callback(&state, &q.code, q.state.as_deref()).await {
        Ok(response) => response,
        Err(e) => {
            tracing::error!("GitHub OAuth callback error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("OAuth error: {e}"),
            )
                .into_response()
        }
    }
}

async fn handle_callback(state: &AppState, code: &str, raw_state: Option<&str>) -> anyhow::Result<Response> {
    let callback_state = github::decode_github_oauth_state(&state.db, raw_state).await?;
    let identity = github::exchange_code_for_identity(&state.db, code).await?;

    let row: (Uuid, String) = if callback_state.intent == "connect_github" {
        let target_user_id = callback_state
            .user_id
            .ok_or_else(|| anyhow::anyhow!("GitHub OAuth state mismatch"))?;

        let linked_user_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM users WHERE github_id = $1 LIMIT 1",
        )
        .bind(identity.github_user_id)
        .fetch_optional(&state.db)
        .await?;

        if let Some(existing_user_id) = linked_user_id {
            if existing_user_id != target_user_id {
                anyhow::bail!("Questo account GitHub e' gia' collegato a un altro utente Nexus");
            }
        }

        sqlx::query_as(
            r#"
            UPDATE users
            SET email = $1,
                github_id = $2,
                github_username = $3,
                avatar_url = $4
            WHERE id = $5
            RETURNING id, role
            "#,
        )
        .bind(&identity.email)
        .bind(identity.github_user_id)
        .bind(&identity.github_username)
        .bind(&identity.avatar_url)
        .bind(target_user_id)
        .fetch_one(&state.db)
        .await?
    } else if let Some(existing_user_id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE github_id = $1 LIMIT 1",
    )
    .bind(identity.github_user_id)
    .fetch_optional(&state.db)
    .await?
    {
        sqlx::query_as(
            r#"
            UPDATE users
            SET email = $1,
                github_username = $2,
                avatar_url = $3
            WHERE id = $4
            RETURNING id, role
            "#,
        )
        .bind(&identity.email)
        .bind(&identity.github_username)
        .bind(&identity.avatar_url)
        .bind(existing_user_id)
        .fetch_one(&state.db)
        .await?
    } else if let Some(existing_user_id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE lower(email) = lower($1) LIMIT 1",
    )
    .bind(&identity.email)
    .fetch_optional(&state.db)
    .await?
    {
        sqlx::query_as(
            r#"
            UPDATE users
            SET github_id = $1,
                github_username = $2,
                avatar_url = $3
            WHERE id = $4
            RETURNING id, role
            "#,
        )
        .bind(identity.github_user_id)
        .bind(&identity.github_username)
        .bind(&identity.avatar_url)
        .bind(existing_user_id)
        .fetch_one(&state.db)
        .await?
    } else {
        // Check if this is the first user ever (for admin auto-assignment)
        let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&state.db)
            .await
            .unwrap_or(1);

        let initial_role = if user_count == 0 { "admin" } else { "viewer" };

        sqlx::query_as(
            r#"
            INSERT INTO users (id, email, display_name, github_id, github_username, avatar_url, role)
            VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6)
            RETURNING id, role
            "#,
        )
        .bind(&identity.email)
        .bind(&identity.github_username)
        .bind(identity.github_user_id)
        .bind(&identity.github_username)
        .bind(&identity.avatar_url)
        .bind(initial_role)
        .fetch_one(&state.db)
        .await?
    };

    let (user_id, role) = row;
    github::upsert_github_connection(&state.db, user_id, &identity).await?;

    // Generate JWT
    let jwt_secret = get_or_create_jwt_secret(&state.db).await?;
    let exp = (Utc::now() + Duration::hours(24)).timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        role,
        exp,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )?;

    // Store session
    let token_hash = hash_token(&token);
    sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(&token_hash)
        .bind(Utc::now() + Duration::hours(24))
        .execute(&state.db)
        .await?;

    // Redirect to frontend with cookie
    let redirect_target = format!("{}{}", frontend_url(), callback_state.return_to);
    let mut response = Response::builder()
        .status(302)
        .header(header::LOCATION, redirect_target);

    response = response.header(header::SET_COOKIE, set_cookie_header(&token));

    Ok(response.body(axum::body::Body::empty())?)
}

pub async fn me(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let claims = match req.extensions().get::<Claims>() {
        Some(c) => c.clone(),
        None => return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response(),
    };

    let user = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String)>(
        "SELECT email, display_name, github_username, avatar_url, role FROM users WHERE id = $1",
    )
    .bind(Uuid::parse_str(&claims.sub).unwrap_or_default())
    .fetch_optional(&state.db)
    .await;

    match user {
        Ok(Some((email, display_name, github_username, avatar_url, role))) => {
            let github_summary = github::github_account_summary(
                &state.db,
                Uuid::parse_str(&claims.sub).unwrap_or_default(),
            )
            .await
            .unwrap_or(github::GitHubAccountSummary {
                username: github_username.clone(),
                avatar_url: avatar_url.clone(),
                status: "not_connected".to_string(),
                connected: false,
                scopes: Vec::new(),
                expires_at: None,
            });
            Json(serde_json::json!({
                "id": claims.sub,
                "email": email,
                "display_name": display_name,
                "github_username": github_username,
                "avatar_url": avatar_url,
                "role": role,
                "githubConnected": github_summary.connected,
                "githubConnectionStatus": github_summary.status,
                "githubScopes": github_summary.scopes,
            }))
            .into_response()
        }
        _ => (StatusCode::NOT_FOUND, "User not found").into_response(),
    }
}

pub async fn logout(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    if let Some(token) = extract_token_from_cookie(req.headers()) {
        let token_hash = hash_token(&token);
        let _ = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(&token_hash)
            .execute(&state.db)
            .await;
    }

    Response::builder()
        .status(200)
        .header(header::SET_COOKIE, clear_cookie_header())
        .body(axum::body::Body::from(r#"{"status":"ok"}"#))
        .unwrap()
}

// validate_token is now re-exported from nexus_auth crate
