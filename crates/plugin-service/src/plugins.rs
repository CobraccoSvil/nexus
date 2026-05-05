//! Plugin management API - catalog, install, update, uninstall, toggle, test, health, tool policy.
//!
//! Endpoints:
//!   GET    /api/plugins/catalog                 -> list catalog items
//!   GET    /api/plugins/installed               -> list installed plugins
//!   POST   /api/plugins/install                 -> install a plugin from catalog
//!   PUT    /api/plugins/:id/update              -> update plugin version
//!   DELETE /api/plugins/:id                     -> uninstall plugin
//!   PUT    /api/plugins/:id/toggle              -> enable/disable
//!   POST   /api/plugins/:id/test                -> test connection + discover tools
//!   GET    /api/plugins/:id/health              -> get health history
//!   PUT    /api/plugins/:id/tool-policy         -> update tool allow/deny policy
//!   POST   /api/plugins/:id/migrate-legacy      -> migrate a legacy mcp_server
//!
//! Figma OAuth:
//!   GET    /api/plugins/figma/oauth/status      -> check OAuth config
//!   POST   /api/plugins/figma/oauth/start       -> start OAuth flow
//!   GET    /auth/figma/mcp/callback             -> OAuth callback

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use nexus_auth::{backend_url, frontend_url, get_or_create_jwt_secret, get_setting, Claims};
use nexus_types::{api_error, ensure_project_access, parse_user_id, ApiResult};

use crate::mcp_client::{self, McpServerConfig, McpTransport};
use crate::AppState;

// -- Request types --

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPluginRequest {
    pub catalog_item_id: Option<String>,
    pub slug: Option<String>,
    pub version: Option<String>,
    pub scope: Option<String>,
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub config: Option<Value>,
    pub secret_bindings: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct UpdatePluginRequest {
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TogglePluginRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateToolPolicyRequest {
    pub mode: String,
    pub tools: Option<Vec<String>>,
    pub blocked_tools: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FigmaOAuthStartRequest {
    pub return_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FigmaOAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FigmaOAuthStateClaims {
    user_id: String,
    return_to: String,
    exp: usize,
}

#[derive(Debug, Deserialize)]
struct FigmaOAuthDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct FigmaOAuthTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

// -- Internal types --

#[derive(Debug, Clone)]
struct CatalogConfig {
    id: Uuid,
    slug: String,
    name: String,
    description: String,
    transport: String,
    http_url: Option<String>,
    stdio_command: Option<String>,
    stdio_args: Value,
    required_secret_refs: Value,
    #[allow(dead_code)]
    optional_secret_refs: Value,
    default_scope: String,
    allowed_commands: Value,
    default_tool_policy: Value,
    release_id: Option<Uuid>,
    release_version: Option<String>,
}

const FIGMA_DEFAULT_RETURN_TO: &str = "/admin/settings/connectors";
const FIGMA_DISCOVERY_URL: &str = "https://api.figma.com/.well-known/oauth-authorization-server";

// -- Helpers --

fn normalize_scope(raw: Option<&str>) -> Result<String, (StatusCode, Json<Value>)> {
    let scope = raw.unwrap_or("global").trim().to_lowercase();
    match scope.as_str() {
        "global" | "project" | "user" => Ok(scope),
        _ => Err(api_error(
            StatusCode::BAD_REQUEST,
            "Scope non valido: usa global, project o user",
        )),
    }
}

fn parse_string_array(raw: &Value) -> Vec<String> {
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn get_json_object<'a>(
    raw: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    raw.get(field).and_then(Value::as_object)
}

fn value_to_string_map(raw: Option<&serde_json::Map<String, Value>>) -> HashMap<String, String> {
    raw.map(|obj| {
        obj.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<HashMap<_, _>>()
    })
    .unwrap_or_default()
}

fn can_manage_instance(row: &sqlx::postgres::PgRow, user_id: Uuid, role: &str) -> bool {
    let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".to_string());
    let owner: Option<Uuid> = row.try_get("installed_by_user_id").unwrap_or(None);
    owner == Some(user_id) || (scope == "global" && role == "admin")
}

fn format_compact_error(message: &str) -> String {
    let compact = message
        .replace('\n', " ")
        .replace('\r', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.chars().count() > 300 {
        format!("{}...", compact.chars().take(300).collect::<String>())
    } else {
        compact
    }
}

fn sanitize_return_to(value: Option<&str>) -> String {
    let raw = value.unwrap_or(FIGMA_DEFAULT_RETURN_TO).trim();
    if raw.starts_with('/') && !raw.starts_with("//") {
        raw.to_string()
    } else {
        FIGMA_DEFAULT_RETURN_TO.to_string()
    }
}

fn redirect_with_status(return_to: &str, status: &str, message: Option<&str>) -> Response {
    let mut target = format!("{}{}", frontend_url(), sanitize_return_to(Some(return_to)));
    let separator = if target.contains('?') { '&' } else { '?' };
    target.push(separator);
    target.push_str("figmaOauth=");
    target.push_str(&urlencoding::encode(status));
    if let Some(message) = message {
        target.push_str("&figmaMessage=");
        target.push_str(&urlencoding::encode(message));
    }
    Redirect::temporary(&target).into_response()
}

#[allow(dead_code)]
fn parse_json_string_set(raw: &Value) -> HashSet<String> {
    raw.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default()
}

fn is_figma_pat(token: &str) -> bool {
    token.trim().to_lowercase().starts_with("figd_")
}

async fn upsert_setting_value(
    db: &PgPool,
    key: &str,
    value: &str,
    category: &str,
    description: &str,
    is_secret: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            category = EXCLUDED.category,
            description = EXCLUDED.description,
            is_secret = EXCLUDED.is_secret,
            updated_at = NOW()
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(category)
    .bind(description)
    .bind(is_secret)
    .execute(db)
    .await
    .map(|_| ())
}

async fn resolve_bool_setting(db: &PgPool, key: &str, default_value: bool) -> bool {
    match get_setting(db, key).await {
        Some(value) => {
            let normalized = value.trim().to_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        }
        None => default_value,
    }
}

async fn resolve_secret_value(db: &PgPool, setting_key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(setting_key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn write_plugin_audit(
    db: &PgPool,
    plugin_instance_id: Option<Uuid>,
    user_id: Option<Uuid>,
    project_id: Option<Uuid>,
    action: &str,
    status: &str,
    message: Option<String>,
    payload: Value,
) {
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_audit_events
            (plugin_instance_id, user_id, project_id, action, status, message, payload)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .bind(project_id)
    .bind(action)
    .bind(status)
    .bind(message)
    .bind(payload)
    .execute(db)
    .await;
}

async fn find_duplicate_instance_anywhere(
    db: &PgPool,
    catalog_item_id: Uuid,
    catalog_slug: &str,
) -> Result<Option<(Uuid, String, Option<Uuid>, Option<Uuid>)>, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        r#"
        SELECT pi.id, pi.scope, pi.project_id, pi.installed_by_user_id
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        WHERE pi.catalog_item_id = $1 OR LOWER(c.slug) = LOWER($2)
        ORDER BY pi.updated_at DESC NULLS LAST, pi.created_at DESC, pi.id DESC
        LIMIT 1
        "#,
    )
    .bind(catalog_item_id)
    .bind(catalog_slug)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(row.map(|r| {
        (
            r.try_get::<Uuid, _>("id").unwrap_or(Uuid::nil()),
            r.try_get::<String, _>("scope")
                .unwrap_or_else(|_| "global".to_string()),
            r.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None),
            r.try_get::<Option<Uuid>, _>("installed_by_user_id")
                .unwrap_or(None),
        )
    }))
}

async fn get_catalog_by_install_request(
    db: &PgPool,
    body: &InstallPluginRequest,
) -> Result<CatalogConfig, (StatusCode, Json<Value>)> {
    let row = if let Some(id_raw) = body.catalog_item_id.as_deref() {
        let catalog_id = Uuid::parse_str(id_raw)
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "catalogItemId non valido"))?;
        sqlx::query(
            r#"
            SELECT c.id, c.slug, c.name, c.description, c.transport, c.http_url, c.stdio_command,
                c.stdio_args, c.required_secret_refs, c.optional_secret_refs, c.default_scope,
                c.allowed_commands, c.default_tool_policy, c.is_allowlisted, c.enabled,
                r.id AS release_id, r.version AS release_version
            FROM plugin_catalog_items c
            LEFT JOIN LATERAL (
                SELECT id, version FROM plugin_releases
                WHERE catalog_item_id = c.id AND is_stable = true
                ORDER BY created_at DESC LIMIT 1
            ) r ON TRUE
            WHERE c.id = $1
            "#,
        )
        .bind(catalog_id)
        .fetch_optional(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        let slug = body
            .slug
            .as_deref()
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "slug o catalogItemId richiesto"))?;
        sqlx::query(
            r#"
            SELECT c.id, c.slug, c.name, c.description, c.transport, c.http_url, c.stdio_command,
                c.stdio_args, c.required_secret_refs, c.optional_secret_refs, c.default_scope,
                c.allowed_commands, c.default_tool_policy, c.is_allowlisted, c.enabled,
                r.id AS release_id, r.version AS release_version
            FROM plugin_catalog_items c
            LEFT JOIN LATERAL (
                SELECT id, version FROM plugin_releases
                WHERE catalog_item_id = c.id AND is_stable = true
                ORDER BY created_at DESC LIMIT 1
            ) r ON TRUE
            WHERE c.slug = $1
            "#,
        )
        .bind(slug)
        .fetch_optional(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Plugin catalog item non trovato"));
    };

    let is_allowlisted: bool = row.try_get("is_allowlisted").unwrap_or(false);
    let enabled: bool = row.try_get("enabled").unwrap_or(false);
    if !enabled || !is_allowlisted {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Plugin non installabile: non presente nella allowlist",
        ));
    }

    Ok(CatalogConfig {
        id: row.try_get("id").unwrap_or(Uuid::nil()),
        slug: row.try_get("slug").unwrap_or_default(),
        name: row.try_get("name").unwrap_or_default(),
        description: row.try_get("description").unwrap_or_default(),
        transport: row.try_get("transport").unwrap_or_else(|_| "http".to_string()),
        http_url: row.try_get("http_url").unwrap_or(None),
        stdio_command: row.try_get("stdio_command").unwrap_or(None),
        stdio_args: row.try_get("stdio_args").unwrap_or(json!([])),
        required_secret_refs: row.try_get("required_secret_refs").unwrap_or(json!([])),
        optional_secret_refs: row.try_get("optional_secret_refs").unwrap_or(json!([])),
        default_scope: row.try_get("default_scope").unwrap_or_else(|_| "global".to_string()),
        allowed_commands: row.try_get("allowed_commands").unwrap_or(json!([])),
        default_tool_policy: row
            .try_get("default_tool_policy")
            .unwrap_or(json!({"mode":"allowlist","tools":[],"blockedTools":[]})),
        release_id: row.try_get("release_id").unwrap_or(None),
        release_version: row.try_get("release_version").unwrap_or(None),
    })
}

async fn resolve_plugin_instance_for_user(
    db: &PgPool,
    plugin_instance_id: Uuid,
    user_id: Uuid,
) -> Result<sqlx::postgres::PgRow, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        r#"
        SELECT
            pi.id, pi.catalog_item_id, pi.release_id, pi.installed_by_user_id,
            pi.project_id, pi.scope, pi.name, pi.enabled, pi.config, pi.secret_bindings,
            pi.health_status, pi.last_health_message, pi.last_tested_at,
            c.slug, c.transport, c.http_url, c.stdio_command, c.stdio_args,
            ms.id AS mcp_server_id, ms.name AS mcp_server_name,
            ms.transport AS mcp_transport, ms.url, ms.command, ms.args, ms.headers, ms.env_vars
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        LEFT JOIN LATERAL (
            SELECT id, name, transport, url, command, args, headers, env_vars
            FROM mcp_servers WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC
            LIMIT 1
        ) ms ON TRUE
        WHERE pi.id = $1
          AND (pi.scope = 'global' OR pi.installed_by_user_id = $2
               OR (pi.scope = 'project' AND EXISTS (
                   SELECT 1 FROM projects p
                   LEFT JOIN project_members pm ON pm.project_id = p.id AND pm.user_id = $2
                   WHERE p.id = pi.project_id AND (p.owner_user_id = $2 OR pm.user_id IS NOT NULL)
               )))
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    row.ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Plugin installato non trovato"))
}

async fn resolve_plugin_runtime_config(
    db: &PgPool,
    mcp_server_row: &sqlx::postgres::PgRow,
    secret_bindings: &Value,
) -> McpServerConfig {
    let mcp_server_id: Uuid = mcp_server_row.try_get("mcp_server_id").unwrap_or(Uuid::nil());
    let mcp_server_name: String = mcp_server_row
        .try_get("mcp_server_name")
        .unwrap_or_else(|_| "Plugin MCP".to_string());
    let transport: String = mcp_server_row
        .try_get("transport")
        .unwrap_or_else(|_| "http".to_string());
    let url: Option<String> = mcp_server_row.try_get("url").unwrap_or(None);
    let command: Option<String> = mcp_server_row.try_get("command").unwrap_or(None);
    let args: Value = mcp_server_row.try_get("args").unwrap_or(json!([]));
    let static_headers: Value = mcp_server_row.try_get("headers").unwrap_or(json!({}));
    let static_env: Value = mcp_server_row.try_get("env_vars").unwrap_or(json!({}));
    let plugin_slug: String = mcp_server_row.try_get("slug").unwrap_or_default();

    let mut headers = value_to_string_map(static_headers.as_object());
    let mut env_vars = value_to_string_map(static_env.as_object());
    let mut figma_token = resolve_secret_value(db, "figma_oauth_token").await;

    if let Some(bindings_headers) = get_json_object(secret_bindings, "headers") {
        for (header_name, setting_key_raw) in bindings_headers {
            if let Some(setting_key) = setting_key_raw.as_str() {
                if let Some(secret) = resolve_secret_value(db, setting_key).await {
                    if header_name.eq_ignore_ascii_case("authorization") {
                        if secret.to_lowercase().starts_with("bearer ") {
                            headers.insert(header_name.clone(), secret);
                        } else {
                            headers.insert(header_name.clone(), format!("Bearer {secret}"));
                        }
                    } else {
                        headers.insert(header_name.clone(), secret);
                    }
                }
            }
        }
    }

    if let Some(bindings_env) = get_json_object(secret_bindings, "envVars") {
        for (env_name, setting_key_raw) in bindings_env {
            if let Some(setting_key) = setting_key_raw.as_str() {
                if let Some(secret) = resolve_secret_value(db, setting_key).await {
                    env_vars.insert(env_name.clone(), secret);
                }
            }
        }
    }

    // Figma compatibility
    if plugin_slug.eq_ignore_ascii_case("figma-http") {
        let has_auth = headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
        let has_figma_tok = headers.keys().any(|k| k.eq_ignore_ascii_case("x-figma-token"));
        let has_figma_reg = headers.keys().any(|k| k.eq_ignore_ascii_case("x-figma-region"));

        if let Some(secret) = figma_token.clone() {
            if !has_figma_tok {
                headers.insert("X-Figma-Token".to_string(), secret.clone());
            }
            if !has_auth {
                if secret.to_lowercase().starts_with("bearer ") {
                    headers.insert("Authorization".to_string(), secret);
                } else {
                    headers.insert("Authorization".to_string(), format!("Bearer {secret}"));
                }
            }
        }
        if !has_figma_reg {
            if let Some(region) = resolve_secret_value(db, "figma_region").await {
                headers.insert("X-Figma-Region".to_string(), region);
            }
        }
    }

    let prefer_figma_stdio = plugin_slug.eq_ignore_ascii_case("figma-http")
        && resolve_bool_setting(db, "figma_mcp_prefer_stdio", true).await;

    let transport_cfg = if prefer_figma_stdio {
        if figma_token.is_none() {
            figma_token = resolve_secret_value(db, "figma_oauth_token").await;
        }
        if let Some(token) = figma_token {
            env_vars.insert("FIGMA_API_KEY".to_string(), token.clone());
            env_vars.insert("FIGMA_OAUTH_TOKEN".to_string(), token);
        }
        McpTransport::Stdio {
            command: "npx".to_string(),
            args: vec!["-y".into(), "figma-developer-mcp".into(), "--stdio".into(), "--json".into()],
            env_vars,
        }
    } else if transport == "stdio" {
        McpTransport::Stdio {
            command: command.unwrap_or_default(),
            args: parse_string_array(&args),
            env_vars,
        }
    } else {
        McpTransport::Http {
            url: url.unwrap_or_default(),
            headers,
        }
    };

    McpServerConfig {
        id: mcp_server_id.to_string(),
        name: mcp_server_name,
        transport: transport_cfg,
        enabled: true,
    }
}

struct PluginResolution {
    #[allow(dead_code)]
    plugin_instance_id: Uuid,
    mcp_server_id: Uuid,
    mcp_server_name: String,
    plugin_slug: String,
    config: McpServerConfig,
}

async fn build_plugin_resolution(
    db: &PgPool,
    plugin_instance_id: Uuid,
    user_id: Uuid,
) -> Result<PluginResolution, (StatusCode, Json<Value>)> {
    let row = resolve_plugin_instance_for_user(db, plugin_instance_id, user_id).await?;
    let mcp_server_id: Uuid = row
        .try_get("mcp_server_id")
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin senza adapter MCP collegato"))?;
    let secret_bindings: Value = row.try_get("secret_bindings").unwrap_or(json!({}));
    let mcp_server_name: String = row.try_get("mcp_server_name").unwrap_or_else(|_| {
        row.try_get::<String, _>("name").unwrap_or_else(|_| "Plugin MCP".to_string())
    });
    let plugin_slug: String = row.try_get("slug").unwrap_or_default();
    let config = resolve_plugin_runtime_config(db, &row, &secret_bindings).await;

    Ok(PluginResolution {
        plugin_instance_id,
        mcp_server_id,
        mcp_server_name,
        plugin_slug,
        config,
    })
}

fn figma_oauth_redirect_uri() -> String {
    format!("{}/auth/figma/mcp/callback", backend_url())
}

async fn figma_oauth_client_credentials(
    db: &PgPool,
) -> Result<(String, String, String), (StatusCode, Json<Value>)> {
    let client_id = get_setting(db, "figma_client_id").await.ok_or_else(|| {
        api_error(StatusCode::BAD_REQUEST, "figma_client_id non configurato")
    })?;
    let client_secret = get_setting(db, "figma_client_secret").await.ok_or_else(|| {
        api_error(StatusCode::BAD_REQUEST, "figma_client_secret non configurato")
    })?;
    let redirect_uri = get_setting(db, "figma_oauth_redirect_uri")
        .await
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(figma_oauth_redirect_uri);
    Ok((client_id, client_secret, redirect_uri))
}

async fn fetch_figma_oauth_discovery() -> Result<FigmaOAuthDiscovery, String> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());
    let response = client.get(FIGMA_DISCOVERY_URL).send().await
        .map_err(|e| format!("Discovery OAuth Figma non raggiungibile: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Discovery OAuth Figma HTTP {}", response.status().as_u16()));
    }
    response.json::<FigmaOAuthDiscovery>().await
        .map_err(|e| format!("Discovery OAuth Figma non valida: {e}"))
}

async fn store_figma_oauth_error(db: &PgPool, message: &str) {
    let _ = upsert_setting_value(db, "figma_last_oauth_error", message, "connectors", "Ultimo errore OAuth Figma", false).await;
}

fn detect_legacy_catalog_slug(transport: &str, url: Option<&str>, command: Option<&str>, args: &Value) -> Option<&'static str> {
    let transport = transport.trim().to_lowercase();
    if transport == "http" {
        let url = url.unwrap_or_default().to_lowercase();
        if url.contains("mcp.figma.com/mcp") { return Some("figma-http"); }
        if url.contains("api.githubcopilot.com/mcp") { return Some("github-http"); }
        return None;
    }
    if transport == "stdio" {
        let command = command.unwrap_or_default().trim().to_lowercase();
        let args_list: Vec<String> = args.as_array()
            .map(|items| items.iter().filter_map(Value::as_str).map(|s| s.to_lowercase()).collect())
            .unwrap_or_default();
        if command == "npx" && args_list.iter().any(|i| i.contains("@modelcontextprotocol/server-filesystem")) {
            return Some("filesystem-local");
        }
        if command == "npx" && args_list.iter().any(|i| i.contains("@playwright/mcp")) {
            return Some("playwright-stdio");
        }
    }
    None
}

// ==========================================================================
// Public Handlers
// ==========================================================================

/// GET /api/plugins/catalog
pub async fn list_plugin_catalog(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT c.id, c.slug, c.name, c.description, c.plugin_type, c.transport,
            c.http_url, c.stdio_command, c.stdio_args, c.required_secret_refs,
            c.optional_secret_refs, c.default_scope, c.allowed_commands,
            c.default_tool_policy, c.metadata, c.is_allowlisted, c.enabled,
            COALESCE(json_agg(json_build_object(
                'id', r.id, 'version', r.version, 'changelog', r.changelog,
                'isStable', r.is_stable, 'createdAt', r.created_at
            ) ORDER BY r.created_at DESC) FILTER (WHERE r.id IS NOT NULL), '[]'::json) AS releases
        FROM plugin_catalog_items c
        LEFT JOIN plugin_releases r ON r.catalog_item_id = c.id
        WHERE c.enabled = TRUE
        GROUP BY c.id ORDER BY c.name
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<Value> = rows.iter().map(|row| json!({
        "id": row.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
        "slug": row.try_get::<String, _>("slug").unwrap_or_default(),
        "name": row.try_get::<String, _>("name").unwrap_or_default(),
        "description": row.try_get::<String, _>("description").unwrap_or_default(),
        "pluginType": row.try_get::<String, _>("plugin_type").unwrap_or_else(|_| "mcp".into()),
        "transport": row.try_get::<String, _>("transport").unwrap_or_else(|_| "http".into()),
        "httpUrl": row.try_get::<Option<String>, _>("http_url").unwrap_or(None),
        "stdioCommand": row.try_get::<Option<String>, _>("stdio_command").unwrap_or(None),
        "stdioArgs": row.try_get::<Value, _>("stdio_args").unwrap_or(json!([])),
        "requiredSecretRefs": row.try_get::<Value, _>("required_secret_refs").unwrap_or(json!([])),
        "optionalSecretRefs": row.try_get::<Value, _>("optional_secret_refs").unwrap_or(json!([])),
        "defaultScope": row.try_get::<String, _>("default_scope").unwrap_or_else(|_| "global".into()),
        "allowedCommands": row.try_get::<Value, _>("allowed_commands").unwrap_or(json!([])),
        "defaultToolPolicy": row.try_get::<Value, _>("default_tool_policy").unwrap_or(json!({})),
        "metadata": row.try_get::<Value, _>("metadata").unwrap_or(json!({})),
        "isAllowlisted": row.try_get::<bool, _>("is_allowlisted").unwrap_or(false),
        "enabled": row.try_get::<bool, _>("enabled").unwrap_or(true),
        "releases": row.try_get::<Value, _>("releases").unwrap_or(json!([])),
    })).collect();

    Ok(Json(json!({ "items": items })))
}

/// GET /api/plugins/installed
pub async fn list_installed_plugins(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let rows = sqlx::query(
        r#"
        SELECT pi.id, pi.catalog_item_id, pi.release_id, pi.installed_by_user_id,
            pi.project_id, pi.scope, pi.name, pi.enabled, pi.config, pi.secret_bindings,
            pi.health_status, pi.last_health_message, pi.last_tested_at,
            pi.created_at, pi.updated_at,
            c.slug, c.name AS catalog_name, c.description AS catalog_description, c.transport,
            pr.version,
            ms.id AS mcp_server_id,
            pol.mode AS policy_mode, pol.tools AS policy_tools, pol.blocked_tools AS policy_blocked_tools
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        LEFT JOIN plugin_releases pr ON pr.id = pi.release_id
        LEFT JOIN LATERAL (
            SELECT id FROM mcp_servers WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC LIMIT 1
        ) ms ON TRUE
        LEFT JOIN LATERAL (
            SELECT mode, tools, blocked_tools FROM plugin_instance_tool_policies
            WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, id DESC LIMIT 1
        ) pol ON TRUE
        WHERE pi.scope = 'global' OR pi.installed_by_user_id = $1
            OR (pi.scope = 'project' AND EXISTS (
                SELECT 1 FROM projects p LEFT JOIN project_members pm ON pm.project_id = p.id AND pm.user_id = $1
                WHERE p.id = pi.project_id AND (p.owner_user_id = $1 OR pm.user_id IS NOT NULL)))
        ORDER BY pi.created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<Value> = rows.iter().map(|row| {
        let sb = row.try_get::<Value, _>("secret_bindings").unwrap_or(json!({}));
        let has_sb = sb.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        json!({
            "id": row.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
            "catalogItemId": row.try_get::<Uuid, _>("catalog_item_id").ok().map(|v| v.to_string()),
            "releaseId": row.try_get::<Option<Uuid>, _>("release_id").unwrap_or(None).map(|v| v.to_string()),
            "version": row.try_get::<Option<String>, _>("version").unwrap_or(None),
            "slug": row.try_get::<String, _>("slug").unwrap_or_default(),
            "catalogName": row.try_get::<String, _>("catalog_name").unwrap_or_default(),
            "catalogDescription": row.try_get::<String, _>("catalog_description").unwrap_or_default(),
            "transport": row.try_get::<String, _>("transport").unwrap_or_else(|_| "http".into()),
            "scope": row.try_get::<String, _>("scope").unwrap_or_else(|_| "global".into()),
            "projectId": row.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None).map(|v| v.to_string()),
            "name": row.try_get::<String, _>("name").unwrap_or_default(),
            "enabled": row.try_get::<bool, _>("enabled").unwrap_or(true),
            "healthStatus": row.try_get::<String, _>("health_status").unwrap_or_else(|_| "unknown".into()),
            "lastHealthMessage": row.try_get::<Option<String>, _>("last_health_message").unwrap_or(None),
            "lastTestedAt": row.try_get::<Option<DateTime<Utc>>, _>("last_tested_at").unwrap_or(None).map(|v| v.to_rfc3339()),
            "mcpServerId": row.try_get::<Option<Uuid>, _>("mcp_server_id").unwrap_or(None).map(|v| v.to_string()),
            "toolPolicy": {
                "mode": row.try_get::<Option<String>, _>("policy_mode").unwrap_or(Some("all".into())),
                "tools": row.try_get::<Value, _>("policy_tools").unwrap_or(json!([])),
                "blockedTools": row.try_get::<Value, _>("policy_blocked_tools").unwrap_or(json!([])),
            },
            "secretBindingsMasked": has_sb,
            "createdAt": row.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
            "updatedAt": row.try_get::<DateTime<Utc>, _>("updated_at").ok().map(|v| v.to_rfc3339()),
            "canManage": can_manage_instance(row, user_id, &claims.role),
        })
    }).collect();

    Ok(Json(json!({ "items": items })))
}

/// POST /api/plugins/install
pub async fn install_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<InstallPluginRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let catalog = get_catalog_by_install_request(&state.db, &body).await?;
    let scope = normalize_scope(body.scope.as_deref().or(Some(&catalog.default_scope)))?;
    if scope == "global" && claims.role != "admin" {
        return Err(api_error(StatusCode::FORBIDDEN, "Solo admin puo' installare plugin globali"));
    }

    let project_id = if scope == "project" {
        let raw = body.project_id.as_deref()
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "projectId richiesto per scope project"))?;
        let parsed = Uuid::parse_str(raw).map_err(|_| api_error(StatusCode::BAD_REQUEST, "projectId non valido"))?;
        ensure_project_access(&state.db, user_id, parsed).await?;
        Some(parsed)
    } else { None };

    if let Some((eid, es, ep, eu)) = find_duplicate_instance_anywhere(&state.db, catalog.id, &catalog.slug).await? {
        let mut d = format!("scope={es}");
        if let Some(p) = ep { d.push_str(&format!(", projectId={p}")); }
        if let Some(u) = eu { d.push_str(&format!(", ownerUserId={u}")); }
        return Err(api_error(StatusCode::CONFLICT, format!("Plugin gia' installato (instance: {}, {})", eid, d)));
    }

    let required_keys = parse_string_array(&catalog.required_secret_refs);
    if !required_keys.is_empty() {
        let mut missing = Vec::new();
        for key in required_keys { if resolve_secret_value(&state.db, &key).await.is_none() { missing.push(key); } }
        if !missing.is_empty() {
            return Err(api_error(StatusCode::BAD_REQUEST, format!("Chiavi mancanti: {}", missing.join(", "))));
        }
    }

    let config = body.config.clone().unwrap_or_else(|| json!({}));
    let secret_bindings = body.secret_bindings.clone().unwrap_or_else(|| json!({}));

    let release_row = if let Some(version) = body.version.as_deref() {
        sqlx::query("SELECT id, version FROM plugin_releases WHERE catalog_item_id = $1 AND version = $2")
            .bind(catalog.id).bind(version).fetch_optional(&state.db).await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else { None };
    let release_id = release_row.as_ref().and_then(|r| r.try_get::<Uuid, _>("id").ok()).or(catalog.release_id);
    let release_version = release_row.as_ref().and_then(|r| r.try_get::<String, _>("version").ok())
        .or(catalog.release_version.clone()).unwrap_or_else(|| "1.0.0".into());

    let runtime_command = config.get("command").and_then(Value::as_str).map(str::to_string).or(catalog.stdio_command.clone());

    if catalog.transport == "stdio" {
        let allowed: HashSet<String> = parse_string_array(&catalog.allowed_commands).into_iter().map(|s| s.to_lowercase()).collect();
        if !allowed.is_empty() {
            let candidate = runtime_command.clone().unwrap_or_default().to_lowercase();
            if !allowed.contains(&candidate) {
                return Err(api_error(StatusCode::FORBIDDEN, "Command stdio non consentito"));
            }
        }
    }

    let instance_name = body.name.clone().unwrap_or_else(|| format!("{} ({release_version})", catalog.name));

    let pi_row = sqlx::query(
        "INSERT INTO plugin_instances (catalog_item_id, release_id, installed_by_user_id, project_id, scope, name, enabled, config, secret_bindings)
         VALUES ($1,$2,$3,$4,$5,$6,TRUE,$7,$8) RETURNING id",
    )
    .bind(catalog.id).bind(release_id).bind(user_id).bind(project_id).bind(&scope).bind(&instance_name).bind(&config).bind(&secret_bindings)
    .fetch_one(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let plugin_instance_id: Uuid = pi_row.try_get("id").unwrap_or(Uuid::nil());

    let pm = catalog.default_tool_policy.get("mode").and_then(Value::as_str).unwrap_or("allowlist").to_string();
    let pt = catalog.default_tool_policy.get("tools").cloned().unwrap_or(json!([]));
    let pb = catalog.default_tool_policy.get("blockedTools").cloned().unwrap_or(json!([]));
    let _ = sqlx::query(
        "INSERT INTO plugin_instance_tool_policies (plugin_instance_id, mode, tools, blocked_tools, updated_by_user_id) VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (plugin_instance_id) DO UPDATE SET mode=EXCLUDED.mode, tools=EXCLUDED.tools, blocked_tools=EXCLUDED.blocked_tools, updated_by_user_id=EXCLUDED.updated_by_user_id, updated_at=NOW()"
    ).bind(plugin_instance_id).bind(&pm).bind(&pt).bind(&pb).bind(user_id).execute(&state.db).await;

    let config_url = config.get("url").and_then(Value::as_str).map(str::to_string).or(catalog.http_url.clone());
    let config_args = config.get("args").cloned().unwrap_or_else(|| catalog.stdio_args.clone());
    let config_headers = config.get("headers").cloned().unwrap_or(json!({}));
    let config_env = config.get("envVars").cloned().unwrap_or(json!({}));

    let ms_row = sqlx::query(
        "INSERT INTO mcp_servers (plugin_instance_id, user_id, project_id, name, description, transport, url, command, args, env_vars, headers, enabled, scope)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,TRUE,$12) RETURNING id",
    )
    .bind(plugin_instance_id).bind(user_id).bind(project_id).bind(&instance_name).bind(Some(catalog.description.clone()))
    .bind(&catalog.transport).bind(config_url).bind(runtime_command).bind(config_args).bind(config_env).bind(config_headers).bind(&scope)
    .fetch_one(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mcp_server_id: Uuid = ms_row.try_get("id").unwrap_or(Uuid::nil());

    write_plugin_audit(&state.db, Some(plugin_instance_id), Some(user_id), project_id, "install", "ok",
        Some(format!("Plugin {} installato ({})", catalog.slug, release_version)),
        json!({"catalogItemId": catalog.id.to_string(), "scope": scope, "mcpServerId": mcp_server_id.to_string()})).await;

    Ok(Json(json!({ "ok": true, "pluginInstanceId": plugin_instance_id.to_string(), "mcpServerId": mcp_server_id.to_string(), "name": instance_name, "slug": catalog.slug, "version": release_version })))
}

/// DELETE /api/plugins/:id
pub async fn uninstall_plugin(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(id): AxumPath<String>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, pid, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) { return Err(api_error(StatusCode::FORBIDDEN, "Plugin non disinstallabile")); }

    let mcp_sid = existing.try_get::<Option<Uuid>, _>("mcp_server_id").unwrap_or(None);
    let proj_id = existing.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None);
    let pname = existing.try_get::<String, _>("name").unwrap_or_else(|_| "Plugin".into());
    let pslug = existing.try_get::<String, _>("slug").unwrap_or_else(|_| "unknown".into());

    let mut tx = state.db.begin().await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(sid) = mcp_sid { sqlx::query("DELETE FROM mcp_servers WHERE id=$1").bind(sid).execute(&mut *tx).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?; }
    sqlx::query("DELETE FROM plugin_instances WHERE id=$1").bind(pid).execute(&mut *tx).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tx.commit().await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(&state.db, None, Some(user_id), proj_id, "uninstall", "ok",
        Some(format!("Plugin disinstallato: {} ({})", pname, pslug)),
        json!({"pluginInstanceId": pid.to_string(), "mcpServerId": mcp_sid.map(|v| v.to_string()), "slug": pslug})).await;

    Ok(Json(json!({ "ok": true, "pluginInstanceId": pid.to_string() })))
}

/// PUT /api/plugins/:id/toggle
pub async fn toggle_plugin(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(id): AxumPath<String>, Json(body): Json<TogglePluginRequest>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, pid, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) { return Err(api_error(StatusCode::FORBIDDEN, "Plugin non modificabile")); }

    sqlx::query("UPDATE plugin_instances SET enabled=$2, updated_at=NOW() WHERE id=$1").bind(pid).bind(body.enabled).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Ok(msid) = existing.try_get::<Uuid, _>("mcp_server_id") {
        let _ = sqlx::query("UPDATE mcp_servers SET enabled=$2, updated_at=NOW() WHERE id=$1").bind(msid).bind(body.enabled).execute(&state.db).await;
    }

    write_plugin_audit(&state.db, Some(pid), Some(user_id), existing.try_get("project_id").unwrap_or(None), "toggle", "ok",
        Some(format!("Plugin {}", if body.enabled { "abilitato" } else { "disabilitato" })), json!({"enabled": body.enabled})).await;

    Ok(Json(json!({ "ok": true, "enabled": body.enabled })))
}

/// POST /api/plugins/:id/test
pub async fn test_plugin(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(id): AxumPath<String>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, pid, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) { return Err(api_error(StatusCode::FORBIDDEN, "Plugin non testabile")); }

    let resolution = build_plugin_resolution(&state.db, pid, user_id).await?;

    let (success, tool_count, error_message, tools_payload) = match mcp_client::list_tools(&resolution.config).await {
        Ok(tools) => {
            for tool in &tools {
                let schema = serde_json::to_value(&tool.input_schema).unwrap_or(json!({}));
                let _ = sqlx::query("INSERT INTO mcp_server_tools (server_id, tool_name, description, input_schema, discovered_at) VALUES ($1,$2,$3,$4,NOW()) ON CONFLICT (server_id, tool_name) DO UPDATE SET description=$3, input_schema=$4, discovered_at=NOW()")
                    .bind(resolution.mcp_server_id).bind(&tool.name).bind(&tool.description).bind(schema).execute(&state.db).await;
            }
            // Auto-populate allowlist if empty
            if let Ok(Some(pr)) = sqlx::query("SELECT mode, tools FROM plugin_instance_tool_policies WHERE plugin_instance_id=$1").bind(pid).fetch_optional(&state.db).await {
                let mode: String = pr.try_get("mode").unwrap_or_else(|_| "all".into());
                let ct: Value = pr.try_get("tools").unwrap_or(json!([]));
                if mode == "allowlist" && ct.as_array().map(|a| a.len()).unwrap_or(0) == 0 {
                    let discovered: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
                    let _ = sqlx::query("UPDATE plugin_instance_tool_policies SET tools=$2, updated_at=NOW() WHERE plugin_instance_id=$1")
                        .bind(pid).bind(json!(discovered)).execute(&state.db).await;
                }
            }
            let payload: Vec<Value> = tools.iter().map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema})).collect();
            (true, payload.len() as i32, None, payload)
        }
        Err(error) => {
            let raw = error.to_string();
            let mut msg = format_compact_error(&raw);
            if resolution.plugin_slug.eq_ignore_ascii_case("figma-http") && raw.contains("HTTP 401") {
                let th = resolve_secret_value(&state.db, "figma_oauth_token").await;
                msg = if th.as_deref().map(is_figma_pat).unwrap_or(false) {
                    "MCP Figma 401. Verifica token e riesegui il test.".into()
                } else {
                    "MCP Figma 401. Verifica OAuth app Figma.".into()
                };
            }
            (false, 0, Some(msg), Vec::new())
        }
    };

    let _ = sqlx::query("INSERT INTO plugin_instance_health_runs (plugin_instance_id, tested_by_user_id, success, tool_count, error_message, details) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(pid).bind(user_id).bind(success).bind(tool_count).bind(error_message.clone())
        .bind(json!({"mcpServerId": resolution.mcp_server_id.to_string(), "mcpServerName": resolution.mcp_server_name}))
        .execute(&state.db).await;
    let _ = sqlx::query("UPDATE plugin_instances SET health_status=$2, last_health_message=$3, last_tested_at=NOW(), updated_at=NOW() WHERE id=$1")
        .bind(pid).bind(if success { "ok" } else { "error" }).bind(error_message.clone()).execute(&state.db).await;

    write_plugin_audit(&state.db, Some(pid), Some(user_id), existing.try_get("project_id").unwrap_or(None),
        "test", if success { "ok" } else { "error" }, error_message.clone(),
        json!({"toolCount": tool_count, "mcpServerId": resolution.mcp_server_id.to_string()})).await;

    Ok(Json(json!({ "success": success, "toolCount": tool_count, "error": error_message, "tools": tools_payload })))
}

/// GET /api/plugins/:id/health
pub async fn get_plugin_health(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(id): AxumPath<String>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, pid, user_id).await?;

    let runs = sqlx::query("SELECT id, success, tool_count, error_message, details, created_at FROM plugin_instance_health_runs WHERE plugin_instance_id=$1 ORDER BY created_at DESC LIMIT 20")
        .bind(pid).fetch_all(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let runs_json: Vec<Value> = runs.iter().map(|r| json!({
        "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
        "success": r.try_get::<bool, _>("success").unwrap_or(false),
        "toolCount": r.try_get::<i32, _>("tool_count").unwrap_or(0),
        "errorMessage": r.try_get::<Option<String>, _>("error_message").unwrap_or(None),
        "details": r.try_get::<Value, _>("details").unwrap_or(json!({})),
        "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
    })).collect();

    Ok(Json(json!({
        "pluginInstanceId": pid.to_string(),
        "status": existing.try_get::<String, _>("health_status").unwrap_or_else(|_| "unknown".into()),
        "lastMessage": existing.try_get::<Option<String>, _>("last_health_message").unwrap_or(None),
        "lastTestedAt": existing.try_get::<Option<DateTime<Utc>>, _>("last_tested_at").unwrap_or(None).map(|v| v.to_rfc3339()),
        "runs": runs_json,
    })))
}

/// PUT /api/plugins/:id/tool-policy
pub async fn update_plugin_tool_policy(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(id): AxumPath<String>, Json(body): Json<UpdateToolPolicyRequest>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, pid, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) { return Err(api_error(StatusCode::FORBIDDEN, "Plugin non modificabile")); }

    let mode = body.mode.trim().to_lowercase();
    if !matches!(mode.as_str(), "allowlist" | "denylist" | "all") {
        return Err(api_error(StatusCode::BAD_REQUEST, "Mode non valido: usa allowlist, denylist o all"));
    }

    let tj = json!(body.tools.unwrap_or_default());
    let btj = json!(body.blocked_tools.unwrap_or_default());
    sqlx::query("INSERT INTO plugin_instance_tool_policies (plugin_instance_id, mode, tools, blocked_tools, updated_by_user_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (plugin_instance_id) DO UPDATE SET mode=EXCLUDED.mode, tools=EXCLUDED.tools, blocked_tools=EXCLUDED.blocked_tools, updated_by_user_id=EXCLUDED.updated_by_user_id, updated_at=NOW()")
        .bind(pid).bind(&mode).bind(&tj).bind(&btj).bind(user_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(&state.db, Some(pid), Some(user_id), existing.try_get("project_id").unwrap_or(None),
        "tool_policy_update", "ok", Some("Policy tool aggiornata".into()), json!({"mode": mode, "tools": tj, "blockedTools": btj})).await;

    Ok(Json(json!({ "ok": true, "mode": mode, "tools": tj, "blockedTools": btj })))
}

/// POST /api/plugins/:id/migrate-legacy
pub async fn migrate_legacy_mcp_server(State(state): State<AppState>, Extension(claims): Extension<Claims>, AxumPath(server_id): AxumPath<String>) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let sid = Uuid::parse_str(&server_id).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server MCP id non valido"))?;

    let row = sqlx::query("SELECT id, user_id, project_id, scope, plugin_instance_id, name, description, transport, url, command, args, env_vars, headers FROM mcp_servers WHERE id=$1")
        .bind(sid).fetch_optional(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(row) = row else { return Err(api_error(StatusCode::NOT_FOUND, "Server MCP legacy non trovato")); };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let scope: String = row.try_get::<String, _>("scope").unwrap_or_else(|_| "user".to_string()).to_lowercase();
    if !(owner == Some(user_id) || (scope == "global" && claims.role == "admin")) {
        return Err(api_error(StatusCode::FORBIDDEN, "Server MCP non gestibile"));
    }

    if let Some(pid) = row.try_get::<Option<Uuid>, _>("plugin_instance_id").unwrap_or(None) {
        return Ok(Json(json!({"ok": true, "alreadyMigrated": true, "pluginInstanceId": pid.to_string()})));
    }

    let transport: String = row.try_get("transport").unwrap_or_else(|_| "http".into());
    let url: Option<String> = row.try_get("url").unwrap_or(None);
    let command: Option<String> = row.try_get("command").unwrap_or(None);
    let args: Value = row.try_get("args").unwrap_or(json!([]));

    let slug = detect_legacy_catalog_slug(&transport, url.as_deref(), command.as_deref(), &args)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "MCP legacy non mappabile a un plugin del catalogo"))?;

    let catalog = get_catalog_by_install_request(&state.db, &InstallPluginRequest {
        catalog_item_id: None, slug: Some(slug.into()), version: None, scope: Some(scope.clone()),
        project_id: row.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None).map(|v| v.to_string()),
        name: None, config: None, secret_bindings: None,
    }).await?;

    if let Some((eid, _, _, _)) = find_duplicate_instance_anywhere(&state.db, catalog.id, &catalog.slug).await? {
        sqlx::query("UPDATE mcp_servers SET plugin_instance_id=$2, updated_at=NOW() WHERE id=$1").bind(sid).bind(eid).execute(&state.db).await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(json!({"ok": true, "relinked": true, "pluginInstanceId": eid.to_string()})));
    }

    let iname = row.try_get::<String, _>("name").unwrap_or_else(|_| catalog.name.clone());
    let pi_row = sqlx::query("INSERT INTO plugin_instances (catalog_item_id, release_id, installed_by_user_id, project_id, scope, name, enabled, config, secret_bindings) VALUES ($1,$2,$3,$4,$5,$6,TRUE,'{}'::jsonb,'{}'::jsonb) RETURNING id")
        .bind(catalog.id).bind(catalog.release_id).bind(user_id).bind(row.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None)).bind(&scope).bind(&iname)
        .fetch_one(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let pid: Uuid = pi_row.try_get("id").unwrap_or(Uuid::nil());

    sqlx::query("UPDATE mcp_servers SET plugin_instance_id=$2, updated_at=NOW() WHERE id=$1").bind(sid).bind(pid).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(&state.db, Some(pid), Some(user_id), row.try_get("project_id").unwrap_or(None),
        "migrate_legacy", "ok", Some(format!("MCP server {} migrato a plugin {}", sid, catalog.slug)),
        json!({"mcpServerId": sid.to_string(), "catalogSlug": catalog.slug})).await;

    Ok(Json(json!({"ok": true, "pluginInstanceId": pid.to_string(), "slug": catalog.slug})))
}

// -- Figma OAuth handlers --

/// GET /api/plugins/figma/oauth/status
pub async fn get_figma_oauth_status(State(state): State<AppState>, Extension(claims): Extension<Claims>) -> ApiResult {
    if claims.role != "admin" { return Err(api_error(StatusCode::FORBIDDEN, "Solo admin")); }
    let has_cid = get_setting(&state.db, "figma_client_id").await.map(|v| !v.trim().is_empty()).unwrap_or(false);
    let has_cs = get_setting(&state.db, "figma_client_secret").await.map(|v| !v.trim().is_empty()).unwrap_or(false);
    let at = get_setting(&state.db, "figma_oauth_token").await.unwrap_or_default();
    let ts = get_setting(&state.db, "figma_token_scope").await.unwrap_or_default();
    let te = get_setting(&state.db, "figma_token_expires_at").await.unwrap_or_default();
    let le = get_setting(&state.db, "figma_last_oauth_error").await.unwrap_or_default();
    let ps = resolve_bool_setting(&state.db, "figma_mcp_prefer_stdio", true).await;
    Ok(Json(json!({
        "configured": has_cid && has_cs, "hasClientId": has_cid, "hasClientSecret": has_cs,
        "hasAccessToken": !at.trim().is_empty(),
        "tokenType": if is_figma_pat(&at) { "pat" } else { "oauth_or_unknown" },
        "tokenScope": ts, "tokenExpiresAt": te, "lastError": le,
        "redirectUri": get_setting(&state.db, "figma_oauth_redirect_uri").await.unwrap_or_else(figma_oauth_redirect_uri),
        "preferStdioFallback": ps
    })))
}

/// POST /api/plugins/figma/oauth/start
pub async fn start_figma_oauth(State(state): State<AppState>, Extension(claims): Extension<Claims>, Json(body): Json<FigmaOAuthStartRequest>) -> ApiResult {
    if claims.role != "admin" { return Err(api_error(StatusCode::FORBIDDEN, "Solo admin")); }
    let user_id = parse_user_id(&claims)?;
    let (client_id, _, redirect_uri) = figma_oauth_client_credentials(&state.db).await?;
    let discovery = fetch_figma_oauth_discovery().await.map_err(|e| api_error(StatusCode::BAD_GATEWAY, e))?;
    let jwt_secret = get_or_create_jwt_secret(&state.db).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let signed_state = encode(&Header::default(), &FigmaOAuthStateClaims {
        user_id: user_id.to_string(), return_to: sanitize_return_to(body.return_to.as_deref()),
        exp: (Utc::now() + Duration::minutes(20)).timestamp() as usize,
    }, &EncodingKey::from_secret(jwt_secret.as_bytes())).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let url = format!("{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&resource={}",
        discovery.authorization_endpoint, urlencoding::encode(&client_id), urlencoding::encode(&redirect_uri),
        urlencoding::encode("mcp:connect"), urlencoding::encode(&signed_state), urlencoding::encode("https://mcp.figma.com"));
    Ok(Json(json!({"url": url, "redirectUri": redirect_uri})))
}

/// GET /auth/figma/mcp/callback
pub async fn figma_oauth_callback(State(state): State<AppState>, Query(query): Query<FigmaOAuthCallbackQuery>) -> Response {
    let mut return_to = FIGMA_DEFAULT_RETURN_TO.to_string();
    let Some(raw_state) = query.state.as_deref() else {
        return redirect_with_status(&return_to, "error", Some("State OAuth mancante"));
    };
    let jwt_secret = match get_or_create_jwt_secret(&state.db).await {
        Ok(s) => s, Err(e) => return redirect_with_status(&return_to, "error", Some(&format!("JWT: {e}"))),
    };
    let state_claims = match decode::<FigmaOAuthStateClaims>(raw_state, &DecodingKey::from_secret(jwt_secret.as_bytes()), &Validation::default()) {
        Ok(d) => d.claims, Err(_) => return redirect_with_status(&return_to, "error", Some("State non valido")),
    };
    return_to = sanitize_return_to(Some(&state_claims.return_to));

    if let Some(error) = query.error.as_deref() {
        let msg = query.error_description.as_deref().filter(|v| !v.trim().is_empty()).unwrap_or(error);
        store_figma_oauth_error(&state.db, msg).await;
        return redirect_with_status(&return_to, "error", Some(msg));
    }

    let user_id = match Uuid::parse_str(&state_claims.user_id) { Ok(v) => v, Err(_) => return redirect_with_status(&return_to, "error", Some("Utente non valido")) };
    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id=$1").bind(user_id).fetch_optional(&state.db).await.ok().flatten().unwrap_or_default();
    if role != "admin" { return redirect_with_status(&return_to, "error", Some("Solo admin")); }

    let Some(code) = query.code.as_deref().filter(|v| !v.trim().is_empty()) else {
        return redirect_with_status(&return_to, "error", Some("Code mancante"));
    };

    let (client_id, client_secret, redirect_uri) = match figma_oauth_client_credentials(&state.db).await {
        Ok(v) => v, Err((_, p)) => { let m = p.0.get("error").and_then(Value::as_str).unwrap_or("Config incompleta").to_string(); store_figma_oauth_error(&state.db, &m).await; return redirect_with_status(&return_to, "error", Some(&m)); }
    };
    let discovery = match fetch_figma_oauth_discovery().await { Ok(v) => v, Err(e) => { store_figma_oauth_error(&state.db, &e).await; return redirect_with_status(&return_to, "error", Some(&e)); } };

    let client = Client::builder().connect_timeout(std::time::Duration::from_secs(8)).timeout(std::time::Duration::from_secs(25)).build().unwrap_or_else(|_| Client::new());
    let token_response = match client.post(&discovery.token_endpoint).header("Accept", "application/json")
        .form(&[("grant_type","authorization_code"),("code",code),("client_id",&client_id),("client_secret",&client_secret),("redirect_uri",&redirect_uri)]).send().await {
        Ok(r) => r, Err(e) => { let m = format!("Token exchange fallito: {e}"); store_figma_oauth_error(&state.db, &m).await; return redirect_with_status(&return_to, "error", Some(&m)); }
    };

    let http_status = token_response.status();
    let tp = match token_response.json::<FigmaOAuthTokenResponse>().await {
        Ok(p) => p, Err(e) => { let m = format!("Risposta token non valida: {e}"); store_figma_oauth_error(&state.db, &m).await; return redirect_with_status(&return_to, "error", Some(&m)); }
    };

    if !http_status.is_success() || tp.access_token.as_deref().unwrap_or("").trim().is_empty() {
        let m = tp.error_description.clone().or(tp.error.clone()).unwrap_or_else(|| format!("HTTP {}", http_status.as_u16()));
        store_figma_oauth_error(&state.db, &m).await;
        return redirect_with_status(&return_to, "error", Some(&m));
    }

    let at = tp.access_token.as_deref().unwrap_or_default().trim().to_string();
    let rt = tp.refresh_token.as_deref().unwrap_or_default().trim().to_string();
    let sc = tp.scope.unwrap_or_else(|| "mcp:connect".into());
    let tt = tp.token_type.unwrap_or_else(|| "Bearer".into());
    let ea = tp.expires_in.map(|s| (Utc::now() + Duration::seconds(s.max(0))).to_rfc3339());

    let _ = upsert_setting_value(&state.db, "figma_oauth_token", &at, "connectors", "Token Figma", true).await;
    let _ = upsert_setting_value(&state.db, "figma_refresh_token", &rt, "connectors", "Refresh token Figma", true).await;
    let _ = upsert_setting_value(&state.db, "figma_token_scope", &sc, "connectors", "Scope token Figma", false).await;
    let _ = upsert_setting_value(&state.db, "figma_token_expires_at", ea.as_deref().unwrap_or(""), "connectors", "Scadenza token Figma", false).await;
    let _ = upsert_setting_value(&state.db, "figma_last_oauth_error", "", "connectors", "Ultimo errore OAuth Figma", false).await;

    redirect_with_status(&return_to, "ok", Some(&format!("OAuth Figma collegato ({tt}).")))
}
