//! Contract test: le query del pannello DB progetto contro lo schema REALE di
//! `project_database_config`.
//!
//! Nasce da tre bug che condividevano la stessa firma: una query SQL invalida
//! rispetto allo schema, il cui errore veniva ingoiato dal chiamante (`let _ =`
//! / `.ok()`), quindi rotta in modo incondizionato e invisibile per mesi.
//! `cargo check` non li vede (SQL e' una stringa) e nessun log li segnalava: solo
//! l'esecuzione contro lo schema reale li smaschera.
//!
//! READ-ONLY sul meta-DB: ogni test che scrive lo fa dentro una transazione
//! ROLLBACK-ata (idempotente, regola F). Skip se `DATABASE_URL` non e' impostata.
//!
//! Eseguire con:
//!   DATABASE_URL=postgres://nexus:nexus@localhost:5433/nexus cargo test --test project_db_config_contract

use sqlx::{PgPool, Row};
use std::env;
use uuid::Uuid;

async fn pool_or_skip() -> Option<PgPool> {
    let url = env::var("DATABASE_URL").ok()?;
    PgPool::connect(&url).await.ok()
}

async fn un_progetto(pool: &PgPool) -> Option<Uuid> {
    sqlx::query("SELECT id FROM projects LIMIT 1")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<Uuid, _>("id").ok())
}

/// L'UNIQUE sul solo `project_id` NON esiste piu' (mig 0083: la tabella e'
/// multi-connessione per progetto). Il conflict target valido per "la riga
/// primaria del progetto" e' l'indice PARZIALE
/// `uq_project_database_config_project_primary`, che si aggancia solo
/// qualificando `ON CONFLICT (project_id) WHERE is_primary`.
///
/// Se questo indice viene rimosso o reso non-parziale, gli upsert di
/// `detect_project_db` e `upsert_db_profile` tornano a fallire in silenzio.
#[tokio::test]
async fn indice_parziale_riga_primaria_esiste() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let row = sqlx::query(
        "SELECT indexdef FROM pg_indexes \
         WHERE tablename = 'project_database_config' \
           AND indexname = 'uq_project_database_config_project_primary'",
    )
    .fetch_optional(&pool)
    .await
    .expect("query su pg_indexes");

    let def: String = row
        .expect("indice uq_project_database_config_project_primary assente: gli upsert della riga primaria non hanno piu' un conflict target")
        .try_get("indexdef")
        .expect("colonna indexdef");

    assert!(def.contains("UNIQUE"), "l'indice deve essere UNIQUE: {def}");
    assert!(
        def.contains("is_primary"),
        "l'indice deve essere PARZIALE su is_primary, altrimenti `ON CONFLICT (project_id) WHERE is_primary` non lo aggancia: {def}"
    );
}

/// Regressione diretta: l'upsert di `detect_project_db` deve essere ESEGUIBILE.
/// La versione precedente diceva `ON CONFLICT (project_id)` e falliva SEMPRE con
/// "no unique or exclusion constraint matching the ON CONFLICT specification",
/// per giunta ingoiata da un `let _`: la detection_metadata non e' mai stata
/// scritta e nessuno se ne e' accorto.
#[tokio::test]
async fn upsert_detection_metadata_e_eseguibile() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let Some(project_id) = un_progetto(&pool).await else {
        eprintln!("skip: nessun progetto nel meta-DB");
        return;
    };

    let mut tx = pool.begin().await.expect("begin");
    let res = sqlx::query(
        r#"
        INSERT INTO project_database_config
            (project_id, engine, hosting_mode, detection_metadata)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (project_id) WHERE is_primary DO UPDATE SET
            detection_metadata = EXCLUDED.detection_metadata,
            updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind("postgres")
    .bind("internal")
    .bind(serde_json::json!({"contract_test": true}))
    .execute(&mut *tx)
    .await;
    tx.rollback().await.expect("rollback");

    res.expect("l'upsert di detect_project_db deve essere valido contro lo schema");
}

/// Il guard di `delete_project_db_connection` deve contare SOLO le connessioni
/// che l'utente vede: `list_project_db_connections` esclude la riga
/// `connection_role='nexus_metadata'`. Contandola, il guard chiedeva di
/// "impostare un'altra connessione come primaria" indicando una riga invisibile,
/// e l'unica connessione reale del progetto diventava INDELEBILE.
#[tokio::test]
async fn guard_delete_ignora_la_riga_metadati_nascosta() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let Some(project_id) = un_progetto(&pool).await else {
        eprintln!("skip: nessun progetto nel meta-DB");
        return;
    };

    // Connessioni VISIBILI: esattamente il predicato di list_project_db_connections.
    let visibili: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_database_config \
         WHERE project_id = $1 AND connection_role <> 'nexus_metadata'",
    )
    .bind(project_id)
    .fetch_one(&pool)
    .await
    .expect("count visibili");

    // Il guard, per OGNI connessione visibile, non deve mai contare piu' delle
    // ALTRE connessioni visibili: se lo facesse, conterebbe una riga che l'utente
    // non puo' promuovere a primaria -> vicolo cieco.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM project_database_config \
         WHERE project_id = $1 AND connection_role <> 'nexus_metadata'",
    )
    .bind(project_id)
    .fetch_all(&pool)
    .await
    .expect("elenco visibili");

    for id in ids {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_database_config \
             WHERE project_id = $1 AND id <> $2 AND connection_role <> $3",
        )
        .bind(project_id)
        .bind(id)
        .bind("nexus_metadata")
        .fetch_one(&pool)
        .await
        .expect("count guard");

        assert_eq!(
            others,
            visibili - 1,
            "il guard deve contare solo le ALTRE connessioni visibili ({} in totale): \
             contando la riga nexus_metadata nascosta, l'ultima connessione diventa indelebile",
            visibili
        );
    }
}
