//! Il punto unico della scrittura settings aggiorna, non crea.
//!
//! Copre lo stesso contratto del contract test HTTP di mcp-core
//! (`settings_update_contract.rs`) ma un livello piu' in basso: qui non servono
//! ne' il server ne' un JWT, solo il DB. Il caso `UnknownKey` e' la regressione
//! che chiude il ramo `INSERT ... 'custom'` dell'handler.
//!
//! Idempotente e indipendente dall'ordine (regola F): ogni test crea le proprie
//! righe con un suffisso unico e le rimuove in chiusura.
//!
//! Serve un DB: la precondizione passa dal punto unico
//! `nexus_test_preconditions::db_o_salta`, che stampa un marker
//! `NEXUS_TEST_SKIP` e sotto `REQUIRE_INTEGRATION_TESTS=1` FALLISCE. Prima era
//! `eprintln!("skip: DATABASE_URL non impostata")` + `return`: un verde
//! indistinguibile da un contratto verificato. In CI `DATABASE_URL` C'E', quindi
//! questi due test girano davvero; erano muti solo in locale e in ambienti
//! parziali.

use nexus_auth::{update_setting_value, SettingWriteError};
use nexus_test_preconditions::db_o_salta;
use sqlx::PgPool;
use uuid::Uuid;

async fn cleanup(pool: &PgPool, key: &str) {
    let _ = sqlx::query("DELETE FROM settings WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn chiave_assente_non_viene_creata() {
    let Some(pool) = db_o_salta().await else { return };

    let key = format!("test.write_unit.assente.{}", Uuid::new_v4());
    cleanup(&pool, &key).await;

    let esito = update_setting_value(&pool, &key, "valore-fantasma").await;

    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&pool)
        .await
        .expect("rilettura del valore");
    cleanup(&pool, &key).await;

    assert_eq!(stored, None, "la riga non deve essere stata creata");
    assert!(
        matches!(esito, Err(SettingWriteError::UnknownKey(ref k)) if *k == key),
        "una chiave assente e' UnknownKey, non un successo: {esito:?}"
    );
}

#[tokio::test]
async fn chiave_esistente_viene_aggiornata() {
    let Some(pool) = db_o_salta().await else { return };

    let key = format!("test.write_unit.esistente.{}", Uuid::new_v4());
    cleanup(&pool, &key).await;
    sqlx::query(
        "INSERT INTO settings (key, value, category, description, is_secret) \
         VALUES ($1, 'valore-iniziale', 'test', 'test punto unico scrittura', FALSE)",
    )
    .bind(&key)
    .execute(&pool)
    .await
    .expect("seed della riga di test");

    let esito = update_setting_value(&pool, &key, "valore-nuovo").await;

    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&pool)
        .await
        .expect("rilettura del valore");
    cleanup(&pool, &key).await;

    assert!(esito.is_ok(), "l'aggiornamento doveva riuscire: {esito:?}");
    assert_eq!(stored.as_deref(), Some("valore-nuovo"));
}

/// La mappatura verso HTTP vive nel punto unico: chi la duplicasse in un
/// handler potrebbe divergere (era il caso dei due gemelli mcp-core/admin-service).
#[test]
fn lo_status_http_dell_errore_e_deciso_una_volta_sola() {
    use axum::http::StatusCode;

    assert_eq!(
        SettingWriteError::UnknownKey("k".into()).status_code(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        SettingWriteError::Db {
            key: "k".into(),
            source: sqlx::Error::RowNotFound,
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}
