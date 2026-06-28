//! Logica core degli endpoint CRUD/test per `mcp_servers`: punto unico
//! (regola L / ADR 0026, cluster E5).
//!
//! Prima i corpi degli handler `list/create/update/delete/toggle/test` e il
//! filtro tool-per-agente vivevano DUPLICATI riga-per-riga in:
//!   - crates/mcp-core/src/mcp_connectors.rs
//!   - crates/plugin-service/src/mcp_connectors.rs
//!
//! Ora i due `mcp_connectors.rs` sono wrapper axum sottili: convertono
//! `Claims` in `(user_id, role)` e `EndpointError` (status u16 + messaggio,
//! nessuna dipendenza axum qui) in `ApiError`. Gli effetti specifici di un
//! solo servizio restano nei wrapper:
//!   - plugin-service: `linkedTemplatesCount` in list e
//!     `trigger_prompt_template_tool_reassignment` su delete/toggle/test
//!   - mcp-core: indicizzazione semantica Qdrant dei tool scoperti dal test

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::server_storage::{
    apply_update_and_fetch, build_config, build_tool_upsert_args, can_manage_server,
    delete_mcp_server, fetch_owner_scope, fetch_server_for_test, insert_mcp_server,
    is_tool_allowed_by_policy, list_cached_tools, list_cached_tools_with_schema,
    list_servers_for_user, parse_json_string_set, row_to_json, set_enabled,
    upsert_discovered_tools, CreateMcpServerRequest, UpdateMcpServerRequest,
};
use crate::McpTool;

/// Errore endpoint senza dipendenza axum: (status HTTP, messaggio).
pub type EndpointError = (u16, String);

const BAD_REQUEST: u16 = 400;
const NOT_FOUND: u16 = 404;
const INTERNAL_SERVER_ERROR: u16 = 500;

fn internal(e: impl std::fmt::Display) -> EndpointError {
    (INTERNAL_SERVER_ERROR, e.to_string())
}

/// Pattern condiviso degli handler che modificano un server MCP:
/// 1. parse Uuid del path :id
/// 2. fetch_owner_scope (NOT_FOUND se assente)
/// 3. can_manage_server (NOT_FOUND mascherato se non autorizzato)
///
/// Ritorna il `server_id` parsato se l'utente e' autorizzato.
async fn authorize_server_mutation(
    db: &PgPool,
    user_id: Uuid,
    role: &str,
    server_id_str: &str,
) -> Result<Uuid, EndpointError> {
    let server_id = Uuid::parse_str(server_id_str)
        .map_err(|_| (BAD_REQUEST, "Server id non valido".to_string()))?;
    let existing = fetch_owner_scope(db, server_id).await.map_err(internal)?;
    let Some(existing) = existing else {
        return Err((NOT_FOUND, "Server non trovato".to_string()));
    };
    if !can_manage_server(&existing, user_id, role) {
        return Err((NOT_FOUND, "Server non modificabile".to_string()));
    }
    Ok(server_id)
}

/// GET /api/mcp-servers: lista i server visibili all'utente, ciascuno con i
/// tool cached. Ritorna i singoli oggetti JSON cosi' che i wrapper possano
/// arricchirli (es. `linkedTemplatesCount` in plugin-service).
pub async fn list_servers_core(
    db: &PgPool,
    user_id: Uuid,
    role: &str,
) -> Result<Vec<Value>, EndpointError> {
    let rows = list_servers_for_user(db, user_id).await.map_err(internal)?;

    let mut servers: Vec<Value> = Vec::new();
    for r in &rows {
        let mut s = row_to_json(r, can_manage_server(r, user_id, role));
        let srv_id: Uuid = r.try_get("id").unwrap_or(Uuid::nil());
        // Fix S82: propaga errore SQL invece di mascherarlo come "0 tool".
        s["tools"] = json!(list_cached_tools(db, srv_id).await.map_err(internal)?);
        servers.push(s);
    }
    Ok(servers)
}

/// POST /api/mcp-servers: valida transport e crea il server.
pub async fn create_server_core(
    db: &PgPool,
    user_id: Uuid,
    body: &CreateMcpServerRequest,
) -> Result<Value, EndpointError> {
    if body.transport != "http" && body.transport != "stdio" {
        return Err((
            BAD_REQUEST,
            "Transport deve essere 'http' o 'stdio'".to_string(),
        ));
    }
    if body.transport == "http" && body.url.is_none() {
        return Err((BAD_REQUEST, "URL richiesto per transport HTTP".to_string()));
    }
    if body.transport == "stdio" && body.command.is_none() {
        return Err((
            BAD_REQUEST,
            "Command richiesto per transport stdio".to_string(),
        ));
    }

    let row = insert_mcp_server(db, user_id, body)
        .await
        .map_err(internal)?;
    Ok(row_to_json(&row, true))
}

/// PUT /api/mcp-servers/:id
pub async fn update_server_core(
    db: &PgPool,
    user_id: Uuid,
    role: &str,
    server_id_str: &str,
    body: &UpdateMcpServerRequest,
) -> Result<Value, EndpointError> {
    let server_id = authorize_server_mutation(db, user_id, role, server_id_str).await?;
    let row = apply_update_and_fetch(db, server_id, body)
        .await
        .map_err(internal)?;
    Ok(row_to_json(&row, true))
}

/// DELETE /api/mcp-servers/:id
pub async fn delete_server_core(
    db: &PgPool,
    user_id: Uuid,
    role: &str,
    server_id_str: &str,
) -> Result<Value, EndpointError> {
    let server_id = authorize_server_mutation(db, user_id, role, server_id_str).await?;
    delete_mcp_server(db, server_id).await.map_err(internal)?;
    Ok(json!({ "deleted": true }))
}

/// PUT /api/mcp-servers/:id/toggle
pub async fn toggle_server_core(
    db: &PgPool,
    user_id: Uuid,
    role: &str,
    server_id_str: &str,
    enabled: bool,
) -> Result<Value, EndpointError> {
    let server_id = authorize_server_mutation(db, user_id, role, server_id_str).await?;
    set_enabled(db, server_id, enabled)
        .await
        .map_err(internal)?;
    Ok(json!({ "id": server_id.to_string(), "enabled": enabled }))
}

/// Esito di `test_server_core`: oltre alla response JSON, espone ai wrapper
/// cio' che serve per gli effetti specifici del singolo servizio.
pub struct TestServerOutcome {
    /// Body JSON della risposta HTTP (identico tra i due servizi).
    pub response: Value,
    pub server_id: Uuid,
    pub server_name: String,
    /// `Some(tools)` SOLO quando il test e' riuscito su un server non-builtin
    /// (tool list appena scoperta e upsertata in `mcp_server_tools`): e' il
    /// gancio per gli effetti post-discovery dei wrapper (indicizzazione
    /// Qdrant in mcp-core, riassegnazione template in plugin-service).
    pub discovered_tools: Option<Vec<McpTool>>,
}

/// POST /api/mcp-servers/:id/test: testa la connessione e ritorna i tool
/// scoperti (o quelli cached per il transport builtin).
pub async fn test_server_core(
    db: &PgPool,
    user_id: Uuid,
    server_id_str: &str,
) -> Result<TestServerOutcome, EndpointError> {
    let server_id = Uuid::parse_str(server_id_str)
        .map_err(|_| (BAD_REQUEST, "Server id non valido".to_string()))?;

    let row = fetch_server_for_test(db, server_id, user_id)
        .await
        .map_err(internal)?;
    let Some(row) = row else {
        return Err((NOT_FOUND, "Server non trovato".to_string()));
    };

    let transport: String = row.try_get("transport").unwrap_or_default();
    let name: String = row.try_get("name").unwrap_or_default();

    // Per il server builtin: restituisce i tool gia' cached nel DB senza
    // chiamate esterne.
    if transport == "builtin" {
        // Fix S82: propaga errore SQL.
        let tool_list = list_cached_tools_with_schema(db, server_id)
            .await
            .map_err(internal)?;
        return Ok(TestServerOutcome {
            response: json!({
                "success": true,
                "toolCount": tool_list.len(),
                "tools": tool_list,
                "builtin": true,
            }),
            server_id,
            server_name: name,
            discovered_tools: None,
        });
    }

    let config = build_config(&server_id, &name, &transport, &row);

    let stdio_timeout = crate::resolve_stdio_timeout(db).await;
    match crate::list_tools(&config, stdio_timeout).await {
        Ok(tools) => {
            // Salva/aggiorna tool cache nel DB (punto unico build_tool_upsert_args, S67).
            let tools_for_upsert = build_tool_upsert_args(&tools);
            // Fix S84: propaga errore SQL invece di mascherare.
            upsert_discovered_tools(db, server_id, &tools_for_upsert)
                .await
                .map_err(internal)?;

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

            Ok(TestServerOutcome {
                response: json!({
                    "success": true,
                    "toolCount": tool_list.len(),
                    "tools": tool_list,
                }),
                server_id,
                server_name: name,
                discovered_tools: Some(tools),
            })
        }
        Err(e) => Ok(TestServerOutcome {
            response: json!({
                "success": false,
                "error": e.to_string(),
                "tools": [],
            }),
            server_id,
            server_name: name,
            discovered_tools: None,
        }),
    }
}

/// Carica le tool definitions dai server MCP abilitati per un utente,
/// applicando le policy per-plugin. `label_for(server_id, server_name)`
/// decide il label del prefisso `mcp__{label}__{tool}`: mcp-core usa lo slug
/// leggibile del nome server ("nexus" per il builtin), plugin-service lo
/// short-id dell'uuid.
pub async fn load_agent_tool_definitions<F>(
    db: &PgPool,
    user_id: Uuid,
    project_id: Option<Uuid>,
    label_for: F,
) -> Result<Vec<Value>, sqlx::Error>
where
    F: Fn(Uuid, &str) -> String,
{
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
    .await?;

    Ok(rows
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

            let prefixed_name =
                format!("mcp__{}__{}", label_for(server_id, &server_name), tool_name);

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
        .collect())
}

/// Genera i wrapper axum "puri" (senza effetti specifici del servizio) sugli
/// endpoint core qui sopra. Punto unico anche del boilerplate HTTP (regola L):
/// senza la macro i wrapper identici di mcp-core e plugin-service tornavano a
/// essere cloni jscpd. La macro e' solo testo: NON aggiunge una dipendenza
/// axum a questo crate; e' il crate chiamante a dover avere fra le dipendenze
/// dirette `axum`, `nexus-types`, `nexus-auth`, `serde_json` e uno state axum
/// con campo `db: sqlx::PgPool`.
///
/// Uso (vedi mcp-core e plugin-service, modulo `mcp_connectors`):
/// ```ignore
/// nexus_mcp_client::mcp_server_axum_handlers!(AppState: error_adapter, create, update);
/// ```
/// Gli handler con effetti specifici del servizio (linkedTemplatesCount,
/// trigger riassegnazione template, indicizzazione Qdrant) restano scritti a
/// mano nel crate chiamante e riusano l'adapter `endpoint_error` generato da
/// `error_adapter`.
#[macro_export]
macro_rules! mcp_server_axum_handlers {
    ($state:ty : $($which:ident),+ $(,)?) => {
        $( $crate::mcp_server_axum_handlers!(@one $state, $which); )+
    };
    (@one $state:ty, error_adapter) => {
        /// Adapter axum: converte l'errore (status u16, messaggio) del core
        /// condiviso in `ApiError`.
        fn endpoint_error(
            (status, message): $crate::server_endpoints::EndpointError,
        ) -> ::nexus_types::ApiError {
            ::nexus_types::api_error(
                ::axum::http::StatusCode::from_u16(status)
                    .unwrap_or(::axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                message,
            )
        }
    };
    (@one $state:ty, list) => {
        /// GET /api/mcp-servers
        pub async fn list_mcp_servers(
            ::axum::extract::State(state): ::axum::extract::State<$state>,
            ::axum::extract::Extension(claims): ::axum::extract::Extension<::nexus_auth::Claims>,
        ) -> ::nexus_types::ApiResult {
            let user_id = ::nexus_types::parse_user_id(&claims)?;
            let servers =
                $crate::server_endpoints::list_servers_core(&state.db, user_id, &claims.role)
                    .await
                    .map_err(endpoint_error)?;
            Ok(::axum::Json(::serde_json::json!({ "servers": servers })))
        }
    };
    (@one $state:ty, create) => {
        /// POST /api/mcp-servers
        pub async fn create_mcp_server(
            ::axum::extract::State(state): ::axum::extract::State<$state>,
            ::axum::extract::Extension(claims): ::axum::extract::Extension<::nexus_auth::Claims>,
            ::axum::Json(body): ::axum::Json<$crate::server_storage::CreateMcpServerRequest>,
        ) -> ::nexus_types::ApiResult {
            let user_id = ::nexus_types::parse_user_id(&claims)?;
            let server = $crate::server_endpoints::create_server_core(&state.db, user_id, &body)
                .await
                .map_err(endpoint_error)?;
            Ok(::axum::Json(server))
        }
    };
    (@one $state:ty, update) => {
        /// PUT /api/mcp-servers/:id
        pub async fn update_mcp_server(
            ::axum::extract::State(state): ::axum::extract::State<$state>,
            ::axum::extract::Extension(claims): ::axum::extract::Extension<::nexus_auth::Claims>,
            ::axum::extract::Path(server_id): ::axum::extract::Path<String>,
            ::axum::Json(body): ::axum::Json<$crate::server_storage::UpdateMcpServerRequest>,
        ) -> ::nexus_types::ApiResult {
            let user_id = ::nexus_types::parse_user_id(&claims)?;
            let server = $crate::server_endpoints::update_server_core(
                &state.db,
                user_id,
                &claims.role,
                &server_id,
                &body,
            )
            .await
            .map_err(endpoint_error)?;
            Ok(::axum::Json(server))
        }
    };
    (@one $state:ty, delete) => {
        /// DELETE /api/mcp-servers/:id
        pub async fn delete_mcp_server(
            ::axum::extract::State(state): ::axum::extract::State<$state>,
            ::axum::extract::Extension(claims): ::axum::extract::Extension<::nexus_auth::Claims>,
            ::axum::extract::Path(server_id): ::axum::extract::Path<String>,
        ) -> ::nexus_types::ApiResult {
            let user_id = ::nexus_types::parse_user_id(&claims)?;
            let response = $crate::server_endpoints::delete_server_core(
                &state.db,
                user_id,
                &claims.role,
                &server_id,
            )
            .await
            .map_err(endpoint_error)?;
            Ok(::axum::Json(response))
        }
    };
    (@one $state:ty, toggle) => {
        /// PUT /api/mcp-servers/:id/toggle
        pub async fn toggle_mcp_server(
            ::axum::extract::State(state): ::axum::extract::State<$state>,
            ::axum::extract::Extension(claims): ::axum::extract::Extension<::nexus_auth::Claims>,
            ::axum::extract::Path(server_id): ::axum::extract::Path<String>,
            ::axum::Json(body): ::axum::Json<$crate::server_storage::ToggleRequest>,
        ) -> ::nexus_types::ApiResult {
            let user_id = ::nexus_types::parse_user_id(&claims)?;
            let response = $crate::server_endpoints::toggle_server_core(
                &state.db,
                user_id,
                &claims.role,
                &server_id,
                body.enabled,
            )
            .await
            .map_err(endpoint_error)?;
            Ok(::axum::Json(response))
        }
    };
}
