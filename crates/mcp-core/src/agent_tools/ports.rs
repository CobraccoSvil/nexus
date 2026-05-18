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
