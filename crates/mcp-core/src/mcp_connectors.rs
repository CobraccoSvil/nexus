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

use std::collections::HashSet;

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    mcp_client::{self, McpServerConfig, McpTransport},
    AppState,
};

// Request types e helper SQL: punto unico in nexus_mcp_client::server_storage
// (regola L / ADR 0026, Wave C1). Prima erano duplicati con plugin-service.
pub use nexus_mcp_client::server_storage::{
    CreateMcpServerRequest, ToggleRequest, UpdateMcpServerRequest,
};
use nexus_mcp_client::server_storage::{
    apply_update_and_fetch, build_config, build_tool_upsert_args, can_manage_server,
    delete_mcp_server as ss_delete, fetch_owner_scope, fetch_server_for_test, insert_mcp_server,
    is_tool_allowed_by_policy, list_cached_tools, list_cached_tools_with_schema,
    list_servers_for_user, parse_json_string_set, row_to_json, set_enabled, upsert_discovered_tools,
};

// build_config: punto unico in nexus_mcp_client::server_storage (regola L /
// ADR 0026). Prima duplicato qui e in plugin-service.

// parse_json_string_set + is_tool_allowed_by_policy: punto unico in
// nexus_mcp_client::server_storage (regola L / ADR 0026, step S12').

/// Pattern condiviso degli handler che modificano un server MCP:
/// 1. parse_user_id dal claims
/// 2. parse Uuid del path :id
/// 3. fetch_owner_scope (NOT_FOUND se assente)
/// 4. can_manage_server (NOT_FOUND mascherato se non autorizzato)
///
/// Punto unico (regola L, step S14') per i 4 handler `update`/`delete`/
/// `toggle`/`test` che condividevano questo prologo riga-per-riga.
/// Ritorna `(user_id, server_id)` se autorizzato.
async fn authorize_server_mutation(
    state: &AppState,
    claims: &Claims,
    server_id_str: &str,
) -> Result<(Uuid, Uuid), ApiError> {
    let user_id = parse_user_id(claims)?;
    let server_id = Uuid::parse_str(server_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server id non valido"))?;
    let existing = fetch_owner_scope(&state.db, server_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(existing) = existing else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };
    if !can_manage_server(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non modificabile"));
    }
    Ok((user_id, server_id))
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/mcp-servers
pub async fn list_mcp_servers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    // Punto unico SQL in nexus_mcp_client::server_storage (regola L).
    let rows = list_servers_for_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut servers: Vec<Value> = Vec::new();
    for r in &rows {
        let mut s = row_to_json(r, can_manage_server(r, user_id, &claims.role));
        let srv_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
        // Fix S82: propaga errore SQL invece di mascherarlo come "0 tool".
        s["tools"] = json!(list_cached_tools(&state.db, srv_id)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?);
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
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Transport deve essere 'http' o 'stdio'",
        ));
    }
    if body.transport == "http" && body.url.is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "URL richiesto per transport HTTP",
        ));
    }
    if body.transport == "stdio" && body.command.is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Command richiesto per transport stdio",
        ));
    }

    // Punto unico SQL in nexus_mcp_client::server_storage (regola L).
    let row = insert_mcp_server(&state.db, user_id, &body)
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
    let (_user_id, server_id) = authorize_server_mutation(&state, &claims, &server_id).await?;

    let row = apply_update_and_fetch(&state.db, server_id, &body)
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
    let (_user_id, server_id) = authorize_server_mutation(&state, &claims, &server_id).await?;

    ss_delete(&state.db, server_id)
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
    let (_user_id, server_id) = authorize_server_mutation(&state, &claims, &server_id).await?;

    set_enabled(&state.db, server_id, body.enabled)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(
        json!({ "id": server_id.to_string(), "enabled": body.enabled }),
    ))
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

    // Punto unico SQL in nexus_mcp_client::server_storage (regola L).
    let row = fetch_server_for_test(&state.db, server_id, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();

    // Per il server builtin: restituisce i tool già cached nel DB senza chiamate esterne
    if transport == "builtin" {
        // Fix S82: propaga errore SQL.
        let tool_list = list_cached_tools_with_schema(&state.db, server_id)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
            // Salva/aggiorna tool cache nel DB (punto unico build_tool_upsert_args, S67).
            let tools_for_upsert = build_tool_upsert_args(&tools);
            // Fix S84: propaga errore SQL invece di mascherare.
            upsert_discovered_tools(&state.db, server_id, &tools_for_upsert)
                .await
                .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
                            &db_idx,
                            &neural_idx,
                            server_id,
                            &sname_idx,
                            tname,
                            tdesc,
                            &scope,
                        )
                        .await
                        {
                            tracing::debug!("index_tool {}/{}: {}", sname_idx, tname, e);
                        }
                    }
                });
            }

            let tool_list: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
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
        return format!(
            "Errore: server MCP {} non trovato o disabilitato",
            server_id
        );
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
