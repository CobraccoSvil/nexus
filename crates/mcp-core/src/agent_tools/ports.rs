//! Fix M51: tool MCP `request_port` per agenti.
//!
//! L'agente AI chiama questo tool quando deve scegliere una porta per un
//! servizio del progetto (al posto di hardcodare 3002/5173 o di costruire
//! curl shell verso l'endpoint REST allocate-port).
//!
//! Logica:
//! 1. Idempotenza: se esiste gia un'allocazione con la stessa label per
//!    il progetto, ritorna quella porta.
//! 2. Altrimenti calcola il bucket deterministico per project_id
//!    (PROJECT_PORT_RANGE_START + hash(project_id) * BUCKET_SIZE) e
//!    sceglie la prima porta libera nel bucket.
//! 3. INSERT in nexus_port_allocations con allocation_mode='dynamic'.
//!
//! Ritorna JSON: {"port": <num>, "label": "<lbl>", "allocation_mode":
//! "existing" | "dynamic"}

use super::AgentToolContext;
use serde_json::{json, Value};

use crate::project_workspace::services::{
    project_bucket_start, PROJECT_PORT_BUCKET_SIZE, PROJECT_PORT_RANGE_START,
};

pub async fn tool_request_port(ctx: &AgentToolContext, input: &Value) -> String {
    let label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let label = match label {
        Some(l) => l.to_string(),
        None => {
            return "[Errore: parametro 'label' obbligatorio (es. 'backend-dev', 'frontend-dev')]".to_string();
        }
    };

    // 1. Idempotenza: cerca allocazione esistente per (project_id, label)
    if let Ok(Some((existing,))) = sqlx::query_as::<_, (i32,)>(
        "SELECT port FROM nexus_port_allocations \
         WHERE project_id = $1 AND label = $2 LIMIT 1",
    )
    .bind(ctx.project_id)
    .bind(&label)
    .fetch_optional(&*ctx.db)
    .await
    {
        return json!({
            "port": existing,
            "label": label,
            "allocation_mode": "existing",
        })
        .to_string();
    }

    // 2. Allocazione nuova: bucket deterministico
    let bucket_start = project_bucket_start(&ctx.project_id);
    let bucket_end = bucket_start.saturating_add(PROJECT_PORT_BUCKET_SIZE).saturating_sub(1);

    // Carica porte gia allocate nel progetto + porte gia in uso da altri progetti
    // nel bucket (rare, ma proteggi da collisioni cross-progetto).
    let used: Vec<i32> = sqlx::query_scalar(
        "SELECT port FROM nexus_port_allocations \
         WHERE port >= $1 AND port <= $2",
    )
    .bind(bucket_start as i32)
    .bind(bucket_end as i32)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    let used_set: std::collections::HashSet<i32> = used.into_iter().collect();

    let mut chosen: Option<i32> = None;
    for p in bucket_start..=bucket_end {
        let p_i = p as i32;
        if used_set.contains(&p_i) {
            continue;
        }
        // Verifica anche che la porta non sia in uso da un processo locale.
        if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            drop(listener);
            chosen = Some(p_i);
            break;
        }
    }

    // Fallback globale se bucket pieno
    if chosen.is_none() {
        for p in PROJECT_PORT_RANGE_START..=39_999u16 {
            let p_i = p as i32;
            if used_set.contains(&p_i) {
                continue;
            }
            if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
                drop(listener);
                chosen = Some(p_i);
                break;
            }
        }
    }

    let port = match chosen {
        Some(p) => p,
        None => {
            return "[Errore: nessuna porta libera disponibile nel range progetti (20000-39999)]"
                .to_string();
        }
    };

    let insert_res = sqlx::query(
        r#"
        INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
        VALUES ($1, $2, $3, 'dynamic')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(ctx.project_id)
    .bind(port)
    .bind(&label)
    .execute(&*ctx.db)
    .await;

    if let Err(e) = insert_res {
        return format!("[Errore: INSERT fallito ({})]", e);
    }

    nexus_events::dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        nexus_events::event::ProjectEvent::PortAllocated {
            port,
            label: label.clone(),
            pid: None,
        },
    );

    json!({
        "port": port,
        "label": label,
        "allocation_mode": "dynamic",
    })
    .to_string()
}
