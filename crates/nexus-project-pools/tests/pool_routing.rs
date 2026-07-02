//! Contract test del punto unico `nexus-project-pools` (risoluzione read-only
//! del pool DB per-progetto dai servizi separati, separazione DB per-progetto).
//!
//! Tutti i test sono READ-ONLY sul meta-DB (idempotenti, regola F) e vengono
//! saltati se `DATABASE_URL` non e' impostata (stesso pattern di
//! `mcp-core/tests/orchestrator_db_schema.rs`).

use sqlx::PgPool;
use std::env;
use uuid::Uuid;

async fn meta_pool_or_skip() -> Option<PgPool> {
    let url = env::var("DATABASE_URL").ok()?;
    PgPool::connect(&url).await.ok()
}

/// Le tabelle del contratto di risoluzione devono esistere nel meta: sono la
/// fonte da cui il crate instrada (elenco progetti, registry connessioni,
/// directory di routing, flag nei settings). Se una viene rinominata/rimossa
/// il punto unico e' rotto per TUTTI i servizi separati.
#[tokio::test]
async fn tabelle_contratto_meta_esistono() {
    let Some(pool) = meta_pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    for t in [
        "projects",
        "project_database_config",
        "nexus_data_routing",
        "settings",
    ] {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM information_schema.tables WHERE table_name = $1 AND table_schema = 'public'",
        )
        .bind(t)
        .fetch_optional(&pool)
        .await
        .expect("query information_schema");
        assert!(found.is_some(), "tabella '{t}' assente nel meta-DB");
    }
}

/// Un progetto inesistente non deve MAI degradare in silenzio: a flag ON la
/// risoluzione fallisce con `NotProvisioned` (errore tipizzato, regola M);
/// a flag OFF ritorna il meta (comportamento storico pre-cutover).
#[tokio::test]
async fn progetto_sconosciuto_fallisce_tipizzato_o_ritorna_meta() {
    let Some(pool) = meta_pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let ghost = Uuid::new_v4();
    let enabled = nexus_project_pools::separation_enabled(&pool).await;
    match nexus_project_pools::project_data_pool(&pool, ghost).await {
        Ok(_) => assert!(
            !enabled,
            "a flag separazione ON un progetto mai provisionato deve dare NotProvisioned"
        ),
        Err(nexus_project_pools::ProjectPoolError::NotProvisioned(pid)) => {
            assert!(enabled, "a flag OFF non deve mai fallire (ritorna il meta)");
            assert_eq!(pid, ghost);
        }
        Err(e) => panic!("errore inatteso per progetto sconosciuto: {e}"),
    }
}

/// La directory di routing risponde `None` per un'entita' mai registrata
/// (esercita la query su `nexus_data_routing` contro lo schema reale).
#[tokio::test]
async fn entita_non_mappata_ritorna_none() {
    let Some(pool) = meta_pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let ghost = Uuid::new_v4();
    let got = nexus_project_pools::project_id_for_entity(&pool, "session", ghost).await;
    assert!(got.is_none(), "entita' fantasma non deve risultare mappata");
}

/// Una sessione inesistente in TUTTI i DB progetto termina con
/// `SessionNotFound` a flag ON (mai fallback silenzioso al meta, dove le
/// tabelle chat sono decommissionate dalla mig 0507); a flag OFF ritorna il
/// meta come da comportamento storico.
#[tokio::test]
async fn sessione_sconosciuta_not_found_o_meta() {
    let Some(pool) = meta_pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let ghost = Uuid::new_v4();
    let enabled = nexus_project_pools::separation_enabled(&pool).await;
    match nexus_project_pools::project_data_pool_by_session(&pool, ghost).await {
        Ok(_) => assert!(
            !enabled,
            "a flag ON una sessione inesistente deve dare SessionNotFound"
        ),
        Err(nexus_project_pools::ProjectPoolError::SessionNotFound(sid)) => {
            assert!(enabled, "a flag OFF non deve mai fallire (ritorna il meta)");
            assert_eq!(sid, ghost);
        }
        Err(e) => panic!("errore inatteso per sessione sconosciuta: {e}"),
    }
}
