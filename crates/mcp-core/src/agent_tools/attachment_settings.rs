//! Cache 60s per i limiti dei tool di ingestion allegati.
//!
//! Niente hardcoded: i limiti vivono in `settings` (key `agent.attachment.*`)
//! con default safe applicati alla creazione della tabella tramite mig 0193.
//! La cache evita di interrogare il DB ad ogni chiamata tool.

use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::RwLock;

/// Limiti operativi per i tool di ingestion allegati.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentLimits {
    /// Max byte letti per una entry estratta da archivio (nexus_read_archive_entry).
    pub archive_entry_max_bytes: usize,
    /// Max entries elencate da nexus_list_archive_entries.
    pub archive_max_entries: usize,
    /// Max byte di testo estratto da PDF in totale.
    pub pdf_max_text_bytes: usize,
    /// Max righe restituite da nexus_extract_xlsx_data.
    pub xlsx_max_rows: usize,
    /// Max byte estratti da canvas.fig per nexus_extract_figma_structure.
    pub figma_max_bytes: usize,
    /// TTL in secondi della cache read_cache (deduplica letture allegati).
    /// Vedi ADR 0012 FIX 2. Default 300s (5 min).
    pub read_cache_ttl_seconds: usize,
    /// Max byte caricati in RAM dal file `ai_chat.json` di un archivio
    /// Figma Make prima della parsing. Default 5 MB. Se il file e' piu'
    /// grande, viene troncato e si segnala con `ai_chat_truncated_at_load`.
    pub figma_make_ai_chat_max_load_bytes: usize,
    /// Max byte di testo cumulativo estratto da `ai_chat.json` (sommando
    /// user+assistant `parts.text`). Default 50 KB.
    pub figma_make_chat_messages_max_chars: usize,
    /// Max numero di messaggi totali (user + assistant) restituiti dal
    /// thread chat AI Figma Make. Default 20.
    pub figma_make_chat_messages_max_count: usize,
    /// Max caratteri di un singolo messaggio assistant prima della
    /// truncatura (i messaggi user non vengono mai troncati singolarmente:
    /// e' il prompt originale dell'utente). Default 2000.
    pub figma_make_assistant_message_max_chars: usize,
}

impl AttachmentLimits {
    /// Default safe (mai usati come fallback in caso di DB up: i valori
    /// arrivano dalla mig 0193). Fungono solo da "ultima rete" se DB down e
    /// la cache e' vuota — comportamento documentato in modulo.
    pub(crate) const fn safe_defaults() -> Self {
        Self {
            archive_entry_max_bytes: 200 * 1024,
            archive_max_entries: 1000,
            pdf_max_text_bytes: 100 * 1024,
            xlsx_max_rows: 1000,
            figma_max_bytes: 50 * 1024,
            read_cache_ttl_seconds: 300,
            figma_make_ai_chat_max_load_bytes: 5 * 1024 * 1024,
            figma_make_chat_messages_max_chars: 50 * 1024,
            figma_make_chat_messages_max_count: 20,
            figma_make_assistant_message_max_chars: 2000,
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
            'agent.attachment.archive_entry_max_bytes', \
            'agent.attachment.archive_max_entries', \
            'agent.attachment.pdf_max_text_bytes', \
            'agent.attachment.xlsx_max_rows', \
            'agent.attachment.figma_max_bytes', \
            'agent.attachment.read_cache_ttl_seconds', \
            'agent.attachment.figma_make_ai_chat_max_load_bytes', \
            'agent.attachment.figma_make_chat_messages_max_chars', \
            'agent.attachment.figma_make_chat_messages_max_count', \
            'agent.attachment.figma_make_assistant_message_max_chars' \
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
            "agent.attachment.archive_entry_max_bytes" => limits.archive_entry_max_bytes = v,
            "agent.attachment.archive_max_entries" => limits.archive_max_entries = v,
            "agent.attachment.pdf_max_text_bytes" => limits.pdf_max_text_bytes = v,
            "agent.attachment.xlsx_max_rows" => limits.xlsx_max_rows = v,
            "agent.attachment.figma_max_bytes" => limits.figma_max_bytes = v,
            "agent.attachment.read_cache_ttl_seconds" => limits.read_cache_ttl_seconds = v,
            "agent.attachment.figma_make_ai_chat_max_load_bytes" => {
                limits.figma_make_ai_chat_max_load_bytes = v
            }
            "agent.attachment.figma_make_chat_messages_max_chars" => {
                limits.figma_make_chat_messages_max_chars = v
            }
            "agent.attachment.figma_make_chat_messages_max_count" => {
                limits.figma_make_chat_messages_max_count = v
            }
            "agent.attachment.figma_make_assistant_message_max_chars" => {
                limits.figma_make_assistant_message_max_chars = v
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
