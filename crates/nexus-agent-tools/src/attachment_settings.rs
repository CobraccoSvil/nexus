//! Cache 60s per i parametri operativi dei tool di ingestion allegati.
//!
//! Niente hardcoded: i parametri vivono in `settings` (key `agent.attachment.*`)
//! con default safe applicati dalle migrazioni. La cache evita di interrogare
//! il DB ad ogni chiamata tool.
//!
//! POLITICA "MAI TRONCARE-E-BUTTARE" (mig 0216):
//! i vecchi cap di contenuto (archive_entry_max_bytes, archive_max_entries,
//! pdf_max_text_bytes, xlsx_max_rows, figma_max_bytes, figma_make_chat_*,
//! figma_make_code_max_total_bytes) sono stati ELIMINATI. L'estrazione
//! processa sempre l'INTERO contenuto. Quando il risultato e' grande viene
//! scritto su disco (es. nexus_extract_figma_code) e/o indicizzato in RAG,
//! restituendo all'agente un puntatore invece del troncamento.
//!
//! Restano solo due parametri, nessuno dei quali e' un budget di contenuto:
//! - `read_cache_ttl_seconds`: TTL cache deduplica letture (igiene/performance,
//!   vedi ADR 0012 FIX 2).
//! - `figma_make_ai_chat_max_load_bytes`: guardia anti-OOM ESTREMA sul
//!   caricamento in RAM di `ai_chat.json` (default 512 MB), rete di sicurezza
//!   contro file patologici, non un cap di contenuto.

use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::RwLock;

/// Parametri operativi per i tool di ingestion allegati.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentLimits {
    /// TTL in secondi della cache read_cache (deduplica letture allegati).
    /// Vedi ADR 0012 FIX 2. Default 300s (5 min). NON e' un cap di contenuto.
    pub read_cache_ttl_seconds: usize,
    /// Guardia anti-OOM ESTREMA (NON un budget di contenuto) sul caricamento in
    /// RAM del file `ai_chat.json` di un archivio Figma Make prima del parsing.
    /// Default 512 MB: serve solo a non esplodere su file patologici. I .make
    /// reali stanno nell'ordine dei MB. Se superato, il caricamento si ferma e
    /// l'estrazione e' segnalata come parziale (`ai_chat_truncated_at_load`).
    pub figma_make_ai_chat_max_load_bytes: usize,
}

impl AttachmentLimits {
    /// Default safe (mai usati come fallback in caso di DB up: i valori
    /// arrivano dalle migrazioni). Fungono solo da "ultima rete" se DB down e
    /// la cache e' vuota — comportamento documentato in modulo.
    pub const fn safe_defaults() -> Self {
        Self {
            read_cache_ttl_seconds: 300,
            // Guardia anti-OOM altissima: 512 MB. NON e' un budget di contenuto.
            figma_make_ai_chat_max_load_bytes: 512 * 1024 * 1024,
        }
    }
}

const CACHE_TTL: Duration = Duration::from_secs(60);

static LIMITS_CACHE: Lazy<RwLock<Option<(AttachmentLimits, Instant)>>> =
    Lazy::new(|| RwLock::new(None));

/// Carica i limiti dalla tabella `settings`, con cache 60s.
///
/// Se il DB e' down o le chiavi mancano, ritorna i `safe_defaults()` con WARN.
/// Niente fallback nascosto: l'amministratore vede l'errore in log.
pub async fn current(db: &PgPool) -> AttachmentLimits {
    {
        let guard = LIMITS_CACHE.read().await;
        if let Some((value, expires)) = *guard {
            if Instant::now() < expires {
                return value;
            }
        }
    }

    let limits = match load_from_db(db).await {
        Ok(l) => l,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "attachment_settings: lettura settings agent.attachment.* fallita, uso safe_defaults"
            );
            AttachmentLimits::safe_defaults()
        }
    };

    let mut guard = LIMITS_CACHE.write().await;
    *guard = Some((limits, Instant::now() + CACHE_TTL));
    limits
}

async fn load_from_db(db: &PgPool) -> Result<AttachmentLimits, sqlx::Error> {
    use sqlx::Row;

    let rows = sqlx::query(
        "SELECT key, value FROM settings \
         WHERE key IN ( \
            'agent.attachment.read_cache_ttl_seconds', \
            'agent.attachment.figma_make_ai_chat_max_load_bytes' \
         )",
    )
    .fetch_all(db)
    .await?;

    let mut limits = AttachmentLimits::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        let parsed: Option<usize> = raw.trim().parse().ok();
        let Some(v) = parsed else { continue };
        match key.as_str() {
            "agent.attachment.read_cache_ttl_seconds" => limits.read_cache_ttl_seconds = v,
            "agent.attachment.figma_make_ai_chat_max_load_bytes" => {
                limits.figma_make_ai_chat_max_load_bytes = v
            }
            _ => {}
        }
    }
    Ok(limits)
}

#[cfg(test)]
pub async fn _reset_for_tests() {
    let mut guard = LIMITS_CACHE.write().await;
    *guard = None;
}
