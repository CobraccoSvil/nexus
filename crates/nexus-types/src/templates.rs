//! Prompt template: cache TTL + loader DB (punto unico, regola L / ADR 0026).
//!
//! Prima questa logica era duplicata IDENTICA in `mcp-core` e `admin-service`,
//! con due cache TTL non coordinate sulla stessa tabella `nexus_prompt_templates`
//! (rischio di prompt incoerenti tra chat e admin). Ora vive qui una volta sola:
//! la logica di scadenza e' in `nexus_cache::TtlCache`, questo modulo aggiunge la
//! specializzazione (TTL 60s, chiave->contenuto) e il caricamento dal DB.

use std::time::Duration;

use nexus_cache::TtlCache;
use sqlx::PgPool;

/// Cache dei prompt template (chiave -> contenuto) con TTL di 60 secondi.
///
/// Incapsula `TtlCache` esponendo l'API attesa dai call site esistenti
/// (`new`/`get`/`set`/`invalidate`).
#[derive(Clone, Debug)]
pub struct TemplateCache(TtlCache<String, String>);

impl TemplateCache {
    /// Crea una nuova cache con TTL di 60 secondi.
    ///
    /// # Esempi
    ///
    /// ```
    /// use nexus_types::TemplateCache;
    ///
    /// let cache = TemplateCache::new();
    /// // Chiave assente restituisce None
    /// assert!(cache.get("missing").is_none());
    /// ```
    pub fn new() -> Self {
        Self(TtlCache::new(Duration::from_secs(60)))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.0.get(key)
    }

    pub fn set(&self, key: String, value: String) {
        self.0.insert(key, value);
    }

    pub fn invalidate(&self, key: &str) {
        self.0.invalidate(key);
    }
}

impl Default for TemplateCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Carica un prompt template dal DB (singola fonte di verita').
///
/// Priorita':
/// 1. Cache in-memory (TTL 60s)
/// 2. DB PostgreSQL (`nexus_prompt_templates` WHERE is_active=TRUE)
/// 3. Stringa vuota con log errore critico
///
/// Tutti i template di sistema devono essere presenti nel DB via migration.
/// Se manca un template, il log errore indica esattamente quale chiave aggiungere.
pub async fn get_template_or_default(db: &PgPool, cache: &TemplateCache, key: &str) -> String {
    if let Some(cached) = cache.get(key) {
        return cached;
    }
    let result = sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = TRUE",
    )
    .bind(key)
    .fetch_optional(db)
    .await;
    match result {
        Ok(Some(content)) => {
            cache.set(key.to_string(), content.clone());
            content
        }
        Ok(None) => {
            tracing::error!(
                "PROMPT TEMPLATE MANCANTE: key='{}' non trovata in nexus_prompt_templates \
                 o disabilitata. Aggiungila tramite /admin/prompts o migration.",
                key
            );
            String::new()
        }
        Err(e) => {
            tracing::error!("Errore lettura prompt template '{}': {}", key, e);
            String::new()
        }
    }
}
