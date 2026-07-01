use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    auth::{backend_url, get_or_create_jwt_secret, get_setting, Claims},
    projects::{
        api_error, load_project_context, parse_user_id, refresh_git_snapshot, run_git_command,
        run_git_command_with_options, GitCommandOptions, ProjectContext,
    },
    AppState,
};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

const GITHUB_REQUIRED_SCOPES: &[&str] = &["read:user", "user:email", "repo"];
const GITHUB_USER_AGENT: &str = "Nexus-IDEAI";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubConnectRequest {
    pub return_to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubCreatePullRequestRequest {
    pub title: String,
    pub body: Option<String>,
    pub base_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubCloneRepositoryRequest {
    pub clone_url: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GitHubAccountSummary {
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub status: String,
    pub connected: bool,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitHubAuthorizedUser {
    pub username: String,
    pub access_token: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct GitHubExchangeResult {
    pub github_user_id: i64,
    pub github_username: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub scopes: Vec<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GitHubPullRequestSummary {
    number: i64,
    html_url: String,
    title: String,
    state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitHubRemoteStatusResponse {
    available: bool,
    reason: String,
    remote_name: Option<String>,
    remote_url: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    repo_full_name: Option<String>,
    branch: Option<String>,
    upstream: Option<String>,
    ahead: i32,
    behind: i32,
    published: bool,
    default_branch: Option<String>,
    can_push_pull: bool,
    suggested_pr_title: Option<String>,
    last_commit_title: Option<String>,
    pull_request: Option<GitHubPullRequestSummary>,
    api_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    refresh_token_expires_in: Option<i64>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubUserResponse {
    id: i64,
    login: String,
    email: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubEmailResponse {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoOwnerResponse {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubUserRepoResponse {
    id: i64,
    name: String,
    full_name: String,
    html_url: String,
    clone_url: String,
    #[serde(default)]
    private: bool,
    default_branch: String,
    updated_at: String,
    owner: GitHubRepoOwnerResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitHubRepositorySummary {
    id: i64,
    name: String,
    full_name: String,
    owner_login: String,
    html_url: String,
    clone_url: String,
    private: bool,
    default_branch: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct GitHubPullRequestResponse {
    number: i64,
    html_url: String,
    title: String,
    state: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GitHubOAuthStateClaims {
    intent: String,
    user_id: Option<String>,
    return_to: Option<String>,
    exp: usize,
}

#[derive(Debug, Clone)]
pub struct GitHubCallbackState {
    pub intent: String,
    pub user_id: Option<Uuid>,
    pub return_to: String,
}

#[derive(Debug)]
struct GitHubConnectionRecord {
    github_username: Option<String>,
    avatar_url: Option<String>,
    connection_status: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_scope: String,
    access_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct GitHubHttpsRemote {
    remote_name: String,
    remote_url: String,
    owner: String,
    repo: String,
}

enum GitHubRemoteResolution {
    GitHubHttps(GitHubHttpsRemote),
    GitHubSsh {
        remote_name: String,
        remote_url: String,
        owner: Option<String>,
        repo: Option<String>,
    },
    Other {
        remote_name: String,
        remote_url: String,
    },
    Missing,
}

#[derive(Debug, Default)]
struct BranchRemoteStatus {
    branch: Option<String>,
    upstream: Option<String>,
    ahead: i32,
    behind: i32,
}

fn github_client() -> Client {
    // Usa nexus-http per supporto proxy applicativo (NEXUS_PROXY) e ottimizzazioni
    nexus_http::build_client_with_config(&nexus_http::NexusHttpConfig {
        timeout_secs: 25,
        pool_max: 10,
        pool_idle_timeout_secs: 60,
        proxy: std::env::var("NEXUS_PROXY").ok().filter(|v| !v.is_empty()),
    })
}

fn sanitize_return_to(value: Option<&str>) -> String {
    let raw = value.unwrap_or("/").trim();
    if raw.starts_with('/') && !raw.starts_with("//") {
        raw.to_string()
    } else {
        "/".to_string()
    }
}

fn parse_scope_list(raw: &str) -> Vec<String> {
    raw.split([',', ' '])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn has_required_scopes(scopes: &[String]) -> bool {
    GITHUB_REQUIRED_SCOPES
        .iter()
        .all(|required| scopes.iter().any(|scope| scope == required))
}

async fn oauth_encryption_key(db: &PgPool) -> anyhow::Result<String> {
    if let Some(secret) = get_setting(db, "oauth_data_encryption_key").await {
        return Ok(secret);
    }

    let secret: String = (0..64)
        .map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>()))
        .collect();
    sqlx::query(
        "UPDATE settings SET value = $1, updated_at = NOW() WHERE key = 'oauth_data_encryption_key'",
    )
    .bind(&secret)
    .execute(db)
    .await?;
    Ok(secret)
}

pub(crate) async fn build_github_oauth_url(
    db: &PgPool,
    intent: &str,
    user_id: Option<Uuid>,
    return_to: Option<&str>,
) -> anyhow::Result<String> {
    let client_id = get_setting(db, "github_client_id")
        .await
        .ok_or_else(|| anyhow::anyhow!("github_client_id not configured"))?;
    let jwt_secret = get_or_create_jwt_secret(db).await?;
    let callback_url = format!("{}/auth/github/callback", backend_url());
    let state = GitHubOAuthStateClaims {
        intent: intent.to_string(),
        user_id: user_id.map(|value| value.to_string()),
        return_to: Some(sanitize_return_to(return_to)),
        exp: (Utc::now() + Duration::minutes(20)).timestamp() as usize,
    };
    let signed_state = encode(
        &Header::default(),
        &state,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )?;

    Ok(format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope={}&state={}",
        urlencoding::encode(&client_id),
        urlencoding::encode(&callback_url),
        urlencoding::encode(&GITHUB_REQUIRED_SCOPES.join(" ")),
        urlencoding::encode(&signed_state),
    ))
}

pub(crate) async fn decode_github_oauth_state(
    db: &PgPool,
    raw_state: Option<&str>,
) -> anyhow::Result<GitHubCallbackState> {
    let Some(raw_state) = raw_state.filter(|value| !value.trim().is_empty()) else {
        return Ok(GitHubCallbackState {
            intent: "login".to_string(),
            user_id: None,
            return_to: "/".to_string(),
        });
    };

    let jwt_secret = get_or_create_jwt_secret(db).await?;
    let token_data = decode::<GitHubOAuthStateClaims>(
        raw_state,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    )?;

    Ok(GitHubCallbackState {
        intent: token_data.claims.intent,
        user_id: token_data
            .claims
            .user_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok()),
        return_to: sanitize_return_to(token_data.claims.return_to.as_deref()),
    })
}

async fn github_exchange_token(db: &PgPool, payload: Value) -> anyhow::Result<GitHubTokenResponse> {
    let client_id = get_setting(db, "github_client_id")
        .await
        .ok_or_else(|| anyhow::anyhow!("github_client_id not configured"))?;
    let client_secret = get_setting(db, "github_client_secret")
        .await
        .ok_or_else(|| anyhow::anyhow!("github_client_secret not configured"))?;

    let mut body = serde_json::Map::new();
    body.insert("client_id".to_string(), json!(client_id));
    body.insert("client_secret".to_string(), json!(client_secret));
    if let Some(payload_object) = payload.as_object() {
        for (key, value) in payload_object {
            body.insert(key.clone(), value.clone());
        }
    }

    let response = github_client()
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .json(&Value::Object(body))
        .send()
        .await?;

    let token_response = response.json::<GitHubTokenResponse>().await?;
    if let Some(error) = token_response.error.clone() {
        anyhow::bail!("{}", token_response.error_description.unwrap_or(error));
    }
    Ok(token_response)
}

async fn fetch_github_user(access_token: &str) -> anyhow::Result<GitHubUserResponse> {
    let response = github_client()
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", GITHUB_USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub user profile fetch failed with {}",
            response.status()
        );
    }

    Ok(response.json::<GitHubUserResponse>().await?)
}

async fn resolve_github_email(
    access_token: &str,
    current_email: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(email) = current_email.filter(|value| !value.trim().is_empty()) {
        return Ok(email.to_string());
    }

    let response = github_client()
        .get("https://api.github.com/user/emails")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", GITHUB_USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub email fetch failed with {}", response.status());
    }

    let emails = response.json::<Vec<GitHubEmailResponse>>().await?;
    Ok(emails
        .into_iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email)
        .unwrap_or_else(|| "github@local.invalid".to_string()))
}

pub(crate) async fn exchange_code_for_identity(
    db: &PgPool,
    code: &str,
) -> anyhow::Result<GitHubExchangeResult> {
    let token = github_exchange_token(db, json!({ "code": code })).await?;
    let access_token = token
        .access_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("GitHub did not return an access token"))?;
    let github_user = fetch_github_user(&access_token).await?;
    let email = resolve_github_email(&access_token, github_user.email.as_deref()).await?;
    let scopes = parse_scope_list(token.scope.as_deref().unwrap_or(""));

    Ok(GitHubExchangeResult {
        github_user_id: github_user.id,
        github_username: github_user.login,
        email,
        avatar_url: github_user.avatar_url,
        access_token,
        refresh_token: token.refresh_token,
        scopes,
        access_token_expires_at: token
            .expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
        refresh_token_expires_at: token
            .refresh_token_expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
    })
}

pub(crate) async fn upsert_github_connection(
    db: &PgPool,
    user_id: Uuid,
    identity: &GitHubExchangeResult,
) -> anyhow::Result<()> {
    let encryption_key = oauth_encryption_key(db).await?;
    let refresh_token = identity.refresh_token.as_deref();
    let scopes = identity.scopes.join(",");

    sqlx::query(
        r#"
        INSERT INTO github_connections (
            user_id, github_user_id, github_username, connection_status,
            access_token_encrypted, refresh_token_encrypted, token_scope,
            access_token_expires_at, refresh_token_expires_at, created_at, updated_at
        )
        VALUES (
            $1, $2, $3, 'connected',
            pgp_sym_encrypt($4, $8),
            CASE WHEN $5 IS NULL THEN NULL ELSE pgp_sym_encrypt($5, $8) END,
            $6, $7, $9, NOW(), NOW()
        )
        ON CONFLICT (user_id) DO UPDATE
        SET github_user_id = EXCLUDED.github_user_id,
            github_username = EXCLUDED.github_username,
            connection_status = 'connected',
            access_token_encrypted = EXCLUDED.access_token_encrypted,
            refresh_token_encrypted = EXCLUDED.refresh_token_encrypted,
            token_scope = EXCLUDED.token_scope,
            access_token_expires_at = EXCLUDED.access_token_expires_at,
            refresh_token_expires_at = EXCLUDED.refresh_token_expires_at,
            updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(identity.github_user_id)
    .bind(&identity.github_username)
    .bind(&identity.access_token)
    .bind(refresh_token)
    .bind(scopes)
    .bind(identity.access_token_expires_at)
    .bind(&encryption_key)
    .bind(identity.refresh_token_expires_at)
    .execute(db)
    .await?;

    Ok(())
}

pub(crate) async fn disconnect_github_connection(db: &PgPool, user_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO github_connections (user_id, connection_status, token_scope, created_at, updated_at)
        VALUES ($1, 'disconnected', '', NOW(), NOW())
        ON CONFLICT (user_id) DO UPDATE
        SET connection_status = 'disconnected',
            access_token_encrypted = NULL,
            refresh_token_encrypted = NULL,
            token_scope = '',
            access_token_expires_at = NULL,
            refresh_token_expires_at = NULL,
            updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .execute(db)
    .await?;

    Ok(())
}

async fn load_github_connection_record(
    db: &PgPool,
    user_id: Uuid,
) -> anyhow::Result<GitHubConnectionRecord> {
    let encryption_key = oauth_encryption_key(db).await?;
    let row = sqlx::query(
        r#"
        SELECT
            u.github_username AS user_github_username,
            u.avatar_url,
            gc.connection_status,
            gc.github_username,
            gc.token_scope,
            gc.access_token_expires_at,
            CASE
                WHEN gc.access_token_encrypted IS NULL THEN NULL
                ELSE pgp_sym_decrypt(gc.access_token_encrypted, $2)
            END AS access_token,
            CASE
                WHEN gc.refresh_token_encrypted IS NULL THEN NULL
                ELSE pgp_sym_decrypt(gc.refresh_token_encrypted, $2)
            END AS refresh_token
        FROM users u
        LEFT JOIN github_connections gc ON gc.user_id = u.id
        WHERE u.id = $1
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(encryption_key)
    .fetch_optional(db)
    .await?;

    let Some(row) = row else {
        anyhow::bail!("GitHub account owner not found");
    };

    Ok(connection_record_from_row(&row))
}

/// Mappa una riga della join users/github_connections nel record applicativo,
/// tollerando colonne mancanti o NULL (accessi via `try_get`).
fn connection_record_from_row(row: &sqlx::postgres::PgRow) -> GitHubConnectionRecord {
    let opt_str = |column: &str| row.try_get::<Option<String>, _>(column).ok().flatten();

    GitHubConnectionRecord {
        github_username: opt_str("github_username").or_else(|| opt_str("user_github_username")),
        avatar_url: opt_str("avatar_url"),
        connection_status: opt_str("connection_status"),
        access_token: opt_str("access_token"),
        refresh_token: opt_str("refresh_token"),
        token_scope: opt_str("token_scope").unwrap_or_default(),
        access_token_expires_at: row
            .try_get::<Option<DateTime<Utc>>, _>("access_token_expires_at")
            .ok()
            .flatten(),
    }
}

fn is_expired(expires_at: Option<DateTime<Utc>>) -> bool {
    expires_at
        .map(|value| value <= Utc::now() + Duration::minutes(1))
        .unwrap_or(false)
}

async fn refresh_github_connection(
    db: &PgPool,
    user_id: Uuid,
    refresh_token: &str,
) -> anyhow::Result<GitHubAuthorizedUser> {
    let token = github_exchange_token(
        db,
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token
        }),
    )
    .await?;
    let access_token = token
        .access_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("GitHub did not return an access token"))?;
    let github_user = fetch_github_user(&access_token).await?;
    let identity = GitHubExchangeResult {
        github_user_id: github_user.id,
        github_username: github_user.login,
        email: resolve_github_email(&access_token, github_user.email.as_deref()).await?,
        avatar_url: github_user.avatar_url,
        access_token,
        refresh_token: token.refresh_token,
        scopes: parse_scope_list(token.scope.as_deref().unwrap_or("")),
        access_token_expires_at: token
            .expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
        refresh_token_expires_at: token
            .refresh_token_expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
    };
    upsert_github_connection(db, user_id, &identity).await?;

    Ok(GitHubAuthorizedUser {
        username: identity.github_username,
        access_token: identity.access_token,
        scopes: identity.scopes,
        expires_at: identity.access_token_expires_at,
    })
}

pub(crate) async fn ensure_github_authorized_user(
    db: &PgPool,
    user_id: Uuid,
) -> anyhow::Result<Option<GitHubAuthorizedUser>> {
    let record = load_github_connection_record(db, user_id).await?;
    let scopes = parse_scope_list(&record.token_scope);

    if record.connection_status.as_deref() != Some("connected") || !has_required_scopes(&scopes) {
        return Ok(None);
    }

    if is_expired(record.access_token_expires_at) {
        if let Some(refresh_token) = record.refresh_token.as_deref() {
            return refresh_github_connection(db, user_id, refresh_token)
                .await
                .map(Some);
        }
        return Ok(None);
    }

    let Some(access_token) = record.access_token else {
        return Ok(None);
    };
    let Some(username) = record.github_username else {
        return Ok(None);
    };

    Ok(Some(GitHubAuthorizedUser {
        username,
        access_token,
        scopes,
        expires_at: record.access_token_expires_at,
    }))
}

/// Costruisce un summary in stato non-connesso/degradato riutilizzando i campi
/// comuni del record (avatar, expires_at, username fallback). `connected` e'
/// sempre falso per questi stati.
fn degraded_account_summary(
    record: &GitHubConnectionRecord,
    username: Option<String>,
    status: &str,
    scopes: Vec<String>,
) -> GitHubAccountSummary {
    GitHubAccountSummary {
        username,
        avatar_url: record.avatar_url.clone(),
        status: status.to_string(),
        connected: false,
        scopes,
        expires_at: record
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
    }
}

pub(crate) async fn github_account_summary(
    db: &PgPool,
    user_id: Uuid,
) -> anyhow::Result<GitHubAccountSummary> {
    let record = load_github_connection_record(db, user_id).await?;
    let scopes = parse_scope_list(&record.token_scope);
    let fallback_username = record.github_username.clone();

    if record.connection_status.as_deref() == Some("disconnected") {
        return Ok(GitHubAccountSummary {
            username: fallback_username,
            avatar_url: record.avatar_url,
            status: "not_connected".to_string(),
            connected: false,
            scopes: Vec::new(),
            expires_at: None,
        });
    }

    if record.connection_status.is_none() || record.access_token.is_none() {
        let status = if fallback_username.is_some() {
            "upgrade_required"
        } else {
            "not_connected"
        };
        return Ok(degraded_account_summary(
            &record,
            fallback_username,
            status,
            scopes,
        ));
    }

    if !has_required_scopes(&scopes) {
        return Ok(degraded_account_summary(
            &record,
            fallback_username,
            "upgrade_required",
            scopes,
        ));
    }

    match ensure_github_authorized_user(db, user_id).await {
        Ok(Some(authorized)) => Ok(GitHubAccountSummary {
            username: Some(authorized.username),
            avatar_url: record.avatar_url,
            status: "connected".to_string(),
            connected: true,
            scopes: authorized.scopes,
            expires_at: authorized.expires_at.map(|value| value.to_rfc3339()),
        }),
        Ok(None) => Ok(degraded_account_summary(
            &record,
            fallback_username,
            "reconnect_required",
            scopes,
        )),
        Err(error) => {
            tracing::warn!("GitHub connection refresh failed for {user_id}: {error}");
            Ok(degraded_account_summary(
                &record,
                fallback_username,
                "reconnect_required",
                scopes,
            ))
        }
    }
}

/// Costruisce la risoluzione SSH da un path `owner/repo` estratto dopo il prefisso
/// git@ / ssh://; il `repo` viene ripulito dal suffisso `.git`.
fn ssh_remote_resolution(
    remote_name: &str,
    remote_url: &str,
    path: &str,
) -> GitHubRemoteResolution {
    let parts = path.split('/').collect::<Vec<_>>();
    GitHubRemoteResolution::GitHubSsh {
        remote_name: remote_name.to_string(),
        remote_url: remote_url.to_string(),
        owner: parts.first().map(|value| (*value).to_string()),
        repo: parts
            .get(1)
            .map(|value| value.trim_end_matches(".git").to_string()),
    }
}

fn parse_github_remote_url(remote_name: &str, remote_url: &str) -> GitHubRemoteResolution {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return GitHubRemoteResolution::Missing;
    }

    let normalize_repo = |raw: &str| raw.trim_end_matches(".git").to_string();

    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return ssh_remote_resolution(remote_name, trimmed, path);
    }

    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return ssh_remote_resolution(remote_name, trimmed, path);
    }

    match reqwest::Url::parse(trimmed) {
        Ok(url) if url.host_str() == Some("github.com") && url.scheme() == "https" => {
            let segments = url
                .path_segments()
                .map(|items| items.collect::<Vec<_>>())
                .unwrap_or_default();
            if segments.len() >= 2 {
                return GitHubRemoteResolution::GitHubHttps(GitHubHttpsRemote {
                    remote_name: remote_name.to_string(),
                    remote_url: trimmed.to_string(),
                    owner: segments[0].to_string(),
                    repo: normalize_repo(segments[1]),
                });
            }
            GitHubRemoteResolution::Other {
                remote_name: remote_name.to_string(),
                remote_url: trimmed.to_string(),
            }
        }
        _ => GitHubRemoteResolution::Other {
            remote_name: remote_name.to_string(),
            remote_url: trimmed.to_string(),
        },
    }
}

fn parse_branch_remote_status(status_output: &str) -> BranchRemoteStatus {
    let mut status = BranchRemoteStatus::default();
    let Some(header) = status_output.lines().next() else {
        return status;
    };
    let header = header.trim_start_matches("## ").trim();
    let mut branch_part = header;
    let mut tracking_part = "";
    if let Some((left, right)) = header.split_once(" [") {
        branch_part = left;
        tracking_part = right.trim_end_matches(']');
    }

    if let Some((branch, upstream)) = branch_part.split_once("...") {
        status.branch = Some(branch.trim().to_string());
        if !upstream.trim().is_empty() {
            status.upstream = Some(upstream.trim().to_string());
        }
    } else if !branch_part.is_empty() {
        status.branch = Some(branch_part.trim().to_string());
    }

    for entry in tracking_part.split(',') {
        let part = entry.trim();
        if let Some(value) = part.strip_prefix("ahead ") {
            status.ahead = value.trim().parse::<i32>().unwrap_or(0);
        }
        if let Some(value) = part.strip_prefix("behind ") {
            status.behind = value.trim().parse::<i32>().unwrap_or(0);
        }
    }

    status
}

fn extract_github_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|payload| {
            payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string())
}

async fn github_api_get<T: for<'de> Deserialize<'de>>(
    access_token: &str,
    url: &str,
) -> anyhow::Result<T> {
    let response = github_client()
        .get(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", GITHUB_USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API {}: {}", status, extract_github_message(&body));
    }

    Ok(response.json::<T>().await?)
}

async fn github_api_post<T: for<'de> Deserialize<'de>>(
    access_token: &str,
    url: &str,
    body: Value,
) -> anyhow::Result<T> {
    let response = github_client()
        .post(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", GITHUB_USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let payload = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "GitHub API {}: {}",
            status,
            extract_github_message(&payload)
        );
    }

    Ok(response.json::<T>().await?)
}

async fn resolve_remote_url(root: &std::path::Path, remote_name: &str) -> GitHubRemoteResolution {
    match run_git_command(root, &["remote", "get-url", remote_name]).await {
        Ok((stdout, _)) => parse_github_remote_url(remote_name, stdout.trim()),
        Err(_) => GitHubRemoteResolution::Missing,
    }
}

async fn load_origin_remote(root: &std::path::Path) -> GitHubRemoteResolution {
    resolve_remote_url(root, "origin").await
}

async fn read_last_commit_title(root: &std::path::Path) -> Option<String> {
    run_git_command(root, &["log", "-1", "--pretty=%s"])
        .await
        .ok()
        .map(|(stdout, _)| stdout.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Costruisce un response in stato "non pubblicabile su GitHub" (available=false)
/// popolando i campi comuni da `branch_status`/`last_commit_title`. I campi
/// specifici del remote (reason, nome/url, owner/repo) sono passati esplicitamente.
fn unavailable_remote_status(
    reason: &str,
    remote_name: Option<String>,
    remote_url: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    branch_status: BranchRemoteStatus,
    last_commit_title: Option<String>,
) -> GitHubRemoteStatusResponse {
    let repo_full_name = owner
        .as_ref()
        .zip(repo.as_ref())
        .map(|(left, right)| format!("{left}/{right}"));
    let published = branch_status.upstream.is_some();
    GitHubRemoteStatusResponse {
        available: false,
        reason: reason.to_string(),
        remote_name,
        remote_url,
        owner,
        repo,
        repo_full_name,
        branch: branch_status.branch,
        upstream: branch_status.upstream,
        ahead: branch_status.ahead,
        behind: branch_status.behind,
        published,
        default_branch: None,
        can_push_pull: false,
        suggested_pr_title: last_commit_title.clone(),
        last_commit_title,
        pull_request: None,
        api_error: None,
    }
}

/// Arricchisce il response di un remote GitHub HTTPS con i dati che richiedono
/// il token utente: `default_branch` del repo e l'eventuale PR aperta per il
/// branch corrente. Gli errori API sono catturati in `api_error`.
async fn enrich_https_remote_status(
    db: &PgPool,
    user_id: Uuid,
    remote: &GitHubHttpsRemote,
    branch: Option<&str>,
    response: &mut GitHubRemoteStatusResponse,
) -> anyhow::Result<()> {
    let Some(authorized) = ensure_github_authorized_user(db, user_id).await? else {
        return Ok(());
    };
    response.can_push_pull = true;
    let repo_url = format!(
        "https://api.github.com/repos/{}/{}",
        remote.owner, remote.repo
    );
    match github_api_get::<GitHubRepoResponse>(&authorized.access_token, &repo_url).await {
        Ok(repo_info) => response.default_branch = Some(repo_info.default_branch),
        Err(error) => response.api_error = Some(error.to_string()),
    }

    if response.api_error.is_some() {
        return Ok(());
    }
    let Some(branch) = branch.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let pulls_url = format!(
        "https://api.github.com/repos/{}/{}/pulls?head={}:{}&state=open&per_page=1",
        remote.owner, remote.repo, remote.owner, branch
    );
    match github_api_get::<Vec<GitHubPullRequestResponse>>(&authorized.access_token, &pulls_url)
        .await
    {
        Ok(mut pulls) => {
            if let Some(pr) = pulls.drain(..).next() {
                response.pull_request = Some(GitHubPullRequestSummary {
                    number: pr.number,
                    html_url: pr.html_url,
                    title: pr.title,
                    state: pr.state,
                });
            }
        }
        Err(error) => response.api_error = Some(error.to_string()),
    }
    Ok(())
}

async fn build_remote_status(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
) -> anyhow::Result<GitHubRemoteStatusResponse> {
    if !context.is_git_repo {
        return Ok(unavailable_remote_status(
            "not_git_repo",
            None,
            None,
            None,
            None,
            BranchRemoteStatus {
                branch: context.current_branch.clone(),
                ..BranchRemoteStatus::default()
            },
            None,
        ));
    }

    let (status_stdout, _) = run_git_command(
        &context.repository_root_path,
        &["status", "--porcelain=1", "--branch"],
    )
    .await?;
    let branch_status = parse_branch_remote_status(&status_stdout);
    let last_commit_title = read_last_commit_title(&context.repository_root_path).await;

    match load_origin_remote(&context.repository_root_path).await {
        GitHubRemoteResolution::Missing => Ok(unavailable_remote_status(
            "missing_origin_remote",
            Some("origin".to_string()),
            None,
            None,
            None,
            branch_status,
            last_commit_title,
        )),
        GitHubRemoteResolution::Other {
            remote_name,
            remote_url,
        } => Ok(unavailable_remote_status(
            "non_github_remote",
            Some(remote_name),
            Some(remote_url),
            None,
            None,
            branch_status,
            last_commit_title,
        )),
        GitHubRemoteResolution::GitHubSsh {
            remote_name,
            remote_url,
            owner,
            repo,
        } => Ok(unavailable_remote_status(
            "ssh_remote_unsupported",
            Some(remote_name),
            Some(remote_url),
            owner,
            repo,
            branch_status,
            last_commit_title,
        )),
        GitHubRemoteResolution::GitHubHttps(remote) => {
            let branch = branch_status.branch.clone();
            let mut response = GitHubRemoteStatusResponse {
                available: true,
                reason: "github_https".to_string(),
                remote_name: Some(remote.remote_name.clone()),
                remote_url: Some(remote.remote_url.clone()),
                owner: Some(remote.owner.clone()),
                repo: Some(remote.repo.clone()),
                repo_full_name: Some(format!("{}/{}", remote.owner, remote.repo)),
                branch: branch_status.branch.clone(),
                upstream: branch_status.upstream.clone(),
                ahead: branch_status.ahead,
                behind: branch_status.behind,
                published: branch_status.upstream.is_some(),
                default_branch: None,
                can_push_pull: false,
                suggested_pr_title: last_commit_title
                    .clone()
                    .or_else(|| branch_status.branch.clone()),
                last_commit_title,
                pull_request: None,
                api_error: None,
            };

            enrich_https_remote_status(db, user_id, &remote, branch.as_deref(), &mut response)
                .await?;
            Ok(response)
        }
    }
}

pub(crate) async fn resolve_github_git_command_options(
    db: &PgPool,
    user_id: Uuid,
    repository_root_path: &std::path::Path,
    remote_name: Option<&str>,
) -> Result<GitCommandOptions, ApiError> {
    let resolved_remote_name = remote_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("origin");
    match resolve_remote_url(repository_root_path, resolved_remote_name).await {
        GitHubRemoteResolution::GitHubHttps(_) => {
            let Some(authorized) = ensure_github_authorized_user(db, user_id)
                .await
                .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
            else {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "Collega GitHub a Nexus per usare push/pull su github.com",
                ));
            };

            let basic =
                BASE64_STANDARD.encode(format!("x-access-token:{}", authorized.access_token));
            Ok(GitCommandOptions {
                configs: vec![
                    (
                        "http.https://github.com/.extraheader".to_string(),
                        format!("AUTHORIZATION: basic {basic}"),
                    ),
                    ("credential.helper".to_string(), String::new()),
                ],
                env: vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())],
            })
        }
        _ => Ok(GitCommandOptions::default()),
    }
}

fn github_auth_git_options(access_token: &str) -> GitCommandOptions {
    let basic = BASE64_STANDARD.encode(format!("x-access-token:{access_token}"));
    GitCommandOptions {
        configs: vec![
            (
                "http.https://github.com/.extraheader".to_string(),
                format!("AUTHORIZATION: basic {basic}"),
            ),
            ("credential.helper".to_string(), String::new()),
        ],
        env: vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())],
    }
}

/// Costruisce un URL della API repos con i query pair `type=owner` comuni ai
/// due fallback (per-utente autenticato e endpoint pubblico per username).
fn owner_repos_url(base: &str) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base)?;
    url.query_pairs_mut()
        .append_pair("type", "owner")
        .append_pair("sort", "updated")
        .append_pair("per_page", "100");
    Ok(url)
}

/// Recupera i repository dell'utente provando in cascata: query principale
/// (visibility=all), fallback owner sull'endpoint autenticato, fallback pubblico
/// per username. In caso di triplo fallimento aggrega i tre errori.
async fn fetch_user_repos(
    access_token: &str,
    username: &str,
) -> anyhow::Result<Vec<GitHubUserRepoResponse>> {
    let mut primary_url = reqwest::Url::parse("https://api.github.com/user/repos")?;
    primary_url
        .query_pairs_mut()
        .append_pair("visibility", "all")
        .append_pair("affiliation", "owner")
        .append_pair("sort", "updated")
        .append_pair("per_page", "100");

    let primary_error = match github_api_get::<Vec<GitHubUserRepoResponse>>(
        access_token,
        primary_url.as_str(),
    )
    .await
    {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };

    let fallback_url = owner_repos_url("https://api.github.com/user/repos")?;
    let fallback_error =
        match github_api_get::<Vec<GitHubUserRepoResponse>>(access_token, fallback_url.as_str())
            .await
        {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };

    let public_fallback =
        owner_repos_url(&format!("https://api.github.com/users/{username}/repos"))?;
    github_api_get::<Vec<GitHubUserRepoResponse>>(access_token, public_fallback.as_str())
        .await
        .map_err(|public_fallback_error| {
            anyhow::anyhow!(
                "Impossibile caricare i repository GitHub (query principale: {:#}; fallback owner: {:#}; fallback users: {:#})",
                primary_error,
                fallback_error,
                public_fallback_error
            )
        })
}

async fn list_github_repositories(
    access_token: &str,
    username: &str,
) -> anyhow::Result<Vec<GitHubRepositorySummary>> {
    let repos_response = fetch_user_repos(access_token, username).await?;

    let mut repos = repos_response
        .into_iter()
        .map(|repo| GitHubRepositorySummary {
            id: repo.id,
            name: repo.name,
            full_name: repo.full_name,
            owner_login: repo.owner.login,
            html_url: repo.html_url,
            clone_url: repo.clone_url,
            private: repo.private,
            default_branch: repo.default_branch,
            updated_at: repo.updated_at,
        })
        .collect::<Vec<_>>();

    repos.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(repos)
}

/// GET /api/github/repositories — global, no project ID required
pub async fn github_list_user_repositories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus per visualizzare i repository",
            )
        })?;

    let repositories = list_github_repositories(&authorized.access_token, &authorized.username)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(json!({
        "repositories": repositories,
    })))
}

pub async fn github_list_repositories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus per visualizzare i repository",
            )
        })?;

    let repositories = list_github_repositories(&authorized.access_token, &authorized.username)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    Ok(Json(json!({
        "repositories": repositories,
    })))
}

pub async fn github_clone_repository(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitHubCloneRepositoryRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    require_git_management(&context)?;
    if context.is_git_repo {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il progetto contiene gia' un repository Git",
        ));
    }

    let clone_url = body.clone_url.trim();
    if clone_url.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "L'URL clone del repository e' obbligatorio",
        ));
    }
    let remote = match parse_github_remote_url("origin", clone_url) {
        GitHubRemoteResolution::GitHubHttps(remote) => remote,
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "Sono supportati solo repository GitHub HTTPS",
            ))
        }
    };
    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus per clonare repository privati o protetti",
            )
        })?;

    // If the current project directory is not empty, clone into a new subdirectory
    // inside projects_base_root and register it as a separate project.
    let is_empty = match tokio::fs::read_dir(&context.repository_root_path).await {
        Ok(mut entries) => entries.next_entry().await.ok().flatten().is_none(),
        Err(_) => false,
    };

    if !is_empty {
        return clone_into_new_project(state, claims, &authorized.access_token, &remote).await;
    }

    clone_in_place(
        &state,
        user_id,
        project_id,
        context,
        &authorized.access_token,
        &remote,
    )
    .await
}

/// Percorso "directory non vuota": clona il repository in una nuova sottocartella
/// dentro `projects_base_root` (nome derivato dal repo, ripulito) e lo registra
/// come progetto separato, ritornando il progetto appena creato.
async fn clone_into_new_project(
    state: AppState,
    claims: Claims,
    access_token: &str,
    remote: &GitHubHttpsRemote,
) -> ApiResult {
    // Derive a clean directory name from the repo name
    let dir_name = remote
        .repo
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect::<String>();
    let dir_name = if dir_name.is_empty() {
        "repo".to_string()
    } else {
        dir_name
    };

    let base_root = crate::projects::load_projects_base_root(&state.db).await?;
    let dest = base_root.join(&dir_name);

    if !tokio::fs::try_exists(&dest).await.unwrap_or(false) {
        tokio::fs::create_dir_all(&dest)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let git_options = github_auth_git_options(access_token);
    let clone_result = run_git_command_with_options(
        &dest,
        &["clone", remote.remote_url.as_str(), "."],
        &git_options,
    )
    .await;

    if let Err(error) = clone_result {
        return Err(api_error(StatusCode::BAD_REQUEST, error.to_string()));
    }

    // Register the new project and return it
    let register_body = crate::projects::RegisterProjectRequest {
        absolute_path: dest.to_string_lossy().to_string(),
        name: Some(remote.repo.clone()),
    };
    crate::projects::register_project(
        State(state),
        Extension(claims),
        axum::Json(register_body),
    )
    .await
}

/// Percorso "directory vuota": clona il repository nella root del progetto,
/// aggiorna la riga `repositories`, registra l'operazione git e ritorna lo
/// snapshot aggiornato. Su fallita clonazione registra l'errore e ritorna 400.
async fn clone_in_place(
    state: &AppState,
    user_id: Uuid,
    project_id: Uuid,
    context: ProjectContext,
    access_token: &str,
    remote: &GitHubHttpsRemote,
) -> ApiResult {
    let remote_owner = &remote.owner;
    let remote_repo = &remote.repo;
    let remote_clone_url = &remote.remote_url;

    let git_options = github_auth_git_options(access_token);
    let clone_result = run_git_command_with_options(
        &context.repository_root_path,
        &["clone", remote_clone_url.as_str(), "."],
        &git_options,
    )
    .await;

    let (stdout, stderr) = match clone_result {
        Ok(value) => value,
        Err(error) => {
            crate::projects::record_git_operation(
                &state.db,
                user_id,
                &context,
                "clone",
                "error",
                "",
                &error.to_string(),
                json!({
                    "cloneUrl": remote_clone_url,
                    "owner": remote_owner,
                    "repo": remote_repo,
                }),
            )
            .await;
            return Err(api_error(StatusCode::BAD_REQUEST, error.to_string()));
        }
    };

    let branch = run_git_command(
        &context.repository_root_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .await
    .ok()
    .map(|(out, _)| out.trim().to_string())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "main".to_string());

    let repository_root = context.repository_root_path.to_string_lossy().to_string();
    sqlx::query(
        r#"
        UPDATE repositories
        SET is_git_repo = TRUE,
            root_path = $2,
            current_branch = $3
        WHERE project_id = $1
        "#,
    )
    .bind(project_id)
    .bind(&repository_root)
    .bind(&branch)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let refreshed_context = load_project_context(&state.db, project_id, user_id).await?;
    crate::projects::record_git_operation(
        &state.db,
        user_id,
        &refreshed_context,
        "clone",
        "success",
        &stdout,
        &stderr,
        json!({
            "cloneUrl": remote_clone_url,
            "owner": remote_owner,
            "repo": remote_repo,
            "branch": branch,
        }),
    )
    .await;

    let git_state = refresh_git_snapshot(&state.db, &refreshed_context).await?;
    Ok(Json(json!({
        "ok": true,
        "repository": {
            "owner": remote_owner,
            "repo": remote_repo,
            "cloneUrl": remote_clone_url,
        },
        "git": git_state
    })))
}

pub async fn github_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let summary = github_account_summary(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "account": summary })))
}

pub async fn github_connect(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<GitHubConnectRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let url = build_github_oauth_url(
        &state.db,
        "connect_github",
        Some(user_id),
        body.return_to.as_deref(),
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "url": url })))
}

pub async fn github_disconnect(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    disconnect_github_connection(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn github_project_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let status = build_remote_status(&state.db, user_id, &context)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({ "github": status })))
}

/// Verifica che il chiamante possa gestire git sul progetto (403 altrimenti).
fn require_git_management(context: &ProjectContext) -> Result<(), ApiError> {
    if context.access.can_manage_git {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ))
    }
}

/// Calcola lo stato del remote e verifica che sia un remote GitHub HTTPS
/// (`reason == "github_https"`); altrimenti ritorna 400 con `unsupported_msg`.
async fn github_https_status_or_error(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
    unsupported_msg: &'static str,
) -> Result<GitHubRemoteStatusResponse, ApiError> {
    let status = build_remote_status(db, user_id, context)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    if status.reason != "github_https" {
        return Err(api_error(StatusCode::BAD_REQUEST, unsupported_msg));
    }
    Ok(status)
}

pub async fn github_publish_branch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    require_git_management(&context)?;
    if !context.is_git_repo {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il progetto selezionato non e' un repository Git",
        ));
    }

    let status = github_https_status_or_error(
        &state.db,
        user_id,
        &context,
        "Publish branch supporta solo remote https://github.com",
    )
    .await?;
    let branch = status
        .branch
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Branch corrente non disponibile"))?;
    let remote_name = status
        .remote_name
        .clone()
        .unwrap_or_else(|| "origin".to_string());
    let git_options = resolve_github_git_command_options(
        &state.db,
        user_id,
        &context.repository_root_path,
        Some(remote_name.as_str()),
    )
    .await?;

    let (stdout, stderr) = run_git_command_with_options(
        &context.repository_root_path,
        &[
            "push",
            "--set-upstream",
            remote_name.as_str(),
            branch.as_str(),
        ],
        &git_options,
    )
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    crate::projects::record_git_operation(
        &state.db,
        user_id,
        &context,
        "publish_branch",
        "success",
        &stdout,
        &stderr,
        json!({ "branch": branch, "remote": remote_name }),
    )
    .await;
    let git_state = refresh_git_snapshot(&state.db, &context).await?;
    Ok(Json(json!({ "ok": true, "git": git_state })))
}

pub async fn github_create_pull_request(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitHubCreatePullRequestRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus prima di creare una pull request",
            )
        })?;

    let status = github_https_status_or_error(
        &state.db,
        user_id,
        &context,
        "Create PR supporta solo remote https://github.com",
    )
    .await?;

    if let Some(existing) = &status.pull_request {
        return Ok(Json(json!({ "created": false, "pullRequest": existing })));
    }

    submit_pull_request(&authorized.access_token, &status, body).await
}

/// Deriva i parametri della pull request dallo stato del remote e dal body
/// (owner/repo/head/base/title, con default sensati) e la apre via API GitHub.
async fn submit_pull_request(
    access_token: &str,
    status: &GitHubRemoteStatusResponse,
    body: GitHubCreatePullRequestRequest,
) -> ApiResult {
    let owner = status
        .owner
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Owner GitHub non disponibile"))?;
    let repo = status
        .repo
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Repository GitHub non disponibile"))?;
    let head_branch = status
        .branch
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Branch corrente non disponibile"))?;

    let base_branch = body
        .base_branch
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or(status.default_branch.clone())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Branch base non disponibile"))?;
    let title = if body.title.trim().is_empty() {
        status
            .suggested_pr_title
            .clone()
            .unwrap_or_else(|| format!("Merge {head_branch}"))
    } else {
        body.title.trim().to_string()
    };

    let pr = github_api_post::<GitHubPullRequestResponse>(
        access_token,
        &format!("https://api.github.com/repos/{owner}/{repo}/pulls"),
        json!({
            "title": title,
            "head": head_branch,
            "base": base_branch,
            "body": body.body.unwrap_or_default(),
        }),
    )
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(json!({
        "created": true,
        "pullRequest": {
            "number": pr.number,
            "htmlUrl": pr.html_url,
            "title": pr.title,
            "state": pr.state
        }
    })))
}

/// Valida il nome repository: solo alfanumerico e i separatori ammessi da GitHub
/// (`-`, `_`, `.`). Ritorna 400 se contiene caratteri non consentiti.
fn validate_repo_name(name: &str) -> Result<(), ApiError> {
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome repository non valido (solo alfanumerico, '-', '_', '.')",
        ))
    }
}

/// Estrae un campo stringa dalla risposta GitHub, con default se assente/non stringa.
fn github_repo_field(body: &serde_json::Value, key: &str, default: &str) -> String {
    body.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// Costruisce l'errore standard "GitHub API <status> — <message>" dal corpo di risposta.
fn github_repo_api_error(status: reqwest::StatusCode, body: &serde_json::Value) -> ApiError {
    let msg = body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("errore sconosciuto");
    api_error(
        StatusCode::BAD_REQUEST,
        format!("GitHub API {} — {}", status, msg),
    )
}

/// Esegue `POST /user/repos` col token utente e ritorna status e corpo JSON grezzo.
/// La gestione degli status non-success e' lasciata al chiamante (create vs publish
/// hanno semantiche diverse sul conflitto 422).
async fn create_repo_via_api(
    access_token: &str,
    name: &str,
    private: bool,
    description: &str,
    auto_init: bool,
) -> Result<(reqwest::StatusCode, serde_json::Value), ApiError> {
    let client = Client::builder()
        .user_agent("nexus-mcp-core")
        .build()
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("http client: {}", e),
            )
        })?;

    let resp = client
        .post("https://api.github.com/user/repos")
        .bearer_auth(access_token)
        .header("Accept", "application/vnd.github+json")
        .json(&json!({
            "name": name,
            "private": private,
            "description": description,
            "auto_init": auto_init,
        }))
        .send()
        .await
        .map_err(|e| api_error(StatusCode::BAD_GATEWAY, format!("GitHub API call: {}", e)))?;

    let status = resp.status();
    let resp_body: serde_json::Value = resp.json().await.unwrap_or_else(|_| json!({}));
    Ok((status, resp_body))
}

/// Fix M15: POST /api/projects/:id/github/create-repo
/// Crea un nuovo repository GitHub per l'utente connesso e configura origin remote
/// nel progetto target. Risolve il flow E2E "create new project on GitHub" che oggi
/// richiede prompt manuale all'agente.
///
/// Body: `{name: string, private?: bool=true, description?: string, auto_init?: bool=false}`
/// Output: `{ok, html_url, clone_url, full_name, private, origin_configured, default_branch}`
pub async fn github_create_repo(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    require_git_management(&context)?;

    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus prima di creare un repository",
            )
        })?;

    let name = body
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'name' obbligatorio"))?;

    validate_repo_name(name)?;

    let private = body
        .get("private")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let description = body
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let auto_init = body
        .get("auto_init")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // ── Chiamata GitHub API: POST /user/repos ────────────────────────────
    let (status, resp_body) =
        create_repo_via_api(&authorized.access_token, name, private, description, auto_init)
            .await?;

    if !status.is_success() {
        return Err(github_repo_api_error(status, &resp_body));
    }

    let clone_url = github_repo_field(&resp_body, "clone_url", "");
    let html_url = github_repo_field(&resp_body, "html_url", "");
    let full_name = github_repo_field(&resp_body, "full_name", "");
    let default_branch = github_repo_field(&resp_body, "default_branch", "main");

    // ── Configura origin remote sul progetto (idempotente) ──────────────
    let mut origin_configured = false;
    if context.is_git_repo && !clone_url.is_empty() {
        // Rimuovi eventuale origin pre-esistente
        let _ = run_git_command(&context.root_path, &["remote", "remove", "origin"]).await;
        match run_git_command(&context.root_path, &["remote", "add", "origin", &clone_url]).await {
            Ok(_) => origin_configured = true,
            Err(e) => {
                tracing::warn!(
                    "github_create_repo: remote add fallito (repo creato ma origin non configurato): {}",
                    e
                );
            }
        }
    }

    Ok(Json(json!({
        "ok": true,
        "full_name": full_name,
        "html_url": html_url,
        "clone_url": clone_url,
        "private": private,
        "default_branch": default_branch,
        "origin_configured": origin_configured,
    })))
}

/// Prepara il repository git locale prima della pubblicazione: init con branch
/// `main` se manca, `.gitignore` di default se assente, config user idempotente,
/// quindi `add -A` + `commit` (il commit e' no-op se non ci sono modifiche).
async fn prepare_local_git_repo(
    root: &std::path::Path,
    commit_message: &str,
) -> Result<(), ApiError> {
    // ── 1. Init git locale se manca ──────────────────────────────────────
    let dot_git = root.join(".git");
    if !tokio::fs::try_exists(&dot_git).await.unwrap_or(false) {
        run_git_command(root, &["init", "-b", "main"])
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("git init: {e}")))?;
    }

    // .gitignore di default se manca
    let gitignore_path = root.join(".gitignore");
    if !tokio::fs::try_exists(&gitignore_path).await.unwrap_or(false) {
        let default_gitignore = "# Dependencies\nnode_modules/\n.pnpm-store/\n\n# Build output\ndist/\nbuild/\n.next/\n.turbo/\nout/\ntarget/\n\n# Environment\n.env\n.env.local\n.env.*.local\n!.env.example\n\n# Logs\n*.log\nnpm-debug.log*\npnpm-debug.log*\n\n# Editor\n.vscode/\n.idea/\n*.swp\n*.swo\n.DS_Store\n\n# Test artifacts\nplaywright-report/\ntest-results/\ncoverage/\n\n# OS\nThumbs.db\n";
        let _ = tokio::fs::write(&gitignore_path, default_gitignore).await;
    }

    // Configura user.email/name locali (idempotente) per consentire git commit
    let _ = run_git_command(root, &["config", "user.email", "nexus@local"]).await;
    let _ = run_git_command(root, &["config", "user.name", "Nexus"]).await;

    // ── 2. add + commit (no-op se nulla da committare) ───────────────────
    let _ = run_git_command(root, &["add", "-A"]).await;
    // git commit ritorna errore se nulla da committare: ignoriamo (e' lecito ripubblicare)
    let _ = run_git_command(root, &["commit", "-m", commit_message]).await;
    Ok(())
}

/// Crea il repository su GitHub, oppure riusa quello esistente se GitHub risponde
/// 422 "already exists" (via GET /repos/:owner/:repo). Ritorna il corpo JSON del
/// repository (creato o riusato). Propaga 400 se il repo esiste ma non e' accessibile.
async fn create_or_reuse_github_repo(
    access_token: &str,
    username: &str,
    name: &str,
    private: bool,
    description: &str,
) -> Result<serde_json::Value, ApiError> {
    let (status, resp_body) =
        create_repo_via_api(access_token, name, private, description, false).await?;

    // Idempotenza: se il repo esiste gia' (422 "name already exists on this
    // account"), riusiamo quello esistente facendo GET /repos/:owner/:repo,
    // cosi' il flusso "Pubblica su GitHub" funziona anche per
    // ripubblicazioni e progetti gia' creati ma senza origin/push.
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        let already_exists = resp_body
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .map(|errs| {
                errs.iter().any(|e| {
                    e.get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(|m| m.contains("already exists"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if already_exists {
            return lookup_existing_repo(access_token, username, name).await;
        }
    }

    if !status.is_success() {
        return Err(github_repo_api_error(status, &resp_body));
    }
    Ok(resp_body)
}

/// Recupera il repository esistente `owner/name` via GET /repos. Ritorna 400 se
/// il token non ha accesso (404/403), altrimenti il corpo JSON del repository.
async fn lookup_existing_repo(
    access_token: &str,
    owner: &str,
    name: &str,
) -> Result<serde_json::Value, ApiError> {
    let client = Client::builder()
        .user_agent("nexus-mcp-core")
        .build()
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("http client: {}", e),
            )
        })?;
    let lookup = client
        .get(format!("https://api.github.com/repos/{}/{}", owner, name))
        .bearer_auth(access_token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| api_error(StatusCode::BAD_GATEWAY, format!("GitHub lookup: {}", e)))?;
    if !lookup.status().is_success() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Il repository {}/{} esiste su GitHub ma non e' accessibile col tuo token (404/403). Verifica i permessi del token.",
                owner, name
            ),
        ));
    }
    Ok(lookup
        .json::<serde_json::Value>()
        .await
        .unwrap_or_else(|_| json!({})))
}

/// POST /api/projects/:id/github/publish
///
/// Orchestrazione completa "pubblica progetto su GitHub":
/// 1. (se manca) git init -b main + .gitignore default
/// 2. git add -A + git commit (se ci sono modifiche staged)
/// 3. Crea repository GitHub via API (riusa github_create_repo)
/// 4. git remote add origin (idempotente)
/// 5. git push -u origin main (con token GitHub iniettato)
///
/// Body: `{name, description?, private?, commit_message?}`
/// Output: `{ok, html_url, clone_url, full_name, pushed: bool}`
pub async fn github_publish_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    require_git_management(&context)?;

    let authorized = ensure_github_authorized_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Collega GitHub a Nexus prima di pubblicare",
            )
        })?;

    let name = body
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'name' obbligatorio"))?
        .to_string();

    validate_repo_name(&name)?;

    let private = body
        .get("private")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let description = body
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let commit_message = body
        .get("commit_message")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Initial commit (Nexus)")
        .to_string();

    let root = context.root_path.clone();

    // ── 1-2. Prepara repo git locale (init, .gitignore, config, add+commit) ─
    prepare_local_git_repo(&root, &commit_message).await?;

    // ── 3. Crea repo su GitHub (o riusa quello esistente su 422) ─────────
    let resp_body = create_or_reuse_github_repo(
        &authorized.access_token,
        &authorized.username,
        &name,
        private,
        &description,
    )
    .await?;

    let clone_url = github_repo_field(&resp_body, "clone_url", "");
    let html_url = github_repo_field(&resp_body, "html_url", "");
    let full_name = github_repo_field(&resp_body, "full_name", "");
    let default_branch = github_repo_field(&resp_body, "default_branch", "main");

    // ── 4. Configura origin (idempotente) ────────────────────────────────
    if !clone_url.is_empty() {
        let _ = run_git_command(&root, &["remote", "remove", "origin"]).await;
        run_git_command(&root, &["remote", "add", "origin", &clone_url])
            .await
            .map_err(|e| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("remote add: {e}"),
                )
            })?;
    }

    // ── 5. Push con token iniettato ──────────────────────────────────────
    let git_options = github_auth_git_options(&authorized.access_token);
    let pushed = match run_git_command_with_options(
        &root,
        &["push", "-u", "origin", &default_branch],
        &git_options,
    )
    .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("github_publish_project: push fallito: {e}");
            false
        }
    };

    Ok(Json(json!({
        "ok": true,
        "full_name": full_name,
        "html_url": html_url,
        "clone_url": clone_url,
        "private": private,
        "default_branch": default_branch,
        "pushed": pushed,
    })))
}
