//! Punto unico (regola L) per l'URL REST del brain Python.
//!
//! Causa radice (incidente Beauty-Book 2026-06-11): la nozione "URL REST del
//! brain" era duplicata in 8+ call site con fallback hardcoded divergenti; i due
//! moduli rag/ (search, indexer) avevano il refuso `127.0.0.1:8088` mentre il
//! brain gira sulla porta in `settings.brain_rest_port` (8001). Risultato:
//! `nexus_search_semantic` falliva SEMPRE l'embed della query ("brain embed
//! endpoint fallito: post http://127.0.0.1:8088/embed") — il recupero RAG del
//! contesto offloadato era strutturalmente rotto, e i modelli ri-leggevano i
//! file da capo (loop esplorativi).
//!
//! Fonte di verita' (regola G): `settings.brain_rest_port`, la STESSA chiave con
//! cui il brain binda la porta (brain/utils/settings_db.resolve_port). Env var
//! `BRAIN_REST_URL` resta come override d'emergenza documentato. Cache 60s.

use sqlx::PgPool;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
const TTL_SECONDS: u64 = 60;

/// URL base REST del brain (senza slash finale). Gerarchia: env override
/// d'emergenza > settings.brain_rest_port (DB, cache 60s) > fallback 8001
/// (coerente con gli altri call site, loggato WARN se la chiave manca).
pub async fn brain_rest_base_url(db: &PgPool) -> String {
    if let Ok(v) = std::env::var("BRAIN_REST_URL") {
        let v = v.trim();
        if !v.is_empty() {
            return v.trim_end_matches('/').to_string();
        }
    }
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((url, ts)) = guard.as_ref() {
            if ts.elapsed().as_secs() < TTL_SECONDS {
                return url.clone();
            }
        }
    }
    let port = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'brain_rest_port'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse::<u16>().ok());
    let url = match port {
        Some(p) => format!("http://127.0.0.1:{p}"),
        None => {
            tracing::warn!(
                "brain_url: settings.brain_rest_port assente/illeggibile, fallback 8001"
            );
            "http://127.0.0.1:8001".to_string()
        }
    };
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((url.clone(), Instant::now()));
    }
    url
}
