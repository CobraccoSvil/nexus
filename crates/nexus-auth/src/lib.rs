pub mod llm_timeouts;

use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

// --- JWT Claims ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String, // user_id
    pub role: String,
    pub exp: usize,
}

// --- Helpers ---
//
// Lettura settings: punto unico (regola L / ADR 0026). La query SQL vive solo
// in `read_setting_raw`; tutte le viste (Result/Option, raw/trim, bool/int)
// delegano qui. Niente query `SELECT ... FROM settings` duplicate nei crate.

/// Query unica della tabella `settings`. Punto di verita' della lettura.
async fn read_setting_raw(db: &PgPool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
}

/// Legge una setting propagando l'errore DB (regola H: non ingoiare). Valore
/// RAW: nessun trim, nessun filtro sui vuoti.
pub async fn get_setting_checked(db: &PgPool, key: &str) -> anyhow::Result<Option<String>> {
    read_setting_raw(db, key)
        .await
        .map_err(|e| anyhow::anyhow!("lettura setting '{key}' fallita: {e}"))
}

/// Come `get_setting_checked` ma con `trim()` e scartando i valori vuoti.
pub async fn get_setting_nonempty(db: &PgPool, key: &str) -> anyhow::Result<Option<String>> {
    Ok(get_setting_checked(db, key)
        .await?
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

/// Variante best-effort che ingoia l'errore DB ritornando `None` (con trim e
/// scarto dei vuoti). Mantenuta per i call site storici; per il codice NUOVO
/// preferire `get_setting_checked`/`get_setting_nonempty`, che propagano.
pub async fn get_setting(db: &PgPool, key: &str) -> Option<String> {
    read_setting_raw(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// Scrittura settings: punto unico (regola L / ADR 0026), accanto alla lettura.
// Prima la stessa logica viveva duplicata in `mcp-core::settings::update_setting`
// e `admin-service::settings::update_setting`.

/// Errore di una scrittura su `settings`.
#[derive(Debug, thiserror::Error)]
pub enum SettingWriteError {
    /// La chiave non esiste. Il PUT aggiorna, non crea: vedi
    /// `update_setting_value` per il razionale.
    #[error("setting '{0}' inesistente: le chiavi si creano da una migrazione, non da una scrittura")]
    UnknownKey(String),
    #[error("scrittura setting '{key}' fallita: {source}")]
    Db { key: String, source: sqlx::Error },
}

impl SettingWriteError {
    /// Status HTTP dell'errore. Vive qui perche' la mappatura e' la STESSA per
    /// ogni chiamante (regola L): chiave assente => 404, DB che rifiuta => 500.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::UnknownKey(_) => StatusCode::NOT_FOUND,
            Self::Db { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Aggiorna il valore di una setting ESISTENTE. Punto di verita' della scrittura.
///
/// Una chiave assente e' un errore (`UnknownKey`), non un invito a crearla.
/// Prima l'handler ripiegava su un `INSERT ... VALUES (key, value, 'custom', '',
/// FALSE)`, e un refuso nel nome creava una riga NUOVA invece di dare errore: la
/// UI rispondeva "salvato" a una scrittura senza alcun effetto. Chi scrive
/// `nexus_behavior_moda` crede di aver cambiato il comportamento, ma il sistema
/// continua a leggere `nexus_behavior_mode` — nessuno legge mai la chiave col
/// refuso. Il danno e' la scrittura silenziosamente inefficace, piu' la
/// spazzatura che si accumula in 'custom'.
///
/// NON e' un problema di invisibilita': la riga si vede eccome. `list_categories`
/// e' data-driven (`GROUP BY category`, nessuna whitelist) e `buildList`
/// (`apps/web-ide/lib/settings-categories.ts`) accoda in fondo ogni categoria
/// sconosciuta, quindi 'custom' compare nella sidebar ed e' navigabile.
/// Verificato contro il DB, non dedotto.
///
/// Il censimento dei chiamanti (pagine admin, plugin manager, toggle provider,
/// pannello orchestrator, council) non ha trovato un flusso che dipenda dalla
/// creazione: scrivono chiavi seedate da migrazione, o chiavi lette da
/// `GET /api/admin/settings`, che per costruzione esistono gia'. Chi ha bisogno
/// di una chiave nuova la dichiara alla fonte, con categoria e `is_secret`
/// veri: le migrazioni per i default statici, `plugins::integrate::publish` per
/// i secret dei plugin integrati a runtime.
pub async fn update_setting_value(
    db: &PgPool,
    key: &str,
    value: &str,
) -> Result<(), SettingWriteError> {
    let result = sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = $2")
        .bind(value)
        .bind(key)
        .execute(db)
        .await
        .map_err(|source| SettingWriteError::Db {
            key: key.to_string(),
            source,
        })?;

    if result.rows_affected() == 0 {
        return Err(SettingWriteError::UnknownKey(key.to_string()));
    }
    Ok(())
}

/// Legge una setting booleana (`true`/`1`/`yes`/`on` => true). Propaga l'errore DB.
pub async fn get_bool_setting(db: &PgPool, key: &str) -> anyhow::Result<Option<bool>> {
    Ok(get_setting_nonempty(db, key)
        .await?
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")))
}

/// Legge una setting intera. Propaga l'errore DB; valore non numerico => errore.
pub async fn get_int_setting(db: &PgPool, key: &str) -> anyhow::Result<Option<i64>> {
    match get_setting_nonempty(db, key).await? {
        Some(v) => Ok(Some(v.parse::<i64>().map_err(|e| {
            anyhow::anyhow!("setting '{key}' non e' un intero valido ('{v}'): {e}")
        })?)),
        None => Ok(None),
    }
}

/// Risolve la porta di bind di un servizio leggendola ESCLUSIVAMENTE dal DB
/// (tabella `settings`, regola G del CLAUDE.md: il DB e' l'unica fonte di
/// verita' per la configurazione). Nessun default hardcoded e nessuna env var:
/// se il valore non e' disponibile il servizio PANICA con un messaggio chiaro,
/// coerente con `RoutingMatrixCache::init` di mcp-core.
///
/// - `key`: chiave in `settings` (es. "admin_service_port").
/// - DB irraggiungibile: retry 5 tentativi x 5s (il container Postgres puo'
///   essere ancora in avvio), poi panic.
/// - chiave assente o valore non valido: panic immediato (config errata /
///   migrazione 0239 non applicata): meglio non partire che fare bind su una
///   porta sbagliata silenziosamente.
pub async fn resolve_port(db: &PgPool, key: &str) -> u16 {
    for attempt in 1..=5u32 {
        match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await
        {
            Ok(Some(raw)) => {
                let v = raw.trim();
                return v.parse::<u16>().ok().filter(|p| *p > 0).unwrap_or_else(|| {
                    panic!(
                        "resolve_port: settings.{key} = {v:?} non e' una porta valida (1..=65535). \
                         Correggi il valore nel DB."
                    )
                });
            }
            Ok(None) => panic!(
                "resolve_port: settings.{key} assente nel DB. Applica la migrazione \
                 db/migrations/0239_infrastructure_ports.sql (regola G: niente porte hardcoded)."
            ),
            Err(e) if attempt < 5 => {
                tracing::warn!(
                    "resolve_port: tentativo {attempt}/5 lettura settings.{key} fallito ({e}). Retry in 5s..."
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => panic!(
                "resolve_port: impossibile leggere settings.{key} dal DB dopo 5 tentativi: {e}. \
                 Verifica che Postgres sia raggiungibile e che la migrazione 0239 sia applicata."
            ),
        }
    }
    unreachable!("resolve_port: loop di retry terminato senza esito per {key}")
}

pub async fn get_or_create_jwt_secret(db: &PgPool) -> anyhow::Result<String> {
    if let Some(secret) = get_setting(db, "jwt_secret").await {
        return Ok(secret);
    }
    let secret: String = (0..64)
        .map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>()))
        .collect();
    sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = 'jwt_secret'")
        .bind(&secret)
        .execute(db)
        .await?;
    Ok(secret)
}

pub fn frontend_url() -> String {
    std::env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string())
}

pub fn backend_url() -> String {
    std::env::var("PUBLIC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:4000".to_string())
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn extract_token_from_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| {
            let c = c.trim();
            c.strip_prefix("token=").map(|v| v.to_string())
        })
}

// --- Token validation ---

pub async fn validate_token(
    db: &PgPool,
    headers: &axum::http::HeaderMap,
) -> Result<Claims, StatusCode> {
    let token = extract_token_from_cookie(headers).ok_or(StatusCode::UNAUTHORIZED)?;

    let jwt_secret = get_setting(db, "jwt_secret")
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token_data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Verify session exists and not expired
    let token_hash = hash_token(&token);
    if !check_session_exists(db, &token_hash).await? {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(token_data.claims)
}

/// Verifica nel DB se esiste una sessione non scaduta col dato hash.
///
/// Estratta da `validate_token` per consentire test di regressione sul
/// comportamento "DB down -> 500, NON 401" (fix S90, regola H).
pub async fn check_session_exists(
    db: &PgPool,
    token_hash: &str,
) -> Result<bool, StatusCode> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE token_hash = $1 AND expires_at > NOW())",
    )
    .bind(token_hash)
    .fetch_one(db)
    .await
    .map_err(|e| {
        // Fix regola H (S90): prima `.unwrap_or(false)` -> tutti gli utenti
        // ricevevano 401 quando il DB cadeva, diagnosi sbagliata garantita.
        tracing::error!("check_session_exists: SELECT sessions fallita: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// --- Middleware ---

/// Middleware that requires a valid JWT token.
/// Inserts Claims into request extensions on success.
pub async fn require_auth<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = validate_token(&db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Middleware that requires a valid JWT token with admin role.
pub async fn require_admin<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    match validate_token(&db, req.headers()).await {
        Ok(claims) => {
            if claims.role != "admin" {
                tracing::warn!(
                    "require_admin: access denied - role={} is not admin, path={}",
                    claims.role,
                    req.uri()
                );
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            tracing::warn!("require_admin: token validation failed: {:?}, path={}", e, req.uri());
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;

    /// Regressione S90: con DB irraggiungibile, check_session_exists deve
    /// ritornare INTERNAL_SERVER_ERROR, NON false (che -> 401 Unauthorized).
    /// Prima del fix  mascherava ogni DB outage come
    /// "tutti gli utenti non sono loggati" - diagnosi catastroficamente
    /// sbagliata.
    #[tokio::test]
    async fn s90_db_down_returns_500_not_401() {
        // Pool puntato a una porta senza listener: la prima query fallira.
        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/nonexistent")
        {
            Ok(p) => p,
            Err(_) => return, // ambiente senza supporto: skip
        };

        let res = check_session_exists(&pool, "deadbeef").await;
        match res {
            Err(StatusCode::INTERNAL_SERVER_ERROR) => {}
            Err(other) => panic!("S90: atteso 500, ricevuto {other}"),
            Ok(v) => panic!("S90: atteso Err(500), ricevuto Ok({v}) - regressione!"),
        }
    }
}
