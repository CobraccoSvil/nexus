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
//!
//! La logica core degli handler vive nel punto unico
//! `nexus_mcp_client::server_endpoints` (regola L / ADR 0026, cluster E5); i
//! wrapper axum puri sono generati da `mcp_server_axum_handlers!`. Qui resta
//! solo cio' che e' specifico di mcp-core: l'indicizzazione semantica Qdrant
//! dei tool scoperti dal test e l'integrazione AgentLoop.

use axum::{
    extract::{Extension, Path as AxumPath, State},
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{parse_user_id, ApiResult},
    mcp_client::{self},
    AppState,
};

// Logica core degli endpoint: punto unico in nexus_mcp_client::server_endpoints
// (regola L / ADR 0026, cluster E5). Prima duplicata con plugin-service.
use nexus_mcp_client::server_endpoints::{load_agent_tool_definitions, test_server_core};
// Helper SQL/policy usati direttamente da `execute_mcp_tool` (fuori dai core).
use nexus_mcp_client::server_storage::{
    build_config, is_tool_allowed_by_policy, parse_json_string_set,
};

// Wrapper axum puri (nessun effetto specifico mcp-core) + adapter errori:
// generati dal punto unico condiviso.
nexus_mcp_client::mcp_server_axum_handlers!(AppState: error_adapter, list, create, update, delete, toggle);

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

    // Effetto specifico mcp-core: indicizzazione semantica Qdrant
    // (fire-and-forget) dei tool appena scoperti.
    if let Some(tools) = &outcome.discovered_tools {
        let db_idx = state.db.clone();
        let sname_idx = outcome.server_name.clone();
        let server_id = outcome.server_id;
        let tools_meta: Vec<(String, String)> = tools
            .iter()
            .map(|t| (t.name.clone(), t.description.clone().unwrap_or_default()))
            .collect();
        tokio::spawn(async move {
            // Legge scope dal DB (non disponibile nella query corrente)
            let scope: String =
                sqlx::query_scalar("SELECT COALESCE(scope, 'user') FROM mcp_servers WHERE id=$1")
                    .bind(server_id)
                    .fetch_optional(&db_idx)
                    .await
                    .unwrap_or(None)
                    .unwrap_or_else(|| "user".to_string());
            for (tname, tdesc) in &tools_meta {
                if let Err(e) = crate::nexus_builtin::index_tool(
                    &db_idx, server_id, &sname_idx, tname, tdesc, &scope,
                )
                .await
                {
                    tracing::debug!("index_tool {}/{}: {}", sname_idx, tname, e);
                }
            }
        });
    }

    Ok(Json(outcome.response))
}

// ── Integrazione con AgentLoop ─────────────────────────────────────────────

/// Carica le tool definitions dai server MCP abilitati per un utente.
/// Ritorna una stringa JSON array da concatenare a AGENT_TOOLS_JSON.
pub async fn load_mcp_tools_for_agent(
    db: &sqlx::PgPool,
    user_id: Uuid,
    project_id: Option<Uuid>,
) -> Vec<Value> {
    // Prefissa il tool con "mcp__{label}__":
    // - Nexus Builtin -> "nexus" (leggibile)
    // - server esterni -> slug del nome server (max 12 char)
    load_agent_tool_definitions(db, user_id, project_id, |server_id, server_name| {
        if server_id.to_string() == crate::nexus_builtin::NEXUS_BUILTIN_SERVER_ID_STR {
            "nexus".to_string()
        } else {
            let slug: String = server_name
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect::<String>()
                .split('_')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("_");
            if slug.len() > 12 {
                slug[..12].to_string()
            } else {
                slug
            }
        }
    })
    .await
    .unwrap_or_default()
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
    let mut config = build_config(&server_id, &name, &transport, &row);

    // BUG d2 (cause B+C): iniezione args SCOPED sul solo server MCP esterno
    // @playwright/mcp (slug "playwright-stdio"). Lo slug e' riconosciuto dal
    // punto unico detect_legacy_catalog_slug a partire da (transport, command,
    // args) della row. Aggiungiamo --headless --isolated --no-sandbox e
    // --executable-path <chromium dalla cache>, derivato dal punto unico
    // playwright_env. NESSUN altro server viene toccato. Se il Chromium manca,
    // si lascia il config invariato e si lascia che il server fallisca con il
    // suo errore: NON sostituiamo silenziosamente (regola G/H).
    {
        let command: Option<String> = row.try_get("command").unwrap_or(None);
        let args_val: Value = row.try_get::<Value, _>("args").unwrap_or(json!([]));
        let slug = crate::plugins::detect_legacy_catalog_slug(
            &transport,
            None,
            command.as_deref(),
            &args_val,
        );
        if crate::playwright_env::is_playwright_mcp_slug(slug) {
            if let mcp_client::McpTransport::Stdio { args, .. } = &mut config.transport {
                match crate::playwright_env::playwright_mcp_extra_args() {
                    Ok(extra) => {
                        // Evita duplicazioni se gia' presenti (idempotente).
                        if !args.iter().any(|a| a == "--executable-path") {
                            args.extend(extra);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %name,
                            error = %e,
                            "playwright-stdio: Chromium non risolto, args non iniettati"
                        );
                    }
                }
            }
        }
    }

    if let Some(plugin_instance_id) = plugin_instance_id {
        if let Ok(Some(policy_row)) = sqlx::query(
            "SELECT mode, tools, blocked_tools FROM plugin_instance_tool_policies WHERE plugin_instance_id = $1",
        )
        .bind(plugin_instance_id)
        .fetch_optional(db)
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

    let stdio_timeout = mcp_client::resolve_stdio_timeout(db).await;
    match mcp_client::call_tool(&config, tool_name, arguments, stdio_timeout).await {
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
