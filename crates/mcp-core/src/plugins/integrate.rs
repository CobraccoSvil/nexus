use std::collections::HashMap;

use axum::{extract::State, http::StatusCode, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    mcp_client::{self, McpServerConfig, McpTransport},
    AppState,
};

/// Chiavi secret dichiarate da un plugin, deduplicate e ripulite dai vuoti.
fn collect_secret_refs(groups: &[&Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for group in groups {
        let Some(list) = group.as_array() else { continue };
        for key in list.iter().filter_map(Value::as_str) {
            let key = key.trim();
            if !key.is_empty() && !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

/// Crea le righe `settings` per i secret che un plugin dichiara di leggere.
///
/// Dichiarare `requiredSecretRefs`/`optionalSecretRefs` significa che il plugin
/// leggera' quelle chiavi: la riga nasce qui, dove categoria e `is_secret` sono
/// noti e veri. Prima il catalogo le registrava senza crearle, e l'unico modo di
/// valorizzarle dalla UI era il ramo INSERT di `settings::update_setting`, che le
/// materializzava in categoria 'custom' con `is_secret = FALSE`: `mask_settings`
/// maschera solo cio' che e' marcato secret, quindi il token finiva IN CHIARO
/// nella risposta di `GET /api/admin/settings` (che la UI legge tutta, senza
/// filtro di categoria). Quel ramo non esiste piu' (il PUT risponde 404 su
/// chiave assente), quindi il seeding qui e' cio' che tiene configurabili i
/// plugin integrati a runtime — e li fa nascere mascherati.
///
/// `ON CONFLICT DO NOTHING`: le chiavi gia' seedate da una migrazione (es.
/// `figma_oauth_token`, mig 0017) tengono descrizione e valore che hanno.
async fn seed_declared_secrets(
    db: &sqlx::PgPool,
    plugin_name: &str,
    keys: &[String],
) -> Result<(), ApiError> {
    for key in keys {
        sqlx::query(
            "INSERT INTO settings (key, value, category, description, is_secret, updated_at) \
             VALUES ($1, '', 'connectors', $2, TRUE, NOW()) ON CONFLICT (key) DO NOTHING",
        )
        .bind(key)
        .bind(format!("Secret del plugin MCP {plugin_name}"))
        .execute(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrateDraftRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub transport: String, // "http" | "stdio"

    // http
    pub http_url: Option<String>,
    pub headers: Option<HashMap<String, String>>,

    // stdio
    pub stdio_command: Option<String>,
    pub stdio_args: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,

    pub default_scope: Option<String>, // "global" | "project" | "user"
    pub required_secret_refs: Option<Vec<String>>,
    pub optional_secret_refs: Option<Vec<String>>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratePublishRequest {
    pub item: Value,
    pub version: Option<String>,
    pub changelog: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrateDraftResponse {
    pub item: Value,
    pub discovered_tools: Vec<Value>,
    pub tool_count: usize,
}

fn normalize_slug(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Come `plugins::normalize_scope`, ma l'errore nomina `defaultScope` perche'
/// e' il campo che questo endpoint riceve. Era l'UNICA differenza fra le tre
/// copie di questa funzione, ed e' la ragione per cui il punto unico
/// (`nexus_mcp_client::plugin_storage::normalizza_scope`) e' puro e non compone
/// messaggi.
fn normalize_scope(raw: Option<&str>) -> Result<String, (StatusCode, Json<Value>)> {
    nexus_mcp_client::plugin_storage::normalizza_scope(raw).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "defaultScope non valido: usa global, project o user",
        )
    })
}

fn build_mcp_config(
    req: &IntegrateDraftRequest,
) -> Result<McpServerConfig, (StatusCode, Json<Value>)> {
    let transport = req.transport.trim().to_lowercase();
    let transport = match transport.as_str() {
        "http" => {
            let url = req.http_url.as_deref().unwrap_or("").trim();
            if url.is_empty() {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "httpUrl richiesto per transport http",
                ));
            }
            McpTransport::Http {
                url: url.to_string(),
                headers: req.headers.clone().unwrap_or_default(),
            }
        }
        "stdio" => {
            let cmd = req.stdio_command.as_deref().unwrap_or("").trim();
            if cmd.is_empty() {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "stdioCommand richiesto per transport stdio",
                ));
            }
            McpTransport::Stdio {
                command: cmd.to_string(),
                args: req.stdio_args.clone().unwrap_or_default(),
                env_vars: req.env_vars.clone().unwrap_or_default(),
            }
        }
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "transport deve essere 'http' o 'stdio'",
            ))
        }
    };

    Ok(McpServerConfig {
        id: format!("integrate:{}", normalize_slug(&req.slug)),
        name: req.name.clone(),
        transport,
        enabled: true,
    })
}

/// POST /api/admin/plugins/integrate/draft
pub async fn draft_plugin_integration(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(body): Json<IntegrateDraftRequest>,
) -> ApiResult {
    let slug = normalize_slug(&body.slug);
    if slug.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "slug richiesto"));
    }
    if body.name.trim().is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "name richiesto"));
    }

    let scope = normalize_scope(body.default_scope.as_deref())?;
    let cfg = build_mcp_config(&body)?;

    let stdio_timeout = mcp_client::resolve_stdio_timeout(&state.db).await;
    let tools = mcp_client::list_tools(&cfg, stdio_timeout)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::BAD_GATEWAY,
                format!("Tool discovery fallito: {e}"),
            )
        })?;

    let discovered_tools = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect::<Vec<_>>();

    let tool_names = tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();

    // Safe default: allowlist all discovered tools (reviewable in UI)
    let default_tool_policy = json!({
        "mode": "allowlist",
        "tools": tool_names,
        "blockedTools": [],
    });

    let description = body.description.unwrap_or_default();
    let metadata = body.metadata.unwrap_or_else(|| json!({}));

    let item = json!({
        "slug": slug,
        "name": body.name.trim(),
        "description": description,
        "pluginType": "mcp",
        "transport": body.transport.trim().to_lowercase(),
        "httpUrl": body.http_url,
        "stdioCommand": body.stdio_command,
        "stdioArgs": body.stdio_args.unwrap_or_default(),
        "requiredSecretRefs": body.required_secret_refs.unwrap_or_default(),
        "optionalSecretRefs": body.optional_secret_refs.unwrap_or_default(),
        "defaultScope": scope,
        "allowedCommands": body.stdio_command.clone().map(|c| vec![c]).unwrap_or_default(),
        "defaultToolPolicy": default_tool_policy,
        "metadata": metadata,
        "isAllowlisted": true,
        "enabled": true,
    });

    Ok(Json(json!(IntegrateDraftResponse {
        item,
        tool_count: discovered_tools.len(),
        discovered_tools,
    })))
}

/// POST /api/admin/plugins/integrate/publish
pub async fn publish_plugin_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<IntegratePublishRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let item = body.item;
    let slug = normalize_slug(item.get("slug").and_then(Value::as_str).unwrap_or(""));
    if slug.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "item.slug richiesto"));
    }
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "item.name richiesto"));
    }

    let description = item
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let transport = item
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("http")
        .trim()
        .to_lowercase();
    if transport != "http" && transport != "stdio" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "item.transport deve essere 'http' o 'stdio'",
        ));
    }

    let http_url = item
        .get("httpUrl")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stdio_command = item
        .get("stdioCommand")
        .and_then(Value::as_str)
        .map(str::to_string);

    let stdio_args = item.get("stdioArgs").cloned().unwrap_or_else(|| json!([]));
    let required_secret_refs = item
        .get("requiredSecretRefs")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let optional_secret_refs = item
        .get("optionalSecretRefs")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let declared_secrets = collect_secret_refs(&[&required_secret_refs, &optional_secret_refs]);
    let default_scope = normalize_scope(item.get("defaultScope").and_then(Value::as_str))?;
    let allowed_commands = item
        .get("allowedCommands")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let default_tool_policy = item
        .get("defaultToolPolicy")
        .cloned()
        .unwrap_or_else(|| json!({"mode":"allowlist","tools":[],"blockedTools":[]}));
    let metadata = item.get("metadata").cloned().unwrap_or_else(|| json!({}));

    let existing = sqlx::query("SELECT id FROM plugin_catalog_items WHERE slug = $1")
        .bind(&slug)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if existing.is_some() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Slug gia' presente nel catalogo",
        ));
    }

    let row = sqlx::query(
        r#"
        INSERT INTO plugin_catalog_items
            (slug, name, description, plugin_type, transport, http_url, stdio_command, stdio_args,
             required_secret_refs, optional_secret_refs, default_scope, allowed_commands, default_tool_policy,
             metadata, is_allowlisted, enabled, updated_at)
        VALUES
            ($1, $2, $3, 'mcp', $4, $5, $6, $7,
             $8, $9, $10, $11, $12,
             $13, TRUE, TRUE, NOW())
        RETURNING id
        "#,
    )
    .bind(&slug)
    .bind(&name)
    .bind(&description)
    .bind(&transport)
    .bind(http_url)
    .bind(stdio_command.clone())
    .bind(stdio_args)
    .bind(required_secret_refs)
    .bind(optional_secret_refs)
    .bind(&default_scope)
    .bind(allowed_commands)
    .bind(default_tool_policy)
    .bind(metadata)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let catalog_item_id: Uuid = row.try_get("id").unwrap_or(Uuid::nil());

    seed_declared_secrets(&state.db, &name, &declared_secrets).await?;

    let version = body.version.unwrap_or_else(|| "1.0.0".to_string());
    let changelog = body
        .changelog
        .unwrap_or_else(|| "Integrated via admin wizard".to_string());
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_releases (catalog_item_id, version, changelog, config_patch, is_stable)
        VALUES ($1, $2, $3, '{}'::jsonb, TRUE)
        ON CONFLICT (catalog_item_id, version) DO NOTHING
        "#,
    )
    .bind(catalog_item_id)
    .bind(&version)
    .bind(&changelog)
    .execute(&state.db)
    .await;

    // Audit: publish event (catalog-level, no instance yet)
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_audit_events (plugin_instance_id, user_id, project_id, action, status, message, payload)
        VALUES (NULL, $1, NULL, 'catalog_publish', 'ok', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(format!("Catalog item pubblicato: {slug}"))
    .bind(json!({
        "slug": slug,
        "catalogItemId": catalog_item_id.to_string(),
        "transport": transport,
        "stdioCommand": stdio_command,
        "version": version,
    }))
    .execute(&state.db)
    .await;

    Ok(Json(json!({
        "ok": true,
        "catalogItemId": catalog_item_id.to_string(),
        "slug": slug,
        "version": version,
    })))
}
