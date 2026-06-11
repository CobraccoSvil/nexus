//! Fix M51: tool MCP `request_port` per agenti.
//! Esteso nel PR hardening per usare `find_or_allocate_port` (quota check + audit).
//!
//! L'agente AI chiama questo tool quando deve scegliere una porta per un
//! servizio del progetto (al posto di hardcodare 3002/5173 o di costruire
//! curl shell verso l'endpoint REST allocate-port).
//!
//! Comportamento:
//! 1. Quota: `security::quotas::check_can_allocate_port` (max_ports per progetto)
//! 2. Idempotenza: se esiste gia un'allocazione con la stessa label, ritorna quella porta
//! 3. Altrimenti alloca dal bucket deterministico tramite `find_free_project_port`
//! 4. INSERT in `nexus_port_allocations` con allocation_mode='dynamic'
//! 5. Audit allocato in `nexus_resource_audit`
//! 6. Emit `ProjectEvent::PortAllocated` per i pannelli UI
//!
//! Ritorna JSON: {"port": <num>, "label": "<lbl>", "allocation_mode":
//! "existing" | "dynamic"}

use super::AgentToolContext;
use serde_json::{json, Value};
use sqlx::Row;

pub async fn tool_request_port(ctx: &AgentToolContext, input: &Value) -> String {
    let label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let label = match label {
        Some(l) => l.to_string(),
        None => {
            return "[Errore: parametro 'label' obbligatorio (es. 'backend-dev', 'frontend-dev')]"
                .to_string();
        }
    };

    // find_or_allocate_port applica quota check, idempotency, audit. Vedi
    // crates/mcp-core/src/project_workspace/allocate_port.rs.
    match crate::project_workspace::find_or_allocate_port(
        &ctx.db,
        &ctx.port_registry,
        ctx.project_id,
        &label,
    )
    .await
    {
        Ok(alloc) => {
            // Emit evento dispatcher per i pannelli UI (riusa transport esistente)
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::PortAllocated {
                    port: alloc.port as i32,
                    label: label.clone(),
                    pid: None,
                },
            );

            json!({
                "port": alloc.port,
                "label": label,
                "allocation_mode": alloc.mode,
            })
            .to_string()
        }
        Err(e) => format!("[Errore allocazione porta: {}]", e),
    }
}

/// Tool READ-ONLY per i task di verifica/audit della gestione porte. Non alloca
/// nulla: legge il bucket deterministico del progetto e le allocazioni
/// registrate in `nexus_port_allocations`. Risolve l'incidente in cui un task
/// "verifica le porte del progetto" non aveva alcun tool per ispezionare lo
/// stato governato e finiva per dedurre porte hardcoded leggendo i sorgenti.
pub async fn tool_nexus_list_ports(ctx: &AgentToolContext, _input: &Value) -> String {
    use crate::project_workspace::services::{
        project_bucket_start, PROJECT_PORT_BUCKET_SIZE, PROJECT_PORT_RANGE_END,
        PROJECT_PORT_RANGE_START,
    };

    let bucket_start = project_bucket_start(&ctx.project_id);
    let bucket_end = bucket_start + PROJECT_PORT_BUCKET_SIZE - 1;

    let rows = sqlx::query(
        "SELECT port, label, allocation_mode, service_unit, created_at \
         FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port",
    )
    .bind(ctx.project_id)
    .fetch_all(ctx.db.as_ref())
    .await;

    let allocations: Vec<Value> = match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| {
                json!({
                    "port": r.try_get::<i32, _>("port").unwrap_or(0),
                    "label": r.try_get::<String, _>("label").unwrap_or_default(),
                    "allocation_mode": r.try_get::<String, _>("allocation_mode").unwrap_or_default(),
                    "service_unit": r.try_get::<Option<String>, _>("service_unit").ok().flatten(),
                    "created_at": r
                        .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_default(),
                })
            })
            .collect(),
        Err(e) => {
            return format!("[Errore lettura allocazioni porte: {}]", e);
        }
    };

    json!({
        "bucket": { "start": bucket_start, "end": bucket_end },
        "nexus_range": { "min": PROJECT_PORT_RANGE_START, "max": PROJECT_PORT_RANGE_END },
        "allocations": allocations,
        "count": allocations.len(),
        "hint": "Sola lettura. Per ottenere una NUOVA porta usa request_port(label=...). \
                 Le porte hardcoded fuori bucket, o nel bucket ma non allocate (inclusi i \
                 fallback tipo `process.env.PORT || 5000`), vengono rifiutate in scrittura e \
                 i processi su porte non allocate vengono terminati dal port enforcer."
    })
    .to_string()
}
