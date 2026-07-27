//! Il catalogo VERO, ricostruito dalle migrazioni, letto dal lettore VERO.
//!
//! Non c'e' JSON scritto in questo file: e' il punto della regola O. Un test
//! che si ricopia il catalogo verifica la propria copia, e per anni non si e'
//! accorto ne' dei `systemd_unit` inventati ne' del manifest mancante di
//! browser-bridge. Qui il DB nasce da `db/migrations` e il catalogo si legge
//! con `load_catalog`, la stessa funzione che usano il pannello e il generatore.

use nexus_service_catalog::{load_catalog, CatalogError};

/// Ogni voce che dichiara un `winsw_id` e' un servizio che su Windows deve
/// avere un manifest. Questo test fissa l'insieme atteso: se una voce viene
/// aggiunta al catalogo senza `winsw_id`, o se una sparisce, il generatore
/// produrrebbe un insieme diverso di manifest e questo test lo dice PRIMA.
#[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
async fn ogni_servizio_windows_del_catalogo_e_dichiarato(pool: sqlx::PgPool) {
    let catalogo = load_catalog(&pool).await.expect("catalogo leggibile");
    assert!(
        !catalogo.is_empty(),
        "catalogo vuoto dopo le migrazioni: la 0541 non ha popolato nulla"
    );

    let mut con_manifest: Vec<String> = catalogo
        .iter()
        .filter_map(|e| e.winsw_id.clone())
        .collect();
    con_manifest.sort();

    // Atteso dopo 0541 + 0601 (rimuove billing) + 0642 (aggiunge qdrant e il
    // winsw_id di garnet). browser-bridge c'e' da sempre: era il generatore a
    // non conoscerlo.
    let atteso = vec![
        "nexus-admin".to_string(),
        "nexus-browser-bridge".to_string(),
        "nexus-doc".to_string(),
        "nexus-garnet".to_string(),
        "nexus-gateway".to_string(),
        "nexus-mcp-core".to_string(),
        "nexus-plugin".to_string(),
        "nexus-qdrant".to_string(),
        "nexus-web-ide".to_string(),
    ];
    assert_eq!(
        con_manifest, atteso,
        "l'insieme dei servizi con manifest e' cambiato: aggiorna il generatore \
         e questo elenco insieme, mai uno solo dei due"
    );

    // Il fossile non deve poter rientrare: la 0601 lo ha tolto dal catalogo.
    assert!(
        !catalogo.iter().any(|e| e.name == "billing-service"),
        "billing-service e' tornato nel catalogo: genererebbe un manifest per un \
         crate che non esiste, cioe' un servizio in crash-loop"
    );
}

/// La porta di qdrant si risolve dal DB (regola G), non da un letterale nel
/// generatore. MUTAZIONE: togliere l'INSERT di `qdrant_port` dalla 0642 e
/// questo test rosseggia, invece di produrre un manifest con la porta sbagliata.
#[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
async fn le_porte_dei_servizi_windows_sono_risolvibili(pool: sqlx::PgPool) {
    let catalogo = load_catalog(&pool).await.expect("catalogo leggibile");
    let mut senza_porta = Vec::new();
    for e in catalogo.iter().filter(|e| e.winsw_id.is_some()) {
        if nexus_service_catalog::resolve_port(&pool, e).await.is_none() {
            senza_porta.push(e.name.clone());
        }
    }
    assert!(
        senza_porta.is_empty(),
        "servizi con manifest ma senza porta risolvibile: {senza_porta:?} — il \
         probe di stato li darebbe 'unknown' e gli argomenti di avvio che usano \
         la porta non sarebbero componibili"
    );
}

/// I tre esiti di `CatalogError` sono distinti. MUTAZIONE: farli collassare in
/// `Vec::new()` come faceva il codice precedente e questo test rosseggia — era
/// il difetto per cui il pannello diceva "zero servizi" quando il fatto vero
/// era "non ho potuto leggere il catalogo".
#[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
async fn la_chiave_assente_non_si_confonde_con_un_catalogo_vuoto(pool: sqlx::PgPool) {
    sqlx::query("DELETE FROM settings WHERE key = 'system.services_catalog'")
        .execute(&pool)
        .await
        .expect("cancellazione della chiave");
    match load_catalog(&pool).await {
        Err(CatalogError::ChiaveAssente { key }) => {
            assert_eq!(key, "system.services_catalog");
        }
        Ok(v) => panic!("chiave assente riportata come catalogo di {} voci", v.len()),
        Err(e) => panic!("errore inatteso: {e}"),
    }
}

/// Un catalogo corrotto non e' un catalogo vuoto.
#[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
async fn il_json_illeggibile_e_un_errore_dedicato(pool: sqlx::PgPool) {
    sqlx::query("UPDATE settings SET value = '{non json' WHERE key = 'system.services_catalog'")
        .execute(&pool)
        .await
        .expect("corruzione del valore");
    match load_catalog(&pool).await {
        Err(CatalogError::NonParsabile { key, .. }) => {
            assert_eq!(key, "system.services_catalog");
        }
        altro => panic!("atteso NonParsabile, ottenuto: {altro:?}"),
    }
}
