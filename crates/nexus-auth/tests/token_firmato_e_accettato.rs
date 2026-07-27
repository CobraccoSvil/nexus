//! Contract test: un token firmato con `settings.jwt_secret` e con la sua
//! sessione viva DEVE essere accettato da `validate_token`.
//!
//! # Perche' esiste
//!
//! Le due meta' dell'autenticazione leggono il segreto per strade diverse: chi
//! FIRMA passa da `get_or_create_jwt_secret` (query diretta con
//! `UPDATE ... RETURNING`, che rigenera il valore se e' vuoto), chi VALIDA passa
//! da `get_setting` (lettura con cache TTL). Finche' le due strade danno lo stesso
//! valore tutto funziona; se divergono, il sistema emette credenziali che
//! rifiuta, e dall'esterno il sintomo e' un 401 indistinguibile da una password
//! sbagliata.
//!
//! Il 2026-07-26, diagnosticando un 401 sistematico sui token del dev-login, la
//! verifica manuale (HMAC ricalcolato a mano contro il segreto del DB, sessione
//! inserita a mano e confermata `valida=true` dal DB) non e' bastata a dire da
//! che parte fosse il difetto: mancava un punto in cui la coerenza
//! firma-validazione fosse asserita dal CODICE, senza passare da un servizio in
//! ascolto e dal suo binario, che puo' essere piu' vecchio dei sorgenti.
//!
//! Questo test chiude quel buco: firma con lo stesso helper della produzione,
//! apre la sessione come fa il login, e chiede a `validate_token` di accettarlo.
//! Se il segreto letto in scrittura e quello letto in validazione divergono, qui
//! si vede subito e si vede DOVE.
//!
//! Idempotente (regola F): usa un utente e un token propri, li rimuove in
//! chiusura.

use jsonwebtoken::{encode, EncodingKey, Header};
use nexus_auth::{get_or_create_jwt_secret, validate_token, Claims};
use nexus_test_preconditions::db_o_salta;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Lo stesso hash che `validate_token` calcola sul token per cercare la sessione
/// (sha256 esadecimale). Vive anche in `nexus-auth` come `hash_token`, privato:
/// qui la formula e' ripetuta perche' e' il DATO su cui il test deve agire, e una
/// divergenza si manifesterebbe subito come sessione non trovata.
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Utente di prova nel meta: `sessions.user_id` ha una FK su `users`.
async fn crea_utente(db: &PgPool) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, display_name, role)
         VALUES ($1, $2, 'contract test auth', 'admin')",
    )
    .bind(id)
    .bind(format!("auth-contract-{id}@test.local"))
    .execute(db)
    .await?;
    Ok(id)
}

async fn pulisci(db: &PgPool, user_id: Uuid, token_hash: &str) {
    let _ = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(token_hash)
        .execute(db)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(db)
        .await;
}

/// Cookie header come lo manda un browser: `validate_token` estrae il token SOLO
/// da qui (mai da `Authorization: Bearer`).
fn headers_con_cookie(token: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        axum::http::header::COOKIE,
        format!("token={token}").parse().expect("header valido"),
    );
    h
}

#[tokio::test]
async fn un_token_firmato_col_segreto_di_piattaforma_viene_accettato() {
    let Some(db) = db_o_salta().await else { return };

    let user_id = crea_utente(&db).await.expect("utente di prova");

    // FIRMA: lo stesso helper che usano il callback OAuth e il dev-login.
    let secret = get_or_create_jwt_secret(&db)
        .await
        .expect("jwt_secret risolvibile");
    let exp = (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        role: "admin".to_string(),
        exp,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("firma del token");

    // SESSIONE: la scrive il percorso di login (`mcp-core/src/auth.rs`). Un token
    // senza questa riga e' valido come firma e inutilizzabile come credenziale.
    let token_hash = hash_token(&token);
    sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(&token_hash)
        .bind(chrono::Utc::now() + chrono::Duration::hours(1))
        .execute(&db)
        .await
        .expect("apertura della sessione");

    // VALIDAZIONE: la funzione che ogni richiesta autenticata attraversa.
    let esito = validate_token(&db, &headers_con_cookie(&token)).await;

    pulisci(&db, user_id, &token_hash).await;

    let claims_validi = esito.unwrap_or_else(|status| {
        panic!(
            "validate_token ha rifiutato ({status}) un token firmato con \
             get_or_create_jwt_secret e con la sua sessione viva. Le due strade del \
             segreto (UPDATE...RETURNING in scrittura, get_setting con cache in \
             lettura) non stanno dando lo stesso valore: e' la causa di un 401 \
             sistematico su credenziali corrette"
        )
    });
    assert_eq!(claims_validi.sub, user_id.to_string());
    assert_eq!(claims_validi.role, "admin");
}

/// Il contro-caso: SENZA la riga in `sessions` lo stesso token deve essere
/// rifiutato. Senza questo, il test sopra passerebbe anche se `validate_token`
/// ignorasse la sessione — e non proverebbe piu' nulla sul perche' un token
/// firmato bene possa non bastare.
#[tokio::test]
async fn un_token_senza_sessione_viene_rifiutato() {
    let Some(db) = db_o_salta().await else { return };

    let user_id = crea_utente(&db).await.expect("utente di prova");
    let secret = get_or_create_jwt_secret(&db)
        .await
        .expect("jwt_secret risolvibile");
    let exp = (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize;
    let token = encode(
        &Header::default(),
        &Claims {
            sub: user_id.to_string(),
            role: "admin".to_string(),
            exp,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("firma del token");

    let esito = validate_token(&db, &headers_con_cookie(&token)).await;

    pulisci(&db, user_id, &hash_token(&token)).await;

    assert!(
        esito.is_err(),
        "un token senza sessione in `sessions` deve essere rifiutato: la firma \
         attesta il contenuto, non che il login sia avvenuto"
    );
}
