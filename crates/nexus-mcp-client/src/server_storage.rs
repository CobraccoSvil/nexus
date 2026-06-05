//! Helper SQL condivisi per la tabella `mcp_servers` (regola L / ADR 0026,
//! Wave C1). Prima questi helper (`row_to_json`, `can_manage_server`,
//! `build_config`) erano duplicati identici tra:
//!   - crates/mcp-core/src/mcp_connectors.rs
//!   - crates/plugin-service/src/mcp_connectors.rs
//!
//! Tipi pure (request/toggle) sono qui per evitare il drift fra le copie
//! delle due definizioni axum.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, PgPool, Row};
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
///
/// Parsing tollerante per args/env_vars/headers: i valori non-stringa nel JSON
/// vengono silently skippati invece di far fallire l'intero campo (sopravvive
/// a row malformate). `enabled` e' hardcoded a `true` perche' i row che arrivano
/// qui sono gia' filtrati a livello SQL (es. `WHERE enabled = TRUE`).
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
            args: args_val
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            env_vars: env_vars_val
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    } else {
        McpTransport::Http {
            url: url.unwrap_or_default(),
            headers: headers_val
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    };

    McpServerConfig {
        id: id.to_string(),
        name: name.to_string(),
        transport: mcp_transport,
        enabled: true,
    }
}

// -- Helper policy/tool sets --

/// Converte un valore JSON (atteso come array di stringhe) in `HashSet<String>`.
/// Valori non-stringa vengono silently skippati. Punto unico (regola L / ADR
/// 0026, step S12'): prima duplicato in mcp-core e plugin-service mcp_connectors.
pub fn parse_json_string_set(raw: &Value) -> HashSet<String> {
    raw.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default()
}

/// Valuta se un tool e' ammesso dalla policy del server. `mode`:
/// - `"allowlist"`: ammesso solo se `allowed_tools.is_empty()` o se `allowed_tools.contains(tool_name)`
/// - `"denylist"` / `"all"`: ammesso (modulo `blocked_tools`)
/// - altro: ammesso (modulo `blocked_tools`)
///
/// `blocked_tools` ha precedenza assoluta su `allowed_tools`.
pub fn is_tool_allowed_by_policy(
    mode: Option<&str>,
    allowed_tools: &HashSet<String>,
    blocked_tools: &HashSet<String>,
    tool_name: &str,
) -> bool {
    if blocked_tools.contains(tool_name) {
        return false;
    }
    match mode.unwrap_or("all") {
        "allowlist" => allowed_tools.is_empty() || allowed_tools.contains(tool_name),
        "denylist" | "all" => true,
        _ => true,
    }
}

// -- Query SQL CRUD (punto unico, regola L / ADR 0026, Wave C5) --
// Prima questi blocchi INSERT/UPDATE/SELECT/DELETE erano duplicati identici
// in crates/mcp-core/src/mcp_connectors.rs e crates/plugin-service/src/mcp_connectors.rs
// (cluster top del jscpd report: 111L + 96L). Gli handler axum nei due crate
// restano locali ma delegano tutta la parte SQL qui.

/// SELECT id, user_id, scope FROM mcp_servers WHERE id=$1
/// Usato dagli handler update/delete/toggle per verificare proprieta'.
pub async fn fetch_owner_scope(
    db: &PgPool,
    server_id: Uuid,
) -> Result<Option<PgRow>, sqlx::Error> {
    sqlx::query("SELECT id, user_id, scope FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .fetch_optional(db)
        .await
}

/// INSERT INTO mcp_servers (...) VALUES (...) RETURNING * (subset)
/// Crea un nuovo server e ritorna la row appena inserita.
pub async fn insert_mcp_server(
    db: &PgPool,
    user_id: Uuid,
    body: &CreateMcpServerRequest,
) -> Result<PgRow, sqlx::Error> {
    let scope = body.scope.as_deref().unwrap_or("user");
    let project_id: Option<Uuid> = body
        .project_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());
    let args_json = json!(body.args.clone().unwrap_or_default());
    let env_json = json!(body.env_vars.clone().unwrap_or_default());
    let headers_json = json!(body.headers.clone().unwrap_or_default());

    sqlx::query(
        "INSERT INTO mcp_servers
            (user_id, project_id, name, description, icon_url, transport, url, command,
             args, env_vars, headers, enabled, scope)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,true,$12)
         RETURNING id, plugin_instance_id, name, description, icon_url, transport, url, command, args,
                   env_vars, headers, enabled, scope, created_at",
    )
    .bind(user_id)
    .bind(project_id)
    .bind(&body.name)
    .bind(&body.description)
    .bind(&body.icon_url)
    .bind(&body.transport)
    .bind(&body.url)
    .bind(&body.command)
    .bind(&args_json)
    .bind(&env_json)
    .bind(&headers_json)
    .bind(scope)
    .fetch_one(db)
    .await
}

/// Applica gli UPDATE incrementali per ogni campo opzionale di
/// `UpdateMcpServerRequest`, poi ricarica e ritorna la row aggiornata.
pub async fn apply_update_and_fetch(
    db: &PgPool,
    server_id: Uuid,
    body: &UpdateMcpServerRequest,
) -> Result<PgRow, sqlx::Error> {
    if let Some(name) = &body.name {
        sqlx::query("UPDATE mcp_servers SET name=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(name)
            .execute(db)
            .await?;
    }
    if let Some(desc) = &body.description {
        sqlx::query("UPDATE mcp_servers SET description=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(desc)
            .execute(db)
            .await?;
    }
    if let Some(url) = &body.url {
        sqlx::query("UPDATE mcp_servers SET url=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(url)
            .execute(db)
            .await?;
    }
    if let Some(cmd) = &body.command {
        sqlx::query("UPDATE mcp_servers SET command=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(cmd)
            .execute(db)
            .await?;
    }
    if let Some(args) = &body.args {
        let v = json!(args);
        sqlx::query("UPDATE mcp_servers SET args=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(v)
            .execute(db)
            .await?;
    }
    if let Some(env) = &body.env_vars {
        let v = json!(env);
        sqlx::query("UPDATE mcp_servers SET env_vars=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(v)
            .execute(db)
            .await?;
    }
    if let Some(headers) = &body.headers {
        let v = json!(headers);
        sqlx::query("UPDATE mcp_servers SET headers=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(v)
            .execute(db)
            .await?;
    }
    if let Some(enabled) = body.enabled {
        sqlx::query("UPDATE mcp_servers SET enabled=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id)
            .bind(enabled)
            .execute(db)
            .await?;
    }

    sqlx::query(
        "SELECT id, plugin_instance_id, name, description, icon_url, transport, url, command, args,
                env_vars, headers, enabled, scope, user_id, created_at
         FROM mcp_servers WHERE id=$1",
    )
    .bind(server_id)
    .fetch_one(db)
    .await
}

/// DELETE FROM mcp_servers WHERE id=$1
pub async fn delete_mcp_server(db: &PgPool, server_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .execute(db)
        .await
        .map(|_| ())
}

/// UPDATE mcp_servers SET enabled=$2 WHERE id=$1
pub async fn set_enabled(
    db: &PgPool,
    server_id: Uuid,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mcp_servers SET enabled=$2, updated_at=NOW() WHERE id=$1")
        .bind(server_id)
        .bind(enabled)
        .execute(db)
        .await
        .map(|_| ())
}

/// SELECT * FROM mcp_servers WHERE user_id=$1 OR scope='global' ORDER BY created_at DESC.
/// Lista i server visibili a un utente (propri + scope global).
pub async fn list_servers_for_user(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT id, plugin_instance_id, name, description, icon_url, transport, url, command, args, env_vars, headers,
                enabled, scope, user_id, created_at
         FROM mcp_servers
         WHERE user_id = $1 OR scope = 'global'
         ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Fetch leggero del server (id + transport + connection fields) per il path
/// `POST /api/mcp-servers/:id/test`, con filtro proprieta' `user_id=$1 OR scope='global'`.
pub async fn fetch_server_for_test(
    db: &PgPool,
    server_id: Uuid,
    user_id: Uuid,
) -> Result<Option<PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT id, name, transport, url, command, args, env_vars, headers
         FROM mcp_servers WHERE id=$1 AND (user_id=$2 OR scope='global')",
    )
    .bind(server_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Lista i tool cached completi (con `input_schema`) per il path `builtin` di
/// `test_mcp_server`. Propaga l'errore SQL al chiamante (fix S82, regola H):
/// l'originale aveva `unwrap_or_default()` che mascherava errori DB facendo
/// vedere "0 tool" anche quando il problema era il DB irraggiungibile o uno
/// schema rotto - bug latente cementato dal consolidamento.
pub async fn list_cached_tools_with_schema(
    db: &PgPool,
    server_id: Uuid,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT tool_name, description, input_schema FROM mcp_server_tools WHERE server_id=$1 ORDER BY tool_name",
    )
    .bind(server_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r.try_get::<Value, _>("input_schema").unwrap_or(json!({}));
            json!({ "name": tool_name, "description": description, "inputSchema": input_schema })
        })
        .collect())
}

/// Trasforma una `Vec<McpTool>` (output di `list_tools`) nel formato atteso
/// da `upsert_discovered_tools`: `Vec<(name, description, input_schema_json)>`.
/// Punto unico (regola L, S67): prima il `.iter().map(|t| (t.name.clone(), ...))`
/// era duplicato fra mcp-core e plugin-service in `test_mcp_server`.
pub fn build_tool_upsert_args(tools: &[crate::McpTool]) -> Vec<(String, Option<String>, Value)> {
    tools
        .iter()
        .map(|t| {
            (
                t.name.clone(),
                t.description.clone(),
                serde_json::to_value(&t.input_schema).unwrap_or(json!({})),
            )
        })
        .collect()
}

/// UPSERT in `mcp_server_tools` per i tool scoperti dal test. Propaga il
/// primo errore SQL (fix S84, regola H): prima `let _ = ... .await;` ingoiava
/// ogni errore tool-per-tool, il chiamante credeva di aver aggiornato la cache
/// mentre in realta' la lista era stale. Ora se UPSERT fallisce, l'admin lo
/// vede subito invece di scoprirlo a runtime.
pub async fn upsert_discovered_tools(
    db: &PgPool,
    server_id: Uuid,
    tools: &[(String, Option<String>, Value)],
) -> Result<(), sqlx::Error> {
    for (name, description, schema) in tools {
        sqlx::query(
            "INSERT INTO mcp_server_tools (server_id, tool_name, description, input_schema, discovered_at)
             VALUES ($1,$2,$3,$4,NOW())
             ON CONFLICT (server_id, tool_name) DO UPDATE
             SET description=$3, input_schema=$4, discovered_at=NOW()",
        )
        .bind(server_id)
        .bind(name)
        .bind(description)
        .bind(schema)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// SELECT tool_name, description FROM mcp_server_tools WHERE server_id=$1 ORDER BY tool_name.
/// Ritorna i tool cached con shape JSON `{name, description}` pronto per la response.
/// Propaga l'errore SQL (fix S82, stessa motivazione di `list_cached_tools_with_schema`).
pub async fn list_cached_tools(
    db: &PgPool,
    server_id: Uuid,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT tool_name, description FROM mcp_server_tools WHERE server_id = $1 ORDER BY tool_name",
    )
    .bind(server_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|t| {
            json!({
                "name": t.try_get::<String, _>("tool_name").unwrap_or_default(),
                "description": t.try_get::<Option<String>, _>("description").unwrap_or(None),
            })
        })
        .collect())
}
