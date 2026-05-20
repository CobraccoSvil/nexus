//! Fix M33-B: endpoint REST per allocazione dinamica di porte di progetto.
//! Esteso nel PR hardening con quota check + audit trail centralizzato.
//!
//! POST /api/projects/:id/services/allocate-port
//!
//! Body: `{label: string}` (es. "backend", "frontend", "api")
//!
//! L'agente AI chiama questo endpoint (o `find_or_allocate` interno) quando deve
//! scegliere una porta per un servizio del progetto. Nexus:
//! 1. Verifica quota porte (`security::quotas::check_can_allocate_port`)
//! 2. Sceglie una porta libera nel bucket deterministico del progetto via
//!    `find_free_project_port`.
//! 3. INSERT in `nexus_port_allocations` con allocation_mode='dynamic'.
//! 4. Scrive `nexus_resource_audit` (allowed/blocked).
//! 5. Ritorna `{port, label, allocation_mode}` per uso dell'agente.

use super::*;
use super::services::find_free_project_port;
use crate::port_registry::PortRegistryCache;
use crate::security::{record_audit, AuditEntry};
use sqlx::PgPool;

#[derive(serde::Deserialize)]
pub struct AllocatePortBody {
    pub label: String,
}

/// Risultato di una chiamata a `find_or_allocate`.
pub struct AllocatedPort {
    pub port: u16,
    pub mode: &'static str, // "existing" | "dynamic"
}

/// Funzione internamente riusabile: trova una porta gia' allocata con la stessa
/// label OPPURE ne alloca una nuova nel bucket del progetto.
///
/// Applica quota check (`max_ports`) prima di allocare. In caso di violazione
/// quota, scrive audit `port_allocate` blocked e ritorna `Err`.
///
/// Idempotente: chiamate ripetute con la stessa `(project_id, label)` ritornano
/// la stessa porta (modalita' "existing").
pub async fn find_or_allocate(
    db: &PgPool,
    registry: &PortRegistryCache,
    project_id: Uuid,
    label: &str,
) -> Result<AllocatedPort, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("label vuota: specifica un nome (es. 'backend', 'frontend')".to_string());
    }

    // 1. Idempotenza: se esiste gia' una allocazione con questa label, riusala
    //    SOLO se qualcuno e' davvero in ascolto sulla porta. Altrimenti tentiamo
    //    di adottare un processo orfano del bucket prima di re-allocare.
    if let Ok(Some((existing_port,))) = sqlx::query_as::<_, (i32,)>(
        "SELECT port FROM nexus_port_allocations WHERE project_id = $1 AND label = $2 LIMIT 1",
    )
    .bind(project_id)
    .bind(label)
    .fetch_optional(db)
    .await
    {
        let p = existing_port as u16;
        if super::port_recovery::tcp_probe(p, 300).await {
            return Ok(AllocatedPort { port: p, mode: "existing" });
        }
        // Allocazione "stale": nessuno in ascolto. Cerca processi orfani del
        // bucket (utente li ha lanciati manualmente con .env hardcoded, oppure
        // un avvio precedente di Nexus non e' stato tracciato).
        let orphans = super::port_recovery::scan_bucket_orphans(db, project_id).await;
        if let Some((found_port, pid, program)) = orphans
            .iter()
            .find(|(_, _, prog)| super::port_recovery::looks_like_server_process(prog))
        {
            tracing::info!(
                label = %label, stale_port = p, adopted_port = *found_port,
                pid = *pid, program = %program,
                "find_or_allocate: allocazione stale, adotto processo orfano del bucket"
            );
            let _ = sqlx::query(
                "UPDATE nexus_port_allocations \
                 SET port = $1, allocation_mode = 'adopted', updated_at = NOW() \
                 WHERE project_id = $2 AND label = $3",
            )
            .bind(*found_port as i32)
            .bind(project_id)
            .bind(label)
            .execute(db)
            .await;
            record_audit(
                AuditEntry::allowed(project_id, "port_adopt", "port")
                    .with_resource(found_port.to_string())
                    .with_details(serde_json::json!({
                        "label": label, "stale_port": p, "pid": pid, "program": program
                    })),
            );
            return Ok(AllocatedPort { port: *found_port, mode: "adopted" });
        }
        // Nessun orfano adottabile: rimuovi la riga stale e prosegui ad
        // allocare ex novo nel bucket.
        let _ = sqlx::query(
            "DELETE FROM nexus_port_allocations WHERE project_id = $1 AND label = $2",
        )
        .bind(project_id)
        .bind(label)
        .execute(db)
        .await;
        tracing::info!(
            label = %label, stale_port = p,
            "find_or_allocate: allocazione stale rimossa, procedo con nuova allocazione"
        );
    }

    // 2. Quota check: non superare max_ports allocate per il progetto.
    if let Err(reason) = crate::security::quotas::check_can_allocate_port(db, project_id).await {
        record_audit(
            AuditEntry::blocked(project_id, "port_allocate", "port")
                .with_resource(label.to_string())
                .with_details(serde_json::json!({"reason": reason})),
        );
        return Err(reason);
    }

    // 3. Trova porta libera nel bucket
    let port = find_free_project_port(&project_id, registry).await;

    // 4. INSERT in DB (idempotente lato porta via UNIQUE)
    let insert_result = sqlx::query(
        r#"
        INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
        VALUES ($1, $2, $3, 'dynamic')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(port as i32)
    .bind(label)
    .execute(db)
    .await;
    if let Err(e) = insert_result {
        tracing::warn!("allocate_port: INSERT fallito (porta {} label {}): {}", port, label, e);
    }

    // 5. Audit allocato
    record_audit(
        AuditEntry::allowed(project_id, "port_allocate", "port")
            .with_resource(port.to_string())
            .with_details(serde_json::json!({"label": label, "mode": "dynamic"})),
    );

    Ok(AllocatedPort {
        port,
        mode: "dynamic",
    })
}

pub async fn allocate_port(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<AllocatePortBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let result = find_or_allocate(&state.db, &state.port_registry, project_id, &body.label)
        .await
        .map_err(|e| api_error(StatusCode::CONFLICT, &e))?;

    Ok(Json(json!({
        "port": result.port,
        "label": body.label.trim(),
        "allocation_mode": result.mode,
        "ok": true,
    })))
}

#[derive(serde::Deserialize)]
pub struct KillPortBody {
    pub port: u16,
}

/// POST /api/projects/:id/services/kill-port-process
///
/// Termina il processo in ascolto sulla porta specificata e rilascia
/// l'allocazione corrispondente da `nexus_port_allocations`. Utilizzato dal
/// pulsante "kill" del pannello Porte per pulire una porta sola alla volta.
pub async fn kill_port_process(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KillPortBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let freed = super::port_recovery::try_free_port(body.port).await;
    let deleted = sqlx::query(
        "DELETE FROM nexus_port_allocations WHERE project_id = $1 AND port = $2",
    )
    .bind(project_id)
    .bind(body.port as i32)
    .execute(&state.db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    record_audit(
        AuditEntry::allowed(project_id, "port_kill", "port")
            .with_resource(body.port.to_string())
            .with_details(serde_json::json!({"freed": freed, "deleted_allocations": deleted})),
    );

    Ok(Json(json!({
        "ok": true,
        "port": body.port,
        "freed": freed,
        "deleted_allocations": deleted,
    })))
}

/// POST /api/projects/:id/services/kill-orphan-processes
///
/// Termina i processi del bucket porte del progetto che NON sono tracciati in
/// `agent_processes` (status running/starting). Risolve la proliferazione di
/// processi avviati fuori da Nexus (es. `pnpm dev` manuale lasciato attivo) che
/// occupano porte del bucket impedendo riallocazione pulita.
pub async fn kill_orphan_processes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let killed = super::port_recovery::kill_bucket_orphans(&state.db, project_id).await;
    record_audit(
        AuditEntry::allowed(project_id, "process_kill", "process")
            .with_resource(format!("orphans:{}", killed.len()))
            .with_details(serde_json::json!({ "pids": killed })),
    );

    Ok(Json(json!({
        "ok": true,
        "killed": killed.len(),
        "pids": killed,
    })))
}
