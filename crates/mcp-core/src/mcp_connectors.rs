//! API per gestione MCP server connectors esterni.
//!
//! Endpoints:
//!   GET    /api/mcp-servers            → lista server dell'utente
//!   POST   /api/mcp-servers            → aggiunge server
//!   PUT    /api/mcp-servers/:id        → aggiorna server
//!   DELETE /api/mcp-servers/:id        → rimuove server
//!   POST   /api/mcp-servers/:id/test   → testa connessione e ritorna tool list
//!   PUT    /api/mcp-servers/:id/toggle → abilita/disabilita
//!
//! Integrazione con AgentLoop:
//!   `load_mcp_tools_for_agent()` → carica tool definitions dai server abilitati
//!   `execute_mcp_tool()`         → esegue un tool su un server esterno

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiResult},
    mcp_client::{self, McpServerConfig, McpTransport},
    AppState,
};

// ── Request/Response types ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMcpServerRequest {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub transport: String,        // "http" | "stdio"
    pub url: Option<String>,      // per HTTP
    pub command: Option<String>,  // per stdio
    pub args: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,
    pub headers: Option<HashMap<String, String>>,
    pub scope: Option<String>,    // "user" | "project"
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

// ── Helpers ────────────────────────────────────────────────────────────────

fn row_to_json(r: &sqlx::postgres::PgRow, can_manage: bool) -> Value {
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

fn can_manage_server(
    row: &sqlx::postgres::PgRow,
    user_id: Uuid,
    role: &str,
) -> bool {
    let owner_user_id: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let scope: String = row
        .try_get("scope")
        .unwrap_or_else(|_| "user".to_string());

    owner_user_id == Some(user_id) || (scope == "global" && role == "admin")
}

fn build_config(id: &Uuid, name: &str, transport: &str, row: &sqlx::postgres::PgRow) -> McpServerConfig {
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
    let args: Value = row.try_get::<Value, _>("args").unwrap_or(json!([]));
    let env_vars: Value = row.try_get::<Value, _>("env_vars").unwrap_or(json!({}));
    let headers: Value = row.try_get::<Value, _>("headers").unwrap_or(json!({}));

    let mcp_transport = if transport == "stdio" {
        McpTransport::Stdio {
            command: command.unwrap_or_default(),
            args: args
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            env_vars: env_vars
                .as_object()
                .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
                .unwrap_or_default(),
        }
    } else {
        McpTransport::Http {
            url: url.unwrap_or_default(),
            headers: headers
                .as_object()
                .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
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

fn is_tool_allowed_by_policy(
    mode: Option<&str>,
    allowed_tools: &HashSet<String>,
    blocked_tools: &HashSet<String>,
    tool_name: &str,
) -> bool {
    if blocked_tools.contains(tool_name) {
        return false;
    }
    match mode.unwrap_or("all") {
        "allowlist" => {
            if allowed_tools.is_empty() {
                true
            } else {
                allowed_tools.contains(tool_name)
            }
        }
        "denylist" | "all" => true,
        _ => true,
    }
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/mcp-servers
pub async fn list_mcp_servers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let rows = sqlx::query(
        "SELECT id, plugin_instance_id, name, description, icon_url, transport, url, command, args, env_vars, headers,
                enabled, scope, user_id, created_at
         FROM mcp_servers
         WHERE user_id = $1 OR scope = 'global'
         ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Per ogni server, legge anche i tool cached
    let mut servers: Vec<Value> = Vec::new();
    for r in &rows {
        let mut s = row_to_json(r, can_manage_server(r, user_id, &claims.role));
        let srv_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
        let tools = sqlx::query(
            "SELECT tool_name, description FROM mcp_server_tools WHERE server_id = $1 ORDER BY tool_name",
        )
        .bind(srv_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .iter()
        .map(|t| json!({
            "name": t.try_get::<String, _>("tool_name").unwrap_or_default(),
            "description": t.try_get::<Option<String>, _>("description").unwrap_or(None),
        }))
        .collect::<Vec<_>>();

        s["tools"] = json!(tools);
        servers.push(s);
    }

    Ok(Json(json!({ "servers": servers })))
}

/// POST /api/mcp-servers
pub async fn create_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateMcpServerRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    if body.transport != "http" && body.transport != "stdio" {
        return Err(api_error(StatusCode::BAD_REQUEST, "Transport deve essere 'http' o 'stdio'"));
    }
    if body.transport == "http" && body.url.is_none() {
        return Err(api_error(StatusCode::BAD_REQUEST, "URL richiesto per transport HTTP"));
    }
    if body.transport == "stdio" && body.command.is_none() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Command richiesto per transport stdio"));
    }

    let scope = body.scope.as_deref().unwrap_or("user");
    let project_id: Option<Uuid> = body
        .project_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let args_json = json!(body.args.unwrap_or_default());
    let env_json = json!(body.env_vars.unwrap_or_default());
    let headers_json = json!(body.headers.unwrap_or_default());

    let row = sqlx::query(
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
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(row_to_json(&row, true)))
}

/// PUT /api/mcp-servers/:id
pub async fn update_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
    Json(body): Json<UpdateMcpServerRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let server_id = Uuid::parse_str(&server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server id non valido"))?;

    let existing = sqlx::query("SELECT id, user_id, scope FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(existing) = existing else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };
    if !can_manage_server(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non modificabile"));
    }

    // Aggiorna solo i campi forniti
    if let Some(name) = &body.name {
        sqlx::query("UPDATE mcp_servers SET name=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(name).execute(&state.db).await.ok();
    }
    if let Some(desc) = &body.description {
        sqlx::query("UPDATE mcp_servers SET description=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(desc).execute(&state.db).await.ok();
    }
    if let Some(url) = &body.url {
        sqlx::query("UPDATE mcp_servers SET url=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(url).execute(&state.db).await.ok();
    }
    if let Some(cmd) = &body.command {
        sqlx::query("UPDATE mcp_servers SET command=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(cmd).execute(&state.db).await.ok();
    }
    if let Some(args) = &body.args {
        let v = json!(args);
        sqlx::query("UPDATE mcp_servers SET args=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(v).execute(&state.db).await.ok();
    }
    if let Some(env) = &body.env_vars {
        let v = json!(env);
        sqlx::query("UPDATE mcp_servers SET env_vars=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(v).execute(&state.db).await.ok();
    }
    if let Some(headers) = &body.headers {
        let v = json!(headers);
        sqlx::query("UPDATE mcp_servers SET headers=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(v).execute(&state.db).await.ok();
    }
    if let Some(enabled) = body.enabled {
        sqlx::query("UPDATE mcp_servers SET enabled=$2, updated_at=NOW() WHERE id=$1")
            .bind(server_id).bind(enabled).execute(&state.db).await.ok();
    }

    let row = sqlx::query(
        "SELECT id, plugin_instance_id, name, description, icon_url, transport, url, command, args,
                env_vars, headers, enabled, scope, user_id, created_at
         FROM mcp_servers WHERE id=$1",
    )
    .bind(server_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(row_to_json(&row, true)))
}

/// DELETE /api/mcp-servers/:id
pub async fn delete_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let server_id = Uuid::parse_str(&server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server id non valido"))?;

    let existing = sqlx::query("SELECT id, user_id, scope FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(existing) = existing else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };
    if !can_manage_server(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non modificabile"));
    }

    sqlx::query("DELETE FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "deleted": true })))
}

/// PUT /api/mcp-servers/:id/toggle
pub async fn toggle_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
    Json(body): Json<ToggleRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let server_id = Uuid::parse_str(&server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server id non valido"))?;

    let existing = sqlx::query("SELECT id, user_id, scope FROM mcp_servers WHERE id=$1")
        .bind(server_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(existing) = existing else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };
    if !can_manage_server(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non modificabile"));
    }

    let result = sqlx::query(
        "UPDATE mcp_servers
         SET enabled = $2, updated_at = NOW()
         WHERE id = $1",
    )
        .bind(server_id)
        .bind(body.enabled)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Server non trovato o non modificabile",
        ));
    }

    Ok(Json(json!({ "id": server_id.to_string(), "enabled": body.enabled })))
}

/// POST /api/mcp-servers/:id/test
/// Testa la connessione e ritorna i tool scoperti.
pub async fn test_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let server_id = Uuid::parse_str(&server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server id non valido"))?;

    let row = sqlx::query(
        "SELECT id, name, transport, url, command, args, env_vars, headers
         FROM mcp_servers WHERE id=$1 AND (user_id=$2 OR scope='global')",
    )
    .bind(server_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();

    // Per il server builtin: restituisce i tool già cached nel DB senza chiamate esterne
    if transport == "builtin" {
        let cached_tools = sqlx::query(
            "SELECT tool_name, description, input_schema FROM mcp_server_tools WHERE server_id=$1 ORDER BY tool_name"
        )
        .bind(server_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let tool_list: Vec<Value> = cached_tools.iter().map(|r| {
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r.try_get::<Value, _>("input_schema").unwrap_or(json!({}));
            json!({ "name": tool_name, "description": description, "inputSchema": input_schema })
        }).collect();

        return Ok(Json(json!({
            "success": true,
            "toolCount": tool_list.len(),
            "tools": tool_list,
            "builtin": true,
        })));
    }

    let config = build_config(&server_id, &name, &transport, &row);

    match mcp_client::list_tools(&config).await {
        Ok(tools) => {
            // Salva/aggiorna tool cache nel DB
            for t in &tools {
                let schema = serde_json::to_value(&t.input_schema).unwrap_or(json!({}));
                sqlx::query(
                    "INSERT INTO mcp_server_tools (server_id, tool_name, description, input_schema, discovered_at)
                     VALUES ($1,$2,$3,$4,NOW())
                     ON CONFLICT (server_id, tool_name) DO UPDATE
                     SET description=$3, input_schema=$4, discovered_at=NOW()",
                )
                .bind(server_id)
                .bind(&t.name)
                .bind(&t.description)
                .bind(schema)
                .execute(&state.db)
                .await
                .ok();
            }

            // Indicizzazione semantica Qdrant (fire-and-forget)
            {
                let db_idx = state.db.clone();
                let neural_idx = state.orchestrator.neural.clone();
                let sname_idx = name.clone();
                let tools_meta: Vec<(String, String)> = tools
                    .iter()
                    .map(|t| (t.name.clone(), t.description.clone().unwrap_or_default()))
                    .collect();
                tokio::spawn(async move {
                    // Legge scope dal DB (non disponibile nella query corrente)
                    let scope: String = sqlx::query_scalar(
                        "SELECT COALESCE(scope, 'user') FROM mcp_servers WHERE id=$1",
                    )
                    .bind(server_id)
                    .fetch_optional(&db_idx)
                    .await
                    .unwrap_or(None)
                    .unwrap_or_else(|| "user".to_string());
                    for (tname, tdesc) in &tools_meta {
                        if let Err(e) = crate::nexus_builtin::index_tool(
                            &db_idx, &neural_idx, server_id, &sname_idx, tname, tdesc, &scope,
                        )
                        .await
                        {
                            tracing::debug!(
                                "index_tool {}/{}: {}",
                                sname_idx, tname, e
                            );
                        }
                    }
                });
            }

            let tool_list: Vec<Value> = tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                }))
                .collect();

            Ok(Json(json!({
                "success": true,
                "toolCount": tool_list.len(),
                "tools": tool_list,
            })))
        }
        Err(e) => Ok(Json(json!({
            "success": false,
            "error": e.to_string(),
            "tools": [],
        }))),
    }
}

// ── Integrazione con AgentLoop ─────────────────────────────────────────────

/// Carica le tool definitions dai server MCP abilitati per un utente.
/// Ritorna una stringa JSON array da concatenare a AGENT_TOOLS_JSON.
pub async fn load_mcp_tools_for_agent(
    db: &sqlx::PgPool,
    user_id: Uuid,
    project_id: Option<Uuid>,
) -> Vec<Value> {
    let rows = sqlx::query(
        "SELECT s.id, s.name, t.tool_name, t.description, t.input_schema,
                p.mode AS policy_mode, p.tools AS policy_tools, p.blocked_tools AS policy_blocked_tools
         FROM mcp_servers s
         JOIN mcp_server_tools t ON t.server_id = s.id
         LEFT JOIN plugin_instance_tool_policies p ON p.plugin_instance_id = s.plugin_instance_id
         WHERE s.enabled = true AND (s.user_id = $1 OR s.scope = 'global'
               OR (s.scope = 'project' AND s.project_id = $2))
         ORDER BY s.name, t.tool_name",
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    rows.iter()
        .filter_map(|r| {
            let server_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
            let server_name: String = r.try_get("name").unwrap_or_default();
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r.try_get::<Value, _>("input_schema").unwrap_or(json!({"type":"object","properties":{}}));
            let policy_mode: Option<String> = r.try_get("policy_mode").unwrap_or(None);
            let policy_tools: Value = r.try_get("policy_tools").unwrap_or(json!([]));
            let policy_blocked_tools: Value = r.try_get("policy_blocked_tools").unwrap_or(json!([]));
            let allowed_tools = parse_json_string_set(&policy_tools);
            let blocked_tools = parse_json_string_set(&policy_blocked_tools);

            if !is_tool_allowed_by_policy(
                policy_mode.as_deref(),
                &allowed_tools,
                &blocked_tools,
                &tool_name,
            ) {
                return None;
            }

            // Prefissa il tool con "mcp__{label}__":
            // - Nexus Builtin -> "nexus" (leggibile)
            // - server esterni -> slug del nome server (max 12 char)
            let label = if server_id.to_string() == crate::nexus_builtin::NEXUS_BUILTIN_SERVER_ID_STR {
                "nexus".to_string()
            } else {
                let slug: String = server_name.to_lowercase()
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect::<String>()
                    .split('_')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("_");
                if slug.len() > 12 { slug[..12].to_string() } else { slug }
            };
            let prefixed_name = format!("mcp__{}__{}", label, tool_name);

            // I campi `_mcp_server_id` e `_mcp_tool_name` sono stati rimossi
            // (audit 27/05/2026): OpenAI/Anthropic strict mode rifiuta campi
            // non-standard nel tool definition ("Extra inputs are not permitted").
            // Il routing al server MCP usa il prefisso `mcp__{label}__{tool}`
            // parsato dal nome, quindi i campi extra erano ridondanti.
            Some(json!({
                "name": prefixed_name,
                "description": format!("[MCP: {}] {}", server_name, description.unwrap_or_default()),
                "input_schema": input_schema,
            }))
        })
        .collect()
}

/// Esegue un tool MCP dato il server_id e il nome tool originale.
pub async fn execute_mcp_tool(
    db: &sqlx::PgPool,
    server_id: Uuid,
    tool_name: &str,
    arguments: Value,
) -> String {
    let row = sqlx::query(
        "SELECT id, name, transport, url, command, args, env_vars, headers, plugin_instance_id
         FROM mcp_servers WHERE id=$1 AND enabled=true",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return format!("Errore: server MCP {} non trovato o disabilitato", server_id);
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();
    let plugin_instance_id: Option<Uuid> = row.try_get("plugin_instance_id").unwrap_or(None);
    let config = build_config(&server_id, &name, &transport, &row);

    if let Some(plugin_instance_id) = plugin_instance_id {
        if let Ok(policy_row) = sqlx::query(
            "SELECT mode, tools, blocked_tools FROM plugin_instance_tool_policies WHERE plugin_instance_id = $1",
        )
        .bind(plugin_instance_id)
        .fetch_optional(db)
        .await
        {
            if let Some(policy_row) = policy_row {
                let mode: Option<String> = policy_row.try_get("mode").ok();
                let tools: Value = policy_row.try_get("tools").unwrap_or(json!([]));
                let blocked: Value = policy_row.try_get("blocked_tools").unwrap_or(json!([]));
                let allowed_tools = parse_json_string_set(&tools);
                let blocked_tools = parse_json_string_set(&blocked);
                if !is_tool_allowed_by_policy(
                    mode.as_deref(),
                    &allowed_tools,
                    &blocked_tools,
                    tool_name,
                ) {
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO plugin_audit_events
                            (plugin_instance_id, action, status, message, payload)
                        VALUES ($1, 'call_tool', 'denied', $2, $3)
                        "#,
                    )
                    .bind(plugin_instance_id)
                    .bind(format!("Tool '{}' bloccato da policy", tool_name))
                    .bind(json!({ "toolName": tool_name, "serverId": server_id.to_string() }))
                    .execute(db)
                    .await;

                    return format!(
                        "Errore: tool MCP '{}' non consentito dalla policy del plugin",
                        tool_name
                    );
                }
            }
        }
    }

    match mcp_client::call_tool(&config, tool_name, arguments).await {
        Ok(result) => {
            if let Some(plugin_instance_id) = plugin_instance_id {
                let _ = sqlx::query(
                    r#"
                    INSERT INTO plugin_audit_events
                        (plugin_instance_id, action, status, message, payload)
                    VALUES ($1, 'call_tool', $2, $3, $4)
                    "#,
                )
                .bind(plugin_instance_id)
                .bind(if result.is_error { "error" } else { "ok" })
                .bind(if result.is_error {
                    format!("Tool '{}' ha restituito errore", tool_name)
                } else {
                    format!("Tool '{}' eseguito", tool_name)
                })
                .bind(json!({ "toolName": tool_name, "serverId": server_id.to_string() }))
                .execute(db)
                .await;
            }
            if result.is_error {
                format!("Errore dal server MCP '{}': {}", name, result.content)
            } else {
                result.content
            }
        }
        Err(e) => format!("Errore chiamata MCP '{}': {}", name, e),
    }
}
