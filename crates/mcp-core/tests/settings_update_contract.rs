//! Contract test: l'esito di `PUT /api/admin/setting/:key` e' lo STATUS HTTP.
//!
//! Regressione catturata: l'handler rispondeva 200 in ogni caso, mettendo
//! l'esito nel solo campo `status` del body (`{"status":"error"}` su rifiuto
//! del DB). Il client (`fetchJson`) decide sullo status HTTP e quindi non
//! sollevava: le pagine admin mostravano "salvato" su scritture mai avvenute.
//!
//! Il caso non e' teorico. Il trigger `trg_settings_guard_protected` (mig 0499)
//! nega gli UPDATE sui setting con `is_protected = TRUE` e il suo commento dice
//! esplicitamente: "l'handler ritorna status=error". Quel guard era di fatto
//! muto in UI.
//!
//! Test opportunistico (stesso pattern di `m71_cost_breakdown`): fa skip se
//! mancano server, DB o JWT. Idempotente e indipendente dall'ordine (regola F):
//! crea le proprie righe con un suffisso unico e le rimuove in chiusura.

use sqlx::PgPool;
use std::env;
use uuid::Uuid;

fn base_url() -> String {
    env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://localhost:4000".into())
}
fn jwt() -> Option<String> {
    env::var("NEXUS_TEST_JWT").ok().filter(|s| !s.is_empty())
}
async fn db() -> Option<PgPool> {
    let url = env::var("DATABASE_URL").ok()?;
    PgPool::connect(&url).await.ok()
}

async fn seed(pool: &PgPool, key: &str, protected: bool) {
    let _ = sqlx::query("DELETE FROM settings WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await;
    sqlx::query(
        "INSERT INTO settings (key, value, category, description, is_secret, is_protected) \
         VALUES ($1, 'valore-iniziale', 'test', 'contract test update_setting', FALSE, $2)",
    )
    .bind(key)
    .bind(protected)
    .execute(pool)
    .await
    .expect("seed della riga di test");
}

async fn cleanup(pool: &PgPool, key: &str) {
    let _ = sqlx::query("DELETE FROM settings WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await;
}

/// Il token viaggia nel COOKIE, non nell'header Authorization:
/// `nexus_auth::validate_token` lo estrae solo da li' (`extract_token_from_cookie`).
/// Con `bearer_auth` la risposta e' sempre 401, qualunque sia il contratto sotto test.
async fn put_setting(token: &str, key: &str, value: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{}/api/admin/setting/{}", base_url(), key))
        .header("Cookie", format!("token={token}"))
        .json(&serde_json::json!({ "value": value }))
        .send()
        .await
        .expect("PUT /api/admin/setting")
}

/// Una scrittura che il DB rifiuta deve essere non-2xx: il client la vede.
#[tokio::test]
async fn scrittura_rifiutata_dal_db_non_e_un_200() {
    let Some(token) = jwt() else {
        eprintln!("skip: NEXUS_TEST_JWT non impostato");
        return;
    };
    let Some(pool) = db().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };

    // Suffisso unico: due esecuzioni concorrenti non si pestano i piedi.
    let key = format!("test.update_contract.protected.{}", Uuid::new_v4());
    seed(&pool, &key, true).await;

    // Valore DIVERSO da quello seedato: il trigger scatta su
    // `NEW.value IS DISTINCT FROM OLD.value`.
    let res = put_setting(&token, &key, "valore-nuovo").await;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();

    // Prova che la scrittura e' stata davvero rifiutata: il valore non e'
    // cambiato. Senza questa asserzione un 500 per un motivo qualsiasi
    // (es. server assente) farebbe passare il test per la ragione sbagliata.
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&pool)
        .await
        .expect("rilettura del valore");
    cleanup(&pool, &key).await;

    assert_eq!(
        stored.as_deref(),
        Some("valore-iniziale"),
        "il guard della mig 0499 deve aver negato l'UPDATE"
    );
    assert!(
        status.is_server_error(),
        "una scrittura negata dal DB deve rispondere non-2xx (era 200 con \
         l'errore nascosto nel body, quindi invisibile al client). Status: {status}, body: {body}"
    );
}

/// Il contro-caso: una scrittura che riesce resta un 200 con esito `ok`.
/// Senza questo, un handler che rispondesse sempre 500 passerebbe il test sopra.
#[tokio::test]
async fn scrittura_riuscita_e_un_200_ok() {
    let Some(token) = jwt() else {
        eprintln!("skip: NEXUS_TEST_JWT non impostato");
        return;
    };
    let Some(pool) = db().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };

    let key = format!("test.update_contract.normale.{}", Uuid::new_v4());
    seed(&pool, &key, false).await;

    let res = put_setting(&token, &key, "valore-nuovo").await;
    let status = res.status();
    let body: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);

    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&pool)
        .await
        .expect("rilettura del valore");
    cleanup(&pool, &key).await;

    assert_eq!(status.as_u16(), 200, "body: {body}");
    assert_eq!(body.get("status").and_then(|s| s.as_str()), Some("ok"));
    assert_eq!(stored.as_deref(), Some("valore-nuovo"));
}

/// Il PUT aggiorna, non crea: una chiave assente e' un 404 e NON lascia righe.
///
/// Regressione catturata: l'handler ripiegava su un `INSERT ... 'custom'` e
/// rispondeva 201 `created`. Un refuso nel nome della chiave (da una pagina
/// admin) creava cosi' una riga fantasma in categoria 'custom' con descrizione
/// vuota: invisibile a `listAdminSettingsByCategory`, che filtra per categoria
/// naturale, ma regolarmente leggibile da `get_setting`. Il sistema leggeva un
/// valore che la UI non mostrava piu'.
#[tokio::test]
async fn chiave_assente_e_un_404_e_non_crea_la_riga() {
    let Some(token) = jwt() else {
        eprintln!("skip: NEXUS_TEST_JWT non impostato");
        return;
    };
    let Some(pool) = db().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };

    // Mai seedata: e' il refuso che l'admin potrebbe digitare.
    let key = format!("test.update_contract.refuso.{}", Uuid::new_v4());
    cleanup(&pool, &key).await;

    let res = put_setting(&token, &key, "valore-fantasma").await;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();

    // L'asserzione che porta il peso: la riga non deve esistere. Un 404 da solo
    // non basterebbe — potrebbe arrivare da una rotta sbagliata mentre l'INSERT
    // e' avvenuto lo stesso.
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&pool)
        .await
        .expect("rilettura del valore");
    cleanup(&pool, &key).await;

    assert_eq!(
        stored, None,
        "il PUT su una chiave assente non deve creare la riga (era un INSERT in \
         categoria 'custom', invisibile alle pagine admin ma letto da get_setting)"
    );
    assert_eq!(
        status.as_u16(),
        404,
        "una chiave assente deve rispondere 404 (era 201 created). Body: {body}"
    );
}
