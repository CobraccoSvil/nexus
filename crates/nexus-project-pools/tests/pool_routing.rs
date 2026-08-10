//! Contract test del punto unico `nexus-project-pools` (risoluzione read-only
//! del pool DB per-progetto dai servizi separati, separazione DB per-progetto).
//!
//! Tutti i test sono READ-ONLY sul meta-DB (idempotenti, regola F) e vengono
//! saltati se il DB non e' raggiungibile. La precondizione passa dal punto unico
//! `nexus_test_preconditions::db_o_salta` (stesso di
//! `mcp-core/tests/orchestrator_db_schema.rs`), che stampa un marker
//! `NEXUS_TEST_SKIP` e sotto `REQUIRE_INTEGRATION_TESTS=1` FALLISCE: prima i
//! quattro `eprintln!("skip: ...")` + `return` erano verdi indistinguibili da un
//! contratto verificato, e non distinguevano "variabile assente" da "DB che non
//! risponde".

use nexus_project_pools::EntityKind;
use nexus_test_preconditions::db_o_salta;
use uuid::Uuid;

/// Le tabelle del contratto di risoluzione devono esistere nel meta: sono la
/// fonte da cui il crate instrada (elenco progetti, registry connessioni,
/// directory di routing, flag nei settings). Se una viene rinominata/rimossa
/// il punto unico e' rotto per TUTTI i servizi separati.
#[tokio::test]
async fn tabelle_contratto_meta_esistono() {
    let Some(pool) = db_o_salta().await else { return };
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

/// Un progetto inesistente non deve MAI degradare in silenzio: la separazione
/// e' sempre attiva (cutover chiuso, flag rimosso mig 0527), quindi la
/// risoluzione di un progetto mai provisionato fallisce con `NotProvisioned`
/// (errore tipizzato, regola M).
#[tokio::test]
async fn progetto_sconosciuto_fallisce_tipizzato() {
    let Some(pool) = db_o_salta().await else { return };
    let ghost = Uuid::new_v4();
    match nexus_project_pools::project_data_pool(&pool, ghost).await {
        Err(nexus_project_pools::ProjectPoolError::NotProvisioned(pid)) => {
            assert_eq!(pid, ghost);
        }
        Ok(_) => panic!(
            "un progetto mai provisionato deve dare NotProvisioned (separazione sempre attiva)"
        ),
        Err(e) => panic!("errore inatteso per progetto sconosciuto: {e}"),
    }
}

/// La directory di routing risponde `None` per un'entita' mai registrata
/// (esercita la query su `nexus_data_routing` contro lo schema reale).
#[tokio::test]
async fn entita_non_mappata_ritorna_none() {
    let Some(pool) = db_o_salta().await else { return };
    let ghost = Uuid::new_v4();
    let got =
        nexus_project_pools::project_id_for_entity(&pool, EntityKind::Session, ghost).await;
    assert!(got.is_none(), "entita' fantasma non deve risultare mappata");
}

/// Una sessione inesistente in TUTTI i DB progetto termina con
/// `SessionNotFound`: separazione sempre attiva (cutover chiuso, flag rimosso
/// mig 0527), mai fallback silenzioso al meta dove le tabelle chat sono
/// decommissionate (mig 0507/0525).
#[tokio::test]
async fn sessione_sconosciuta_not_found() {
    let Some(pool) = db_o_salta().await else { return };
    let ghost = Uuid::new_v4();
    match nexus_project_pools::project_data_pool_by_session(&pool, ghost).await {
        Err(nexus_project_pools::ProjectPoolError::SessionNotFound(sid)) => {
            assert_eq!(sid, ghost);
        }
        Ok(_) => {
            panic!("una sessione inesistente deve dare SessionNotFound (separazione sempre attiva)")
        }
        Err(e) => panic!("errore inatteso per sessione sconosciuta: {e}"),
    }
}

/// Ogni kind del vocabolario `EntityKind` deve essere ACCETTATO dalla CHECK di
/// `nexus_data_routing`.
///
/// Il test attraversa il vincolo REALE (regola O): apre una transazione, prova
/// l'INSERT esattamente come la fa `register_entity_routing` in produzione, e
/// fa rollback. Confrontare l'enum col testo della migrazione sarebbe
/// ricopiare la lista che si vuole verificare, e resterebbe verde proprio nel
/// caso che conta — la migrazione non applicata al DB su cui il codice gira.
///
/// Perche' esiste: la CHECK della mig 0496 (30/06/2026) ammetteva i soli
/// 'session' e 'run', mentre il codice ha aggiunto 'message', 'correction' e
/// 'feedback'. MISURATO il 10/08/2026 sul meta-DB: 706 righe 'run', 187
/// 'session', ZERO degli altri tre in 41 giorni — ogni scrittura respinta con
/// SQLSTATE 23514 e l'errore ingoiato in un WARN. Allineamento: mig 0695.
///
/// MUTAZIONE che lo fa rosseggiare: togliere un kind dalla CHECK (o aggiungere
/// una variante a `EntityKind::TUTTI` senza la migrazione che la ammette).
/// Il messaggio nomina il kind rifiutato e il codice SQLSTATE.
#[tokio::test]
async fn la_check_ammette_ogni_kind_del_vocabolario() {
    let Some(pool) = db_o_salta().await else { return };
    let mut tx = pool.begin().await.expect("apertura transazione");
    for kind in EntityKind::TUTTI {
        let esito = sqlx::query(
            "INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)              VALUES ($1, $2, $3)",
        )
        .bind(kind.as_str())
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&mut *tx)
        .await;
        if let Err(e) = esito {
            let codice = match &e {
                sqlx::Error::Database(db) => db.code().map(|c| c.to_string()),
                _ => None,
            };
            panic!(
                "la CHECK di nexus_data_routing rifiuta il kind '{kind}'                  (SQLSTATE {codice:?}): lo schema e' indietro rispetto a                  EntityKind::TUTTI. Ogni scrittura di questo kind sara' respinta                  in silenzio e la directory non potra' mai instradarlo.                  Errore: {e}"
            );
        }
    }
    // Rollback: il test resta idempotente (regola F) e non lascia righe.
    tx.rollback().await.expect("rollback");
}
