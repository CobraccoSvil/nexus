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

/// Chiave del setting che seleziona la variante INGLESE dei template
/// (A/B lingua, fase 5b, mig 0725): CSV di chiavi di `nexus_prompt_templates`
/// da servire nella variante `<chiave>.en`. Vuoto = tutti i template in
/// italiano. Regola G: il flip e' un UPDATE del setting, il rollback e'
/// svuotare il CSV, niente redeploy.
pub const ENGLISH_VARIANTS_SETTING_KEY: &str = "prompt.english_variants";

/// Suffisso delle righe di variante inglese in `nexus_prompt_templates`.
pub const ENGLISH_VARIANT_SUFFIX: &str = ".en";

/// La SELECT unica sulla tabella dei template: la variante EN e quella IT
/// escono dalla stessa query, o le due strade divergerebbero al primo filtro
/// aggiunto a una sola delle due (regola L).
async fn fetch_active_content(db: &PgPool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = TRUE",
    )
    .bind(key)
    .fetch_optional(db)
    .await
}

/// La chiave e' fra quelle flippate alla variante inglese?
///
/// Legge il CSV dal punto unico dei settings (`nexus_auth::get_csv_setting`,
/// cache 60s per pool): a regime la domanda non costa un round-trip in piu'.
async fn variante_inglese_selezionata(db: &PgPool, key: &str) -> bool {
    nexus_auth::get_csv_setting(db, ENGLISH_VARIANTS_SETTING_KEY)
        .await
        .iter()
        .any(|voce| voce == key)
}

/// Carica un prompt template dal DB (singola fonte di verita').
///
/// Priorita':
/// 1. Cache in-memory (TTL 60s)
/// 2. Variante INGLESE `<chiave>.en`, SOLO se la chiave e' elencata nel CSV
///    del setting `prompt.english_variants` E la riga `.en` e' attiva
///    (A/B lingua fase 5b, mig 0725). Riga `.en` assente o illeggibile =
///    degrado DICHIARATO con WARN e si serve la riga italiana: il flip di un
///    template non migrato non puo' produrre un prompt vuoto.
/// 3. DB PostgreSQL (`nexus_prompt_templates` WHERE is_active=TRUE)
/// 4. Stringa vuota con log errore critico
///
/// CACHE: il contenuto risolto (IT o EN) e' memorizzato sotto la chiave
/// RICHIESTA, con lo stesso TTL 60s di ogni template: un flip del setting si
/// propaga entro il TTL, la stessa disciplina di ogni modifica a caldo.
///
/// Tutti i template di sistema devono essere presenti nel DB via migration.
/// Se manca un template, il log errore indica esattamente quale chiave aggiungere.
pub async fn get_template_or_default(db: &PgPool, cache: &TemplateCache, key: &str) -> String {
    if let Some(cached) = cache.get(key) {
        return cached;
    }
    if variante_inglese_selezionata(db, key).await {
        let chiave_en = format!("{key}{ENGLISH_VARIANT_SUFFIX}");
        match fetch_active_content(db, &chiave_en).await {
            Ok(Some(content)) => {
                cache.set(key.to_string(), content.clone());
                return content;
            }
            Ok(None) => tracing::warn!(
                "Variante EN selezionata per '{}' ma riga '{}' assente o disabilitata: \
                 servo la riga italiana.",
                key,
                chiave_en
            ),
            Err(e) => tracing::warn!(
                "Errore lettura variante EN '{}': {} — servo la riga italiana.",
                chiave_en,
                e
            ),
        }
    }
    match fetch_active_content(db, key).await {
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
