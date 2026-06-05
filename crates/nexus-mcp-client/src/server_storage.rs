//! Helper SQL condivisi per la tabella `mcp_servers` (regola L / ADR 0026,
//! Wave C1). Prima questi helper (`row_to_json`, `can_manage_server`,
//! `build_config`) erano duplicati identici tra:
//!   - crates/mcp-core/src/mcp_connectors.rs
//!   - crates/plugin-service/src/mcp_connectors.rs
//!
//! Tipi pure (request/toggle) sono qui per evitare il drift fra le copie
//! delle due definizioni axum.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{McpServerConfig, McpTransport};

// -- Request types (deserializzati dai body axum dei due crate) --

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMcpServerRequest {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    /// "http" | "stdio"
    pub transport: String,
    /// per HTTP
    pub url: Option<String>,
    /// per stdio
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,
    pub headers: Option<HashMap<String, String>>,
    /// "user" | "project"
    pub scope: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMcpServerRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,
    pub headers: Option<HashMap<String, String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToggleRequest {
    pub enabled: bool,
}

// -- Helpers SQL --

/// Mappa una row Postgres della tabella `mcp_servers` nel payload JSON
/// camelCase atteso dal frontend.
pub fn row_to_json(r: &sqlx::postgres::PgRow, can_manage: bool) -> Value {
    let args: Value = r.try_get::<Value, _>("args").unwrap_or(json!([]));
    let env_vars: Value = r.try_get::<Value, _>("env_vars").unwrap_or(json!({}));
    let headers: Value = r.try_get::<Value, _>("headers").unwrap_or(json!({}));

    json!({
        "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
        "pluginInstanceId": r
            .try_get::<Option<Uuid>, _>("plugin_instance_id")
            .unwrap_or(None)
            .map(|v| v.to_string()),
        "name": r.try_get::<String, _>("name").unwrap_or_default(),
        "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
        "iconUrl": r.try_get::<Option<String>, _>("icon_url").unwrap_or(None),
        "transport": r.try_get::<String, _>("transport").unwrap_or_default(),
        "url": r.try_get::<Option<String>, _>("url").unwrap_or(None),
        "command": r.try_get::<Option<String>, _>("command").unwrap_or(None),
        "args": args,
        "envVars": env_vars,
        "headers": headers,
        "enabled": r.try_get::<bool, _>("enabled").unwrap_or(true),
        "scope": r.try_get::<String, _>("scope").unwrap_or_else(|_| "user".to_string()),
        "canManage": can_manage,
        "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
    })
}

/// Decide se l'utente puo' gestire un server MCP sulla base di owner + scope + ruolo.
pub fn can_manage_server(row: &sqlx::postgres::PgRow, user_id: Uuid, role: &str) -> bool {
    let owner_user_id: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let scope: String = row
        .try_get("scope")
        .unwrap_or_else(|_| "user".to_string());

    owner_user_id == Some(user_id) || (scope == "global" && role == "admin")
}

/// Costruisce un `McpServerConfig` da una row Postgres. Riconosce il transport
/// speciale "builtin" (in-process), oltre a "http" e "stdio".
pub fn build_config(
    id: &Uuid,
    name: &str,
    transport: &str,
    row: &sqlx::postgres::PgRow,
) -> McpServerConfig {
    // Nexus Builtin: nessuna rete, nessun processo esterno
    if transport == "builtin" {
        return McpServerConfig {
            id: id.to_string(),
            name: name.to_string(),
            transport: McpTransport::Builtin,
            enabled: true,
        };
    }

    let url: Option<String> = row.try_get("url").unwrap_or(None);
    let command: Option<String> = row.try_get("command").unwrap_or(None);
    let args_val: Value = row.try_get::<Value, _>("args").unwrap_or(json!([]));
    let env_vars_val: Value = row.try_get::<Value, _>("env_vars").unwrap_or(json!({}));
    let headers_val: Value = row.try_get::<Value, _>("headers").unwrap_or(json!({}));

    let mcp_transport = if transport == "stdio" {
        McpTransport::Stdio {
            command: command.unwrap_or_default(),
            args: serde_json::from_value(args_val).unwrap_or_default(),
            env_vars: serde_json::from_value(env_vars_val).unwrap_or_default(),
        }
    } else {
        McpTransport::Http {
            url: url.unwrap_or_default(),
            headers: serde_json::from_value(headers_val).unwrap_or_default(),
        }
    };

    McpServerConfig {
        id: id.to_string(),
        name: name.to_string(),
        transport: mcp_transport,
        enabled: row.try_get("enabled").unwrap_or(true),
    }
}
