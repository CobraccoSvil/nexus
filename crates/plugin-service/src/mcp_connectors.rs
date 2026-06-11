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
//!
//! La logica core degli handler vive nel punto unico
//! `nexus_mcp_client::server_endpoints` (regola L / ADR 0026, cluster E5); i
//! wrapper axum puri sono generati da `mcp_server_axum_handlers!`. Qui resta
//! solo cio' che e' specifico di plugin-service: `linkedTemplatesCount` in
//! list e la riassegnazione tool dei prompt template su delete/toggle/test.

use std::collections::HashMap;

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
use nexus_types::{api_error, parse_user_id, ApiResult};

// Logica core degli endpoint: punto unico in nexus_mcp_client::server_endpoints
// (regola L / ADR 0026, cluster E5). Prima duplicata con mcp-core.
use nexus_mcp_client::server_endpoints::{
    delete_server_core, list_servers_core, load_agent_tool_definitions, test_server_core,
    toggle_server_core,
};
// Helper SQL/policy usati direttamente da `execute_mcp_tool` (fuori dai core)
// e request type del toggle esplicito.
use nexus_mcp_client::server_storage::{
    build_config, is_tool_allowed_by_policy, parse_json_string_set, ToggleRequest,
};

use crate::mcp_client::{self};
use crate::AppState;

// Wrapper axum puri (nessun effetto specifico plugin-service) + adapter
// errori: generati dal punto unico condiviso.
nexus_mcp_client::mcp_server_axum_handlers!(AppState: error_adapter, create, update);

async fn trigger_prompt_template_tool_reassignment() {
    // Fire-and-forget: non blocchiamo l'API utente.
    // Endpoint internal lato mcp-core (trusted localhost).
    let base =
        std::env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:4000".to_string());
    let url = format!(
        "{}/api/internal/prompt-templates/batch-assign-tools",
        base.trim_end_matches('/')
    );
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

// -- Handlers con effetti specifici plugin-service --

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
    .unwrap_or_else(|e| {
        tracing::warn!("linked_templates per tool_server fallita: {e}");
        Vec::new()
    });

    let mut linked_by_server: HashMap<String, i64> = HashMap::new();
    for r in counts_rows {
        let srv: Option<String> = r.try_get("tool_server").ok();
        let cnt: Option<i64> = r.try_get("linked_templates").ok();
        if let (Some(srv), Some(cnt)) = (srv, cnt) {
            linked_by_server.insert(srv, cnt);
        }
    }

    let mut servers = list_servers_core(&state.db, user_id, &claims.role)
        .await
        .map_err(endpoint_error)?;

    // Effetto specifico plugin-service: arricchisce ogni server con il numero
    // di prompt template attivi collegati.
    for s in &mut servers {
        let server_name = s["name"].as_str().unwrap_or_default();
        let linked_templates = linked_by_server.get(server_name).copied().unwrap_or(0);
        s["linkedTemplatesCount"] = json!(linked_templates);
    }

    Ok(Json(json!({ "servers": servers })))
}

/// DELETE /api/mcp-servers/:id
pub async fn delete_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let response = delete_server_core(&state.db, user_id, &claims.role, &server_id)
        .await
        .map_err(endpoint_error)?;

    // Un MCP è stato rimosso: riallinea le assegnazioni tool per togliere riferimenti obsoleti.
    trigger_prompt_template_tool_reassignment().await;

    Ok(Json(response))
}

/// PUT /api/mcp-servers/:id/toggle
pub async fn toggle_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
    Json(body): Json<ToggleRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let response = toggle_server_core(&state.db, user_id, &claims.role, &server_id, body.enabled)
        .await
        .map_err(endpoint_error)?;

    // Il set di tool disponibili per i prompt cambia (server abilitato/disabilitato).
    trigger_prompt_template_tool_reassignment().await;
    Ok(Json(response))
}

/// POST /api/mcp-servers/:id/test
/// Testa la connessione e ritorna i tool scoperti.
pub async fn test_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let outcome = test_server_core(&state.db, user_id, &server_id)
        .await
        .map_err(endpoint_error)?;

    // I tool del server potrebbero essere cambiati: riallinea le assegnazioni.
    if outcome.discovered_tools.is_some() {
        trigger_prompt_template_tool_reassignment().await;
    }

    Ok(Json(outcome.response))
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

    // Prefisso `mcp__{short_id}__{tool}`: short-id dell'uuid del server.
    let tools = load_agent_tool_definitions(&state.db, user_id, project_id, |server_id, _| {
        server_id.to_string().replace('-', "")[..8].to_string()
    })
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
