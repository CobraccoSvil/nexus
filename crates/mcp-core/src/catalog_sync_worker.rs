//! Worker `catalog_sync` — chiama periodicamente `models::run_catalog_sync`
//! per allineare `ai_price_catalog` al dataset upstream di LiteLLM su GitHub.
//!
//! Motivazione: l'endpoint `POST /api/admin/sync-model-catalog` esiste ma
//! richiede trigger manuale da admin. In assenza di sync periodico, quando
//! un provider rilascia un nuovo modello o ne dismette uno, il catalog
//! resta "fermo nel tempo" e:
//!   - i modelli nuovi non sono mai disponibili in routing dinamico
//!   - i modelli dismessi restano `is_enabled=true` (il sync e' solo upsert,
//!     ma `model_health_probe` li disabilita comunque a runtime; questo
//!     worker pero' garantisce che il *pricing* e le *capabilities* dei
//!     modelli sopravvissuti siano sempre aggiornati).
//!
//! Cadenza default: 12 ore (settings.model_catalog_sync_interval_s = 43200).

use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;

use crate::models;

const MIN_INTERVAL_S: u64 = 3600; // 1h: piu' freneticamente non ha senso (upstream e' GitHub).

/// Avvia il worker. Restituisce subito.
pub fn spawn_catalog_sync_worker(db: PgPool, enabled: bool, interval_s: u64) {
    let enabled = match std::env::var("NEXUS_MODEL_CATALOG_SYNC_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("catalog_sync_worker: DISABILITATO (model_catalog_sync_enabled=false)");
        return;
    }
    let interval_s = std::env::var("NEXUS_MODEL_CATALOG_SYNC_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!("catalog_sync_worker: avvio worker (interval={interval_s}s)");
    tokio::spawn(async move {
        // Aspetta 5 minuti al primo avvio: prima vogliamo che gli altri
        // worker abbiano completato il loro warm-up, e non vogliamo
        // bloccare lo startup mcp-core con un fetch sincrono da GitHub.
        sleep(Duration::from_secs(300)).await;
        loop {
            run_one_round(&db).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

async fn run_one_round(db: &PgPool) {
    match models::run_catalog_sync(db).await {
        Ok((added, updated, skipped)) => {
            tracing::info!(
                "catalog_sync_worker: sync OK — added={added} updated={updated} skipped={skipped}"
            );
        }
        Err(e) => {
            tracing::warn!("catalog_sync_worker: sync fallito: {e}");
        }
    }
}
