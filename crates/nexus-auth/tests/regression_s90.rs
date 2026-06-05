//! Regressione S90: verify_session_token (ora `check_session_exists`) NON deve
//! ritornare false su DB error. Prima del fix `.unwrap_or(false)` mascherava
//! ogni outage Postgres come "tutti gli utenti non loggati" -> 401 a tappeto e
//! diagnosi sbagliata garantita.
//!
//! Test in `tests/` (integration) per non gravare il binario principale di
//! dipendenze test-only.

use axum::http::StatusCode;
use nexus_auth::check_session_exists;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

#[tokio::test]
async fn db_unreachable_returns_500_not_401() {
    // Pool puntato a un endpoint senza listener: la prima query fallisce a
    // tempo di acquire/execute. `connect_lazy` non valida l'URL all'avvio,
    // l'errore arriva alla prima fetch_one.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/nonexistent")
        .expect("connect_lazy non deve fallire (URL parsing valido)");

    let res = check_session_exists(&pool, "deadbeefdeadbeef").await;

    match res {
        Err(StatusCode::INTERNAL_SERVER_ERROR) => {
            // OK: comportamento atteso post-fix S90.
        }
        Err(other) => {
            panic!("S90 regressione: atteso INTERNAL_SERVER_ERROR, ricevuto {other}");
        }
        Ok(v) => {
            panic!(
                "S90 regressione: atteso Err(500), ricevuto Ok({v}). \
                 Il fix di nexus-auth::check_session_exists e' stato annullato \
                 e gli utenti vedono di nuovo 401 su DB down."
            );
        }
    }
}
