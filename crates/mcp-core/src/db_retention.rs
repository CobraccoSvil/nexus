//! Worker di retention del DB (regola H: chiude alla causa la crescita illimitata
//! di due domini a-crescita-monotona che nessun codice ripuliva).
//!
//! 1. `nexus_graph_checkpoints` — il checkpointer LangGraph scrive uno stato
//!    serializzato per superstep (~centinaia di KB l'uno): senza pruning cresce
//!    di ~10MB per run. Potiamo i checkpoint dei run TERMINALI (non piu'
//!    resumibili) oltre un grace period, tenendo i run attivi/in-attesa.
//!    Per-progetto (separazione DB): itera i progetti e pota sul pool di ciascuno.
//! 2. `ai_model_health_history` / `nexus_provider_health_history` — telemetria
//!    append-only dei probe provider (decine di migliaia di righe/giorno): TTL
//!    sui record oltre N giorni. Dominio globale -> resta sul meta-DB.
//!
//! Finestre DB-driven (regola G), safe-default se il setting manca (come
//! `run_reaper`). Flag `db.retention.enabled` per disattivarlo.

use std::time::Duration;

use sqlx::PgPool;

/// Legge un setting intero con default (regola G): safe-default se la chiave manca
/// o non e' parsabile, con guard minima.
async fn setting_i64(db: &PgPool, key: &str, default: i64, min: i64) -> i64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .max(min)
}

/// `true` se il worker e' abilitato (default true se il setting manca).
async fn retention_enabled(db: &PgPool) -> bool {
    crate::settings::get_setting(db, "db.retention.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true)
}

/// Avvia il loop di retention in background. Delay iniziale per non pesare sul boot.
pub fn start_retention_worker(db: PgPool) {
    tokio::spawn(async move {
        // Delay iniziale (5 min): il boot ha gia' recovery/reaper da fare.
        tokio::time::sleep(Duration::from_secs(300)).await;
        loop {
            let interval = setting_i64(&db, "db.retention.interval_secs", 21_600, 3_600).await; // 6h
            if retention_enabled(&db).await {
                if let Err(e) = run_cycle(&db).await {
                    tracing::warn!(error = %e, "db_retention: ciclo fallito");
                }
            }
            tokio::time::sleep(Duration::from_secs(interval as u64)).await;
        }
    });
}

/// Un ciclo di retention: checkpoint per-progetto + telemetria sul meta.
async fn run_cycle(db: &PgPool) -> Result<(), sqlx::Error> {
    prune_checkpoints(db).await;
    prune_health_history(db).await;
    Ok(())
}

/// Pota i checkpoint dei run TERMINALI oltre il grace period, iterando i progetti
/// (separazione DB). Tiene i run resumibili (punto unico `ACTIVE_RUN_STATUSES`:
/// running / awaiting_confirmation / awaiting_subagents) e quelli recenti: potare
/// il checkpoint di un run sospeso-vivo lo renderebbe non piu' resumibile
/// (`resume_native_fanin`/HITL ripartono PROPRIO da quel checkpoint).
/// `blocked_needs_input` e' TERMINALE (ADR 0034: run concluso
/// con dichiarazione "serve input", nessun resume) -> potabile. A flag OFF tutti
/// i pool sono il meta: la prima iterazione pota, le successive sono no-op
/// (idempotente).
async fn prune_checkpoints(db: &PgPool) {
    let grace_hours = setting_i64(db, "db.retention.checkpoint_grace_hours", 168, 1).await; // 7 giorni
    let mut total: u64 = 0;
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
        let deleted = sqlx::query(
            &format!(
                r#"
            DELETE FROM nexus_graph_checkpoints
            WHERE run_id IN (
                SELECT id FROM agent_runs
                WHERE status NOT IN ({active})
                  AND COALESCE(completed_at, updated_at, created_at) < NOW() - make_interval(hours => $1)
            )
            "#,
                active = crate::agent_types::ACTIVE_RUN_STATUS_SQL
            ),
        )
        .bind(grace_hours as f64)
        .execute(&pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
        total += deleted;
    }
    if total > 0 {
        tracing::info!(
            "db_retention: potati {} checkpoint di run terminali (grace {}h)",
            total,
            grace_hours
        );
    }
}

/// TTL sulla telemetria dei probe provider (dominio globale, meta-DB).
async fn prune_health_history(db: &PgPool) {
    let days = setting_i64(db, "db.retention.health_history_days", 30, 1).await;
    for table in ["ai_model_health_history", "nexus_provider_health_history"] {
        // `table` e' una costante interna (mai input utente): nessuna injection.
        let sql =
            format!("DELETE FROM {table} WHERE checked_at < NOW() - make_interval(days => $1)");
        let deleted = sqlx::query(&sql)
            .bind(days as f64)
            .execute(db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
        if deleted > 0 {
            tracing::info!(
                "db_retention: TTL {} -> rimosse {} righe piu' vecchie di {} giorni",
                table,
                deleted,
                days
            );
        }
    }
}
