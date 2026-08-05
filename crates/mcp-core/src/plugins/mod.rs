use std::collections::HashMap;

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use reqwest::Client;
use nexus_types::error_presentation::{render_user_error, ErrorDomain, ErrorFacts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    auth::{backend_url, frontend_url, get_or_create_jwt_secret, get_setting, Claims},
    chat_learning::{api_error, parse_user_id, ApiResult},
    mcp_client::{McpServerConfig, McpTransport},
    AppState,
};

pub mod catalog;
pub mod figma;
pub mod install;
pub mod integrate;
pub mod runtime;

pub use catalog::{list_installed_plugins, list_plugin_catalog};
pub use figma::{figma_oauth_callback, get_figma_oauth_status, start_figma_oauth};
pub use install::{
    install_plugin, migrate_legacy_mcp_server, toggle_plugin, uninstall_plugin, update_plugin,
};
pub use integrate::{draft_plugin_integration, publish_plugin_integration};
pub use runtime::{get_plugin_health, test_plugin, update_plugin_tool_policy};

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
pub(super) struct FigmaOAuthStateClaims {
    pub(super) user_id: String,
    pub(super) return_to: String,
    pub(super) exp: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct FigmaOAuthDiscovery {
    pub(super) authorization_endpoint: String,
    pub(super) token_endpoint: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct FigmaOAuthTokenResponse {
    pub(super) access_token: Option<String>,
    pub(super) refresh_token: Option<String>,
    pub(super) token_type: Option<String>,
    pub(super) expires_in: Option<i64>,
    pub(super) scope: Option<String>,
    pub(super) error: Option<String>,
    pub(super) error_description: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CatalogConfig {
    pub(super) id: Uuid,
    pub(super) slug: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) transport: String,
    pub(super) http_url: Option<String>,
    pub(super) stdio_command: Option<String>,
    pub(super) stdio_args: Value,
    pub(super) required_secret_refs: Value,
    pub(super) optional_secret_refs: Value,
    pub(super) default_scope: String,
    pub(super) allowed_commands: Value,
    pub(super) default_tool_policy: Value,
    pub(super) release_id: Option<Uuid>,
    pub(super) release_version: Option<String>,
}

#[derive(Debug)]

pub(super) struct PluginResolution {
    pub(super) mcp_server_id: Uuid,
    pub(super) mcp_server_name: String,
    pub(super) plugin_slug: String,
    pub(super) config: McpServerConfig,
}

pub(super) const FIGMA_DEFAULT_RETURN_TO: &str = "/admin/settings/connectors";
pub(super) const FIGMA_DISCOVERY_URL: &str =
    "https://api.figma.com/.well-known/oauth-authorization-server";

/// Lo scope validato, con l'errore HTTP di QUESTO endpoint.
///
/// Il vocabolario e la normalizzazione vivono in
/// `nexus_mcp_client::plugin_storage` (le stesse righe stavano in TRE copie —
/// qui, in `integrate.rs` e in `plugin-service`); qui resta il messaggio, che
/// nomina il campo e percio' non e' condivisibile.
pub(super) fn normalize_scope(raw: Option<&str>) -> Result<String, (StatusCode, Json<Value>)> {
    nexus_mcp_client::plugin_storage::normalizza_scope(raw).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Scope non valido: usa global, project o user",
        )
    })
}

pub(super) fn parse_string_array(raw: &Value) -> Vec<String> {
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

// Tre helper che erano gemelli identici di quelli in `plugin-service`, e che il
// censimento delle firme ha fatto emergere il 2026-08-05. La definizione vive
// ora in `nexus_mcp_client::plugin_storage`, lo stesso crate che la Wave C1
// aveva creato proprio per la duplicazione fra questi due lati.
pub(super) use nexus_mcp_client::plugin_storage::{get_json_object, value_to_string_map};

/// Chi puo' gestire un'ISTANZA di plugin: proprietario, o admin su `global`.
///
/// La colonna del proprietario e' l'unica cosa che cambia rispetto ai server
/// MCP (`installed_by_user_id` contro `user_id`), ed e' percio' un parametro del
/// punto unico invece di due copie della stessa condizione.
pub(super) fn can_manage_instance(row: &sqlx::postgres::PgRow, user_id: Uuid, role: &str) -> bool {
    nexus_mcp_client::plugin_storage::puo_gestire(row, user_id, role, "installed_by_user_id")
}

/// Messaggio d'errore di un plugin, dal punto unico di presentazione.
///
/// Qui viveva `format_compact_error`, uno dei due gemelli (l'altro in
/// `plugin-service`) che collassavano gli spazi e troncavano a 300: la stessa
/// normalizzazione scritta due volte, e nessuna traduzione — il `Display` di
/// reqwest e l'oggetto JSON-RPC passavano interi, solo compressi. Ora la
/// normalizzazione vive una sola volta dentro `render_user_error`, e il testo
/// del plugin viaggia come `upstream_message`: e' una frase, non una struttura.
pub(super) fn plugin_error_message(message: &str) -> String {
    render_user_error(
        &ErrorFacts::opaque(ErrorDomain::Plugin, message).with_upstream(message),
    )
    .message
}

pub(super) fn sanitize_return_to(value: Option<&str>) -> String {
    let raw = value.unwrap_or(FIGMA_DEFAULT_RETURN_TO).trim();
    if raw.starts_with('/') && !raw.starts_with("//") {
        raw.to_string()
    } else {
        FIGMA_DEFAULT_RETURN_TO.to_string()
    }
}

pub(super) fn redirect_with_status(
    return_to: &str,
    status: &str,
    message: Option<&str>,
) -> Response {
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

pub(super) async fn upsert_setting_value(
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

pub(super) async fn resolve_bool_setting(db: &PgPool, key: &str, default_value: bool) -> bool {
    match get_setting(db, key).await {
        Some(value) => {
            let normalized = value.trim().to_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        }
        None => default_value,
    }
}

pub(super) fn is_figma_pat(token: &str) -> bool {
    token.trim().to_lowercase().starts_with("figd_")
}

pub(super) async fn fetch_figma_oauth_discovery() -> Result<FigmaOAuthDiscovery, String> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| Client::new());

    let response = client
        .get(FIGMA_DISCOVERY_URL)
        .send()
        .await
        .map_err(|error| format!("Discovery OAuth Figma non raggiungibile: {error}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Discovery OAuth Figma HTTP {}",
            response.status().as_u16()
        ));
    }

    response
        .json::<FigmaOAuthDiscovery>()
        .await
        .map_err(|error| format!("Discovery OAuth Figma non valida: {error}"))
}

pub(super) async fn find_duplicate_instance_anywhere(
    db: &PgPool,
    catalog_item_id: Uuid,
    catalog_slug: &str,
) -> Result<Option<(Uuid, String, Option<Uuid>, Option<Uuid>)>, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        r#"
        SELECT
            pi.id,
            pi.scope,
            pi.project_id,
            pi.installed_by_user_id
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        WHERE pi.catalog_item_id = $1
           OR LOWER(c.slug) = LOWER($2)
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

pub(super) async fn cleanup_plugin_and_adapter_duplicates(db: &PgPool) -> Result<(), sqlx::Error> {
    // Deduplica plugin instances per catalog_item_id (tiene la più vecchia come keeper).
    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                catalog_item_id,
                ROW_NUMBER() OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS rn,
                FIRST_VALUE(id) OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS keeper_id
            FROM plugin_instances
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE mcp_servers m
        SET plugin_instance_id = d.keeper_id,
            updated_at = NOW()
        FROM dups d
        WHERE m.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                catalog_item_id,
                ROW_NUMBER() OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS rn,
                FIRST_VALUE(id) OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS keeper_id
            FROM plugin_instances
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_instance_tool_policies p
        SET plugin_instance_id = d.keeper_id,
            updated_at = NOW()
        FROM dups d
        WHERE p.plugin_instance_id = d.duplicate_id
          AND NOT EXISTS (
              SELECT 1
              FROM plugin_instance_tool_policies keep
              WHERE keep.plugin_instance_id = d.keeper_id
          );
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                catalog_item_id,
                ROW_NUMBER() OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS rn,
                FIRST_VALUE(id) OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS keeper_id
            FROM plugin_instances
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_instance_health_runs h
        SET plugin_instance_id = d.keeper_id
        FROM dups d
        WHERE h.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                catalog_item_id,
                ROW_NUMBER() OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS rn,
                FIRST_VALUE(id) OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS keeper_id
            FROM plugin_instances
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_audit_events a
        SET plugin_instance_id = d.keeper_id
        FROM dups d
        WHERE a.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                catalog_item_id,
                ROW_NUMBER() OVER (
                    PARTITION BY catalog_item_id
                    ORDER BY created_at ASC, id ASC
                ) AS rn
            FROM plugin_instances
        ),
        dups AS (
            SELECT id AS duplicate_id
            FROM ranked
            WHERE rn > 1
        )
        DELETE FROM plugin_instances p
        USING dups d
        WHERE p.id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    // Pass 2: deduplica per slug catalogo (copre vecchi dati con catalog_item_id diversi ma stesso plugin logico).
    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                pi.id,
                LOWER(c.slug) AS slug_key,
                ROW_NUMBER() OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS rn,
                FIRST_VALUE(pi.id) OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS keeper_id
            FROM plugin_instances pi
            JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
            WHERE COALESCE(c.slug, '') <> ''
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE mcp_servers m
        SET plugin_instance_id = d.keeper_id,
            updated_at = NOW()
        FROM dups d
        WHERE m.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                pi.id,
                LOWER(c.slug) AS slug_key,
                ROW_NUMBER() OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS rn,
                FIRST_VALUE(pi.id) OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS keeper_id
            FROM plugin_instances pi
            JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
            WHERE COALESCE(c.slug, '') <> ''
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_instance_tool_policies p
        SET plugin_instance_id = d.keeper_id,
            updated_at = NOW()
        FROM dups d
        WHERE p.plugin_instance_id = d.duplicate_id
          AND NOT EXISTS (
              SELECT 1
              FROM plugin_instance_tool_policies keep
              WHERE keep.plugin_instance_id = d.keeper_id
          );
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                pi.id,
                LOWER(c.slug) AS slug_key,
                ROW_NUMBER() OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS rn,
                FIRST_VALUE(pi.id) OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS keeper_id
            FROM plugin_instances pi
            JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
            WHERE COALESCE(c.slug, '') <> ''
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_instance_health_runs h
        SET plugin_instance_id = d.keeper_id
        FROM dups d
        WHERE h.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                pi.id,
                LOWER(c.slug) AS slug_key,
                ROW_NUMBER() OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS rn,
                FIRST_VALUE(pi.id) OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS keeper_id
            FROM plugin_instances pi
            JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
            WHERE COALESCE(c.slug, '') <> ''
        ),
        dups AS (
            SELECT id AS duplicate_id, keeper_id
            FROM ranked
            WHERE rn > 1
        )
        UPDATE plugin_audit_events a
        SET plugin_instance_id = d.keeper_id
        FROM dups d
        WHERE a.plugin_instance_id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                pi.id,
                LOWER(c.slug) AS slug_key,
                ROW_NUMBER() OVER (
                    PARTITION BY LOWER(c.slug)
                    ORDER BY pi.created_at ASC, pi.id ASC
                ) AS rn
            FROM plugin_instances pi
            JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
            WHERE COALESCE(c.slug, '') <> ''
        ),
        dups AS (
            SELECT id AS duplicate_id
            FROM ranked
            WHERE rn > 1
        )
        DELETE FROM plugin_instances p
        USING dups d
        WHERE p.id = d.duplicate_id;
        "#,
    )
    .execute(db)
    .await?;

    // Deduplica adapter MCP collegati allo stesso plugin_instance_id.
    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT
                id,
                plugin_instance_id,
                ROW_NUMBER() OVER (
                    PARTITION BY plugin_instance_id
                    ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC
                ) AS rn
            FROM mcp_servers
            WHERE plugin_instance_id IS NOT NULL
        ),
        dups AS (
            SELECT id
            FROM ranked
            WHERE rn > 1
        )
        DELETE FROM mcp_servers m
        USING dups d
        WHERE m.id = d.id;
        "#,
    )
    .execute(db)
    .await?;

    Ok(())
}

pub(super) fn parse_args_array(raw: &Value) -> Vec<String> {
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

// Punto unico (regola L) per mappare (transport, command, args) -> slug del
// catalog. Usato da plugins::install e da mcp_connectors::execute_mcp_tool per
// l'iniezione args scoped sul server @playwright/mcp (slug "playwright-stdio").
pub(crate) fn detect_legacy_catalog_slug(
    transport: &str,
    url: Option<&str>,
    command: Option<&str>,
    args: &Value,
) -> Option<&'static str> {
    let transport = transport.trim().to_lowercase();
    if transport == "http" {
        let url = url.unwrap_or_default().to_lowercase();
        if url.contains("mcp.figma.com/mcp") {
            return Some("figma-http");
        }
        if url.contains("api.githubcopilot.com/mcp") {
            return Some("github-http");
        }
        return None;
    }

    if transport == "stdio" {
        let command = command.unwrap_or_default().trim().to_lowercase();
        let args = parse_args_array(args);
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-filesystem"))
        {
            return Some("filesystem-local");
        }
        if command == "npx" && args.iter().any(|item| item.contains("@playwright/mcp")) {
            return Some("playwright-stdio");
        }
        // MCP standard servers (stdio) via npx @modelcontextprotocol/server-*
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-redis"))
        {
            return Some("redis-stdio");
        }
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-sqlite"))
        {
            return Some("sqlite-stdio");
        }
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-postgres"))
        {
            return Some("postgres-stdio");
        }
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-gitlab"))
        {
            return Some("gitlab-stdio");
        }
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-github"))
        {
            return Some("github-stdio");
        }
        if command == "npx"
            && args
                .iter()
                .any(|item| item.contains("@modelcontextprotocol/server-memory"))
        {
            return Some("memory-stdio");
        }
    }

    None
}

pub(super) async fn write_plugin_audit(
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

pub(super) async fn resolve_secret_value(db: &PgPool, setting_key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(setting_key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Campi grezzi della riga `mcp_servers` (join con il catalog) necessari a
/// costruire la runtime config di un plugin.
struct PluginMcpServerRow {
    id: Uuid,
    name: String,
    transport: String,
    url: Option<String>,
    command: Option<String>,
    args: Value,
    static_headers: Value,
    static_env: Value,
    plugin_slug: String,
}

/// Legge la riga DB con i default applicati per ogni colonna mancante.
/// Separa la decodifica della riga dalla risoluzione dei secret.
fn read_plugin_mcp_server_row(row: &sqlx::postgres::PgRow) -> PluginMcpServerRow {
    PluginMcpServerRow {
        id: row.try_get("mcp_server_id").unwrap_or(Uuid::nil()),
        name: row
            .try_get("mcp_server_name")
            .unwrap_or_else(|_| "Plugin MCP".to_string()),
        transport: row
            .try_get("transport")
            .unwrap_or_else(|_| "http".to_string()),
        url: row.try_get("url").unwrap_or(None),
        command: row.try_get("command").unwrap_or(None),
        args: row.try_get("args").unwrap_or(json!([])),
        static_headers: row.try_get("headers").unwrap_or(json!({})),
        static_env: row.try_get("env_vars").unwrap_or(json!({})),
        plugin_slug: row.try_get("slug").unwrap_or_default(),
    }
}

/// Valore da mettere nell'header `Authorization`: il prefisso `Bearer ` viene
/// aggiunto solo se il secret non lo porta già con sé.
/// Punto unico (regola L): serve sia ai secret binding sia agli header Figma.
fn bearer_header_value(secret: &str) -> String {
    if secret.to_lowercase().starts_with("bearer ") {
        secret.to_string()
    } else {
        format!("Bearer {secret}")
    }
}

/// Sovrascrive gli header statici con i secret referenziati da
/// `secret_bindings.headers` (mappa nome header -> chiave in `settings`).
async fn apply_header_secret_bindings(
    db: &PgPool,
    secret_bindings: &Value,
    headers: &mut HashMap<String, String>,
) {
    if let Some(bindings_headers) = get_json_object(secret_bindings, "headers") {
        for (header_name, setting_key_raw) in bindings_headers {
            if let Some(setting_key) = setting_key_raw.as_str() {
                if let Some(secret) = resolve_secret_value(db, setting_key).await {
                    if header_name.eq_ignore_ascii_case("authorization") {
                        headers.insert(header_name.clone(), bearer_header_value(&secret));
                    } else {
                        headers.insert(header_name.clone(), secret);
                    }
                }
            }
        }
    }
}

/// Sovrascrive gli env var statici con i secret referenziati da
/// `secret_bindings.envVars` (mappa nome env var -> chiave in `settings`).
async fn apply_env_secret_bindings(
    db: &PgPool,
    secret_bindings: &Value,
    env_vars: &mut HashMap<String, String>,
) {
    if let Some(bindings_env) = get_json_object(secret_bindings, "envVars") {
        for (env_name, setting_key_raw) in bindings_env {
            if let Some(setting_key) = setting_key_raw.as_str() {
                if let Some(secret) = resolve_secret_value(db, setting_key).await {
                    env_vars.insert(env_name.clone(), secret);
                }
            }
        }
    }
}

/// Compatibilità Figma sul transport HTTP:
/// - OAuth token: Authorization: Bearer <token>
/// - Personal token (figd_...): X-Figma-Token: <token>
/// Manteniamo entrambi se disponibili per ridurre errori 401 su setup legacy.
/// Gli header già presenti (statici o da secret binding) non vengono toccati.
async fn apply_figma_http_headers(
    db: &PgPool,
    figma_token: Option<&str>,
    headers: &mut HashMap<String, String>,
) {
    let has_authorization = headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("authorization"));
    let has_x_figma_token = headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("x-figma-token"));
    let has_x_figma_region = headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("x-figma-region"));

    if let Some(secret) = figma_token {
        if !has_x_figma_token {
            headers.insert("X-Figma-Token".to_string(), secret.to_string());
        }
        if !has_authorization {
            headers.insert("Authorization".to_string(), bearer_header_value(secret));
        }
    }

    if !has_x_figma_region {
        if let Some(region) = resolve_secret_value(db, "figma_region").await {
            headers.insert("X-Figma-Region".to_string(), region);
        }
    }
}

/// Transport stdio verso `figma-developer-mcp`: risolve il token se non è già
/// disponibile e lo propaga negli env var del processo figlio.
async fn build_figma_stdio_transport(
    db: &PgPool,
    figma_token: Option<String>,
    mut env_vars: HashMap<String, String>,
) -> McpTransport {
    let mut token = figma_token;
    if token.is_none() {
        token = resolve_secret_value(db, "figma_oauth_token").await;
    }

    if let Some(token) = token {
        // `figma-developer-mcp` accetta PAT e OAuth token.
        // Passiamo entrambi gli env var per compatibilità.
        env_vars.insert("FIGMA_API_KEY".to_string(), token.clone());
        env_vars.insert("FIGMA_OAUTH_TOKEN".to_string(), token);
    }

    McpTransport::Stdio {
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "figma-developer-mcp".to_string(),
            "--stdio".to_string(),
            "--json".to_string(),
        ],
        env_vars,
    }
}

pub(super) async fn resolve_plugin_runtime_config(
    db: &PgPool,
    mcp_server_row: &sqlx::postgres::PgRow,
    secret_bindings: &Value,
) -> McpServerConfig {
    let row = read_plugin_mcp_server_row(mcp_server_row);
    let mut headers = value_to_string_map(row.static_headers.as_object());
    let mut env_vars = value_to_string_map(row.static_env.as_object());
    let figma_token = resolve_secret_value(db, "figma_oauth_token").await;

    apply_header_secret_bindings(db, secret_bindings, &mut headers).await;
    apply_env_secret_bindings(db, secret_bindings, &mut env_vars).await;

    let is_figma_http = row.plugin_slug.eq_ignore_ascii_case("figma-http");
    if is_figma_http {
        apply_figma_http_headers(db, figma_token.as_deref(), &mut headers).await;
    }

    let prefer_figma_stdio =
        is_figma_http && resolve_bool_setting(db, "figma_mcp_prefer_stdio", true).await;

    let transport_cfg = if prefer_figma_stdio {
        build_figma_stdio_transport(db, figma_token, env_vars).await
    } else if row.transport == "stdio" {
        McpTransport::Stdio {
            command: row.command.unwrap_or_default(),
            args: parse_string_array(&row.args),
            env_vars,
        }
    } else {
        McpTransport::Http {
            url: row.url.unwrap_or_default(),
            headers,
        }
    };

    McpServerConfig {
        id: row.id.to_string(),
        name: row.name,
        transport: transport_cfg,
        enabled: true,
    }
}

/// Applica la tool policy di default presente nel catalog a un plugin
/// instance appena creato (UPSERT in `plugin_instance_tool_policies`).
/// Punto unico (regola L / ADR 0026, step S20): prima questo blocco era
/// duplicato pari-pari in `install.rs` su 2 handler diversi (~34L cluster
/// jscpd). Best-effort: errori SQL ignorati.
pub(super) async fn apply_default_tool_policy(
    db: &PgPool,
    plugin_instance_id: Uuid,
    catalog: &CatalogConfig,
    user_id: Uuid,
) {
    let policy_mode = catalog
        .default_tool_policy
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("allowlist")
        .to_string();
    let policy_tools = catalog
        .default_tool_policy
        .get("tools")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let policy_blocked = catalog
        .default_tool_policy
        .get("blockedTools")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_instance_tool_policies
            (plugin_instance_id, mode, tools, blocked_tools, updated_by_user_id)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (plugin_instance_id)
        DO UPDATE SET
            mode = EXCLUDED.mode,
            tools = EXCLUDED.tools,
            blocked_tools = EXCLUDED.blocked_tools,
            updated_by_user_id = EXCLUDED.updated_by_user_id,
            updated_at = NOW()
        "#,
    )
    .bind(plugin_instance_id)
    .bind(policy_mode)
    .bind(policy_tools)
    .bind(policy_blocked)
    .bind(user_id)
    .execute(db)
    .await;
}

pub(super) async fn get_catalog_by_install_request(
    db: &PgPool,
    body: &InstallPluginRequest,
) -> Result<CatalogConfig, (StatusCode, Json<Value>)> {
    let row = if let Some(id_raw) = body.catalog_item_id.as_deref() {
        let catalog_id = Uuid::parse_str(id_raw)
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "catalogItemId non valido"))?;
        sqlx::query(
            r#"
            SELECT
                c.id, c.slug, c.name, c.description, c.transport, c.http_url, c.stdio_command,
                c.stdio_args, c.required_secret_refs, c.optional_secret_refs, c.default_scope,
                c.allowed_commands, c.default_tool_policy, c.is_allowlisted, c.enabled,
                r.id AS release_id, r.version AS release_version
            FROM plugin_catalog_items c
            LEFT JOIN LATERAL (
                SELECT id, version
                FROM plugin_releases
                WHERE catalog_item_id = c.id AND is_stable = true
                ORDER BY created_at DESC
                LIMIT 1
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
            SELECT
                c.id, c.slug, c.name, c.description, c.transport, c.http_url, c.stdio_command,
                c.stdio_args, c.required_secret_refs, c.optional_secret_refs, c.default_scope,
                c.allowed_commands, c.default_tool_policy, c.is_allowlisted, c.enabled,
                r.id AS release_id, r.version AS release_version
            FROM plugin_catalog_items c
            LEFT JOIN LATERAL (
                SELECT id, version
                FROM plugin_releases
                WHERE catalog_item_id = c.id AND is_stable = true
                ORDER BY created_at DESC
                LIMIT 1
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
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Plugin catalog item non trovato",
        ));
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
        transport: row
            .try_get("transport")
            .unwrap_or_else(|_| "http".to_string()),
        http_url: row.try_get("http_url").unwrap_or(None),
        stdio_command: row.try_get("stdio_command").unwrap_or(None),
        stdio_args: row.try_get("stdio_args").unwrap_or(json!([])),
        required_secret_refs: row.try_get("required_secret_refs").unwrap_or(json!([])),
        optional_secret_refs: row.try_get("optional_secret_refs").unwrap_or(json!([])),
        default_scope: row
            .try_get("default_scope")
            .unwrap_or_else(|_| "global".to_string()),
        allowed_commands: row.try_get("allowed_commands").unwrap_or(json!([])),
        default_tool_policy: row
            .try_get("default_tool_policy")
            .unwrap_or(json!({"mode":"allowlist","tools":[],"blockedTools":[] })),
        release_id: row.try_get("release_id").unwrap_or(None),
        release_version: row.try_get("release_version").unwrap_or(None),
    })
}

pub(super) async fn resolve_plugin_instance_for_user(
    db: &PgPool,
    plugin_instance_id: Uuid,
    user_id: Uuid,
) -> Result<sqlx::postgres::PgRow, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        r#"
        SELECT
            pi.id,
            pi.catalog_item_id,
            pi.release_id,
            pi.installed_by_user_id,
            pi.project_id,
            pi.scope,
            pi.name,
            pi.enabled,
            pi.config,
            pi.secret_bindings,
            pi.health_status,
            pi.last_health_message,
            pi.last_tested_at,
            c.slug,
            c.transport,
            c.http_url,
            c.stdio_command,
            c.stdio_args,
            ms.id AS mcp_server_id,
            ms.name AS mcp_server_name,
            ms.transport AS mcp_transport,
            ms.url,
            ms.command,
            ms.args,
            ms.headers,
            ms.env_vars
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        LEFT JOIN LATERAL (
            SELECT
                id,
                name,
                transport,
                url,
                command,
                args,
                headers,
                env_vars
            FROM mcp_servers
            WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC
            LIMIT 1
        ) ms ON TRUE
        WHERE pi.id = $1
          AND (
              pi.scope = 'global'
              OR pi.installed_by_user_id = $2
              OR (
                  pi.scope = 'project'
                  AND EXISTS (
                      SELECT 1
                      FROM projects p
                      LEFT JOIN project_members pm
                        ON pm.project_id = p.id AND pm.user_id = $2
                      WHERE p.id = pi.project_id
                        AND (p.owner_user_id = $2 OR pm.user_id IS NOT NULL)
                  )
              )
          )
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    row.ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Plugin installato non trovato"))
}

pub(super) async fn build_plugin_resolution(
    db: &PgPool,
    plugin_instance_id: Uuid,
    user_id: Uuid,
) -> Result<PluginResolution, (StatusCode, Json<Value>)> {
    let row = resolve_plugin_instance_for_user(db, plugin_instance_id, user_id).await?;
    let mcp_server_id: Uuid = row.try_get("mcp_server_id").map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Plugin senza adapter MCP collegato",
        )
    })?;
    let secret_bindings: Value = row.try_get("secret_bindings").unwrap_or(json!({}));
    let mcp_server_name: String = row.try_get("mcp_server_name").unwrap_or_else(|_| {
        row.try_get::<String, _>("name")
            .unwrap_or_else(|_| "Plugin MCP".to_string())
    });
    let plugin_slug: String = row.try_get("slug").unwrap_or_default();
    let config = resolve_plugin_runtime_config(db, &row, &secret_bindings).await;

    Ok(PluginResolution {
        mcp_server_id,
        mcp_server_name,
        plugin_slug,
        config,
    })
}

pub(super) fn figma_oauth_redirect_uri() -> String {
    format!("{}/auth/figma/mcp/callback", backend_url())
}

pub(super) async fn figma_oauth_client_credentials(
    db: &PgPool,
) -> Result<(String, String, String), (StatusCode, Json<Value>)> {
    let client_id = get_setting(db, "figma_client_id").await.ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "figma_client_id non configurato in Admin > Connettori",
        )
    })?;
    let client_secret = get_setting(db, "figma_client_secret")
        .await
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "figma_client_secret non configurato in Admin > Connettori",
            )
        })?;
    let redirect_uri = get_setting(db, "figma_oauth_redirect_uri")
        .await
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(figma_oauth_redirect_uri);
    Ok((client_id, client_secret, redirect_uri))
}

pub(super) async fn store_figma_oauth_error(db: &PgPool, message: &str) {
    let _ = upsert_setting_value(
        db,
        "figma_last_oauth_error",
        message,
        "connectors",
        "Ultimo errore OAuth Figma",
        false,
    )
    .await;
}
