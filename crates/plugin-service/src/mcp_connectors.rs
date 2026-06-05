//! API per gestione MCP server connectors esterni.
//!
//! Endpoints:
//!   GET    /api/mcp-servers            -> lista server dell'utente
//!   POST   /api/mcp-servers            -> aggiunge server
//!   PUT    /api/mcp-servers/:id        -> aggiorna server
//!   DELETE /api/mcp-servers/:id        -> rimuove server
//!   POST   /api/mcp-servers/:id/test   -> testa connessione e ritorna tool list
//!   PUT    /api/mcp-servers/:id/toggle -> abilita/disabilita
//!
//! Internal (no auth):
//!   GET  /internal/mcp/tools/:user_id/:project_id -> load_mcp_tools_for_agent
//!   POST /internal/mcp/execute                     -> execute_mcp_tool

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use nexus_auth::Claims;
use nexus_types::{api_error, parse_user_id, ApiError, ApiResult};

// Request types e helper SQL: punto unico in nexus_mcp_client::server_storage
// (regola L / ADR 0026, Wave C1). Prima erano duplicati con mcp-core.
pub use nexus_mcp_client::server_storage::{
    CreateMcpServerRequest, ToggleRequest, UpdateMcpServerRequest,
};
use nexus_mcp_client::server_storage::{
    apply_update_and_fetch, build_config, can_manage_server, delete_mcp_server as ss_delete,
    fetch_owner_scope, fetch_server_for_test, insert_mcp_server, is_tool_allowed_by_policy,
    list_cached_tools, list_cached_tools_with_schema, list_servers_for_user, parse_json_string_set,
    row_to_json, set_enabled, upsert_discovered_tools,
};

use crate::mcp_client::{self, McpServerConfig, McpTransport};
use crate::AppState;

async fn trigger_prompt_template_tool_reassignment() {
    // Fire-and-forget: non blocchiamo l'API utente.
    // Endpoint internal lato mcp-core (trusted localhost).
    let base = std::env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:4000".to_string());
    let url = format!("{}/api/internal/prompt-templates/batch-assign-tools", base.trim_end_matches('/'));
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            // Deve solo accodare il job: la risposta dovrebbe essere rapida.
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    });
}

// ExecuteMcpToolRequest e' specifico di plugin-service (endpoint internal
// /internal/mcp/execute), non vive nel crate condiviso.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteMcpToolRequest {
    pub server_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

// build_config: punto unico in nexus_mcp_client::server_storage (regola L /
// ADR 0026). Prima duplicato qui e in mcp-core.

// parse_json_string_set + is_tool_allowed_by_policy: punto unico in
// nexus_mcp_client::server_storage (regola L / ADR 0026, step S12').

/// Helper di autorizzazione + parsing condiviso dai 4 handler mutation
/// (update/delete/toggle/test). Vedi mcp-core/mcp_connectors.rs per la
/// descrizione completa (regola L, step S14').
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

// -- Handlers --

/// GET /api/mcp-servers
pub async fn list_mcp_servers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    // Conteggio: quanti prompt template (attivi) fanno riferimento a ciascun tool_server.
    // Nota: oggi l'associazione tool è salvata in `nexus_prompt_templates.mcp_tools_json`
    // come array di oggetti {tool_name, tool_server, usage_context?}.
    let counts_rows = sqlx::query(
        r#"
        SELECT
          (elem->>'tool_server') AS tool_server,
          COUNT(DISTINCT t.key)  AS linked_templates
        FROM nexus_prompt_templates t
        JOIN LATERAL jsonb_array_elements(COALESCE(t.mcp_tools_json, '[]'::jsonb)) AS elem ON true
        WHERE t.is_active = true
          AND (elem->>'tool_server') IS NOT NULL
        GROUP BY (elem->>'tool_server')
        "#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut linked_by_server: HashMap<String, i64> = HashMap::new();
    for r in counts_rows {
        let srv: Option<String> = r.try_get("tool_server").ok();
        let cnt: Option<i64> = r.try_get("linked_templates").ok();
        if let (Some(srv), Some(cnt)) = (srv, cnt) {
            linked_by_server.insert(srv, cnt);
        }
    }

    // Punto unico SQL in nexus_mcp_client::server_storage (regola L).
    let rows = list_servers_for_user(&state.db, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut servers: Vec<Value> = Vec::new();
    for r in &rows {
        let mut s = row_to_json(r, can_manage_server(r, user_id, &claims.role));
        let server_name = r.try_get::<String, _>("name").unwrap_or_default();
        let linked_templates = linked_by_server.get(&server_name).copied().unwrap_or(0);
        let srv_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
        s["tools"] = json!(list_cached_tools(&state.db, srv_id).await);
        s["linkedTemplatesCount"] = json!(linked_templates);
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

    // Un MCP è stato rimosso: riallinea le assegnazioni tool per togliere riferimenti obsoleti.
    trigger_prompt_template_tool_reassignment().await;

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

    // Il set di tool disponibili per i prompt cambia (server abilitato/disabilitato).
    trigger_prompt_template_tool_reassignment().await;
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

    // Punto unico SQL in nexus_mcp_client::server_storage (regola L).
    let row = fetch_server_for_test(&state.db, server_id, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Server non trovato"));
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();

    // Builtin: return cached tools from DB
    if transport == "builtin" {
        let tool_list = list_cached_tools_with_schema(&state.db, server_id).await;
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
            // Save tool cache in DB
            let tools_for_upsert: Vec<(String, Option<String>, Value)> = tools
                .iter()
                .map(|t| {
                    (
                        t.name.clone(),
                        t.description.clone(),
                        serde_json::to_value(&t.input_schema).unwrap_or(json!({})),
                    )
                })
                .collect();
            upsert_discovered_tools(&state.db, server_id, &tools_for_upsert).await;

            // I tool del server potrebbero essere cambiati: riallinea le assegnazioni.
            trigger_prompt_template_tool_reassignment().await;

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

// -- Internal API (no auth) --

/// GET /internal/mcp/tools/:user_id/:project_id
/// Carica le tool definitions dai server MCP abilitati per un utente.
pub async fn load_mcp_tools_for_agent(
    State(state): State<AppState>,
    AxumPath((user_id_str, project_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = Uuid::parse_str(&user_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "user_id non valido"))?;
    let project_id = if project_id_str == "none" || project_id_str == "_" {
        None
    } else {
        Some(
            Uuid::parse_str(&project_id_str)
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "project_id non valido"))?,
        )
    };

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
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let tools: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            let server_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
            let server_name: String = r.try_get("name").unwrap_or_default();
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r
                .try_get::<Value, _>("input_schema")
                .unwrap_or(json!({"type":"object","properties":{}}));
            let policy_mode: Option<String> = r.try_get("policy_mode").unwrap_or(None);
            let policy_tools: Value = r.try_get("policy_tools").unwrap_or(json!([]));
            let policy_blocked_tools: Value =
                r.try_get("policy_blocked_tools").unwrap_or(json!([]));
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

            let short_id = server_id.to_string().replace('-', "")[..8].to_string();
            let prefixed_name = format!("mcp__{}__{}", short_id, tool_name);

            // I campi `_mcp_server_id` e `_mcp_tool_name` sono stati rimossi
            // (audit 27/05/2026): OpenAI/Anthropic strict mode rifiuta campi
            // non-standard nel tool definition ("Extra inputs are not permitted").
            // Il routing al server MCP usa il prefisso `mcp__{short_id}__{tool}`
            // parsato dal nome, quindi i campi extra erano ridondanti.
            Some(json!({
                "name": prefixed_name,
                "description": format!("[MCP: {}] {}", server_name, description.unwrap_or_default()),
                "input_schema": input_schema,
            }))
        })
        .collect();

    Ok(Json(json!({ "tools": tools })))
}

/// POST /internal/mcp/execute
/// Esegue un tool MCP dato il server_id e il nome tool originale.
pub async fn execute_mcp_tool(
    State(state): State<AppState>,
    Json(body): Json<ExecuteMcpToolRequest>,
) -> ApiResult {
    let server_id = Uuid::parse_str(&body.server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "server_id non valido"))?;

    let row = sqlx::query(
        "SELECT id, name, transport, url, command, args, env_vars, headers, plugin_instance_id
         FROM mcp_servers WHERE id=$1 AND enabled=true",
    )
    .bind(server_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            format!("Server MCP {} non trovato o disabilitato", server_id),
        ));
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();
    let plugin_instance_id: Option<Uuid> = row.try_get("plugin_instance_id").unwrap_or(None);
    let config = build_config(&server_id, &name, &transport, &row);

    // Check tool policy
    if let Some(plugin_instance_id) = plugin_instance_id {
        if let Ok(Some(policy_row)) = sqlx::query(
            "SELECT mode, tools, blocked_tools FROM plugin_instance_tool_policies WHERE plugin_instance_id = $1",
        )
        .bind(plugin_instance_id)
        .fetch_optional(&state.db)
        .await
        {
            let mode: Option<String> = policy_row.try_get("mode").ok();
            let tools: Value = policy_row.try_get("tools").unwrap_or(json!([]));
            let blocked: Value = policy_row.try_get("blocked_tools").unwrap_or(json!([]));
            let allowed_tools = parse_json_string_set(&tools);
            let blocked_tools = parse_json_string_set(&blocked);
            if !is_tool_allowed_by_policy(
                mode.as_deref(),
                &allowed_tools,
                &blocked_tools,
                &body.tool_name,
            ) {
                return Err(api_error(
                    StatusCode::FORBIDDEN,
                    format!("Tool '{}' bloccato dalla policy del plugin", body.tool_name),
                ));
            }
        }
    }

    match mcp_client::call_tool(&config, &body.tool_name, body.arguments.clone()).await {
        Ok(result) => Ok(Json(json!({
            "content": result.content,
            "isError": result.is_error,
        }))),
        Err(e) => Ok(Json(json!({
            "content": format!("Errore MCP: {}", e),
            "isError": true,
        }))),
    }
}
