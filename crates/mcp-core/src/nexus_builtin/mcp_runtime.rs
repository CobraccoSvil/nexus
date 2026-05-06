//! MCP runtime discovery + call tools.
//!
//! Obiettivo: ridurre token verso il provider evitando di inviare tutte le tool definitions MCP
//! in ogni richiesta. L'agente può:
//! 1) cercare tool con `nexus_mcp_tool_search`
//! 2) invocare tool specifico con `nexus_mcp_tool_call` (server_id + tool_name + arguments)
//!
//! Sicurezza:
//! - la search e la call sono limitate ai server MCP accessibili (scope global oppure owner user_id oppure project_id)
//! - la call applica anche la policy del plugin (via mcp_connectors::execute_mcp_tool)

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::mcp_connectors;

fn parse_i64(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(default)
}

fn parse_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

async fn can_access_server(
    db: &PgPool,
    server_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
) -> bool {
    // Scope "global" sempre accessibile; scope "user" solo owner; scope "project" solo se project_id match.
    let row = sqlx::query(
        "SELECT scope, user_id, project_id, enabled FROM mcp_servers WHERE id=$1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else { return false; };
    let enabled: bool = row.try_get("enabled").unwrap_or(false);
    if !enabled { return false; }

    let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".to_string());
    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let pid: Option<Uuid> = row.try_get("project_id").unwrap_or(None);

    match scope.as_str() {
        "global" => true,
        "project" => pid == Some(project_id),
        _ => owner == Some(user_id),
    }
}

pub async fn handle_mcp_tool_search(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    let query = parse_str(arguments.get("query")).unwrap_or_default();
    if query.is_empty() {
        return format_json(&json!({"error": "query richiesto"}));
    }
    let limit = parse_i64(arguments.get("limit"), 10).clamp(1, 50) as i64;

    // Nota: query semplice ILIKE su tool_name/description e server_name.
    // In futuro: ranking BM25/embeddings, ma già questo è sufficiente per discovery minimale.
    let like = format!("%{}%", query.replace('%', "").replace('_', ""));
    let rows = sqlx::query(
        r#"
        SELECT
          s.id          AS server_id,
          s.name        AS server_name,
          s.scope       AS scope,
          t.tool_name   AS tool_name,
          t.description AS description,
          t.input_schema AS input_schema
        FROM mcp_servers s
        JOIN mcp_server_tools t ON t.server_id = s.id
        WHERE s.enabled = true
          AND (
            s.scope = 'global'
            OR (s.scope = 'user' AND s.user_id = $1)
            OR (s.scope = 'project' AND s.project_id = $2)
          )
          AND (
            t.tool_name ILIKE $3
            OR COALESCE(t.description,'') ILIKE $3
            OR s.name ILIKE $3
          )
        ORDER BY s.scope DESC, s.name, t.tool_name
        LIMIT $4
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(like)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let results: Vec<Value> = rows
        .iter()
        .map(|r| {
            let server_id: Uuid = r.try_get("server_id").unwrap_or(Uuid::nil());
            let server_name: String = r.try_get("server_name").unwrap_or_default();
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r.try_get::<Value, _>("input_schema").unwrap_or(json!({}));
            json!({
              "server_id": server_id.to_string(),
              "server_name": server_name,
              "tool_name": tool_name,
              "description": description,
              "input_schema": input_schema
            })
        })
        .collect();

    format_json(&json!({
      "query": query,
      "count": results.len(),
      "results": results
    }))
}

pub async fn handle_mcp_tool_call(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    let server_id_str = parse_str(arguments.get("server_id"));
    let tool_name = parse_str(arguments.get("tool_name")).unwrap_or_default();
    let args = arguments.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let Some(server_id_str) = server_id_str else {
        return format_json(&json!({"error": "server_id richiesto"}));
    };
    let Ok(server_id) = Uuid::parse_str(&server_id_str) else {
        return format_json(&json!({"error": "server_id non valido"}));
    };
    if tool_name.is_empty() {
        return format_json(&json!({"error": "tool_name richiesto"}));
    }

    if !can_access_server(db, server_id, user_id, project_id).await {
        return format_json(&json!({"error": "server non accessibile o disabilitato"}));
    }

    // Esegue rispettando le policy plugin (se presente)
    mcp_connectors::execute_mcp_tool(db, server_id, &tool_name, args).await
}

// Reuse formatter from prompt_admin for consistent output
fn format_json(v: &Value) -> String {
    // Best-effort; se fallisce, fallback compatto
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

