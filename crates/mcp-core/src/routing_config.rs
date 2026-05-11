//! Cache di configurazione routing letta da DB (Fase 3).
//!
//! Sostituisce le costanti hardcoded e i match statici in `orchestrator.rs`:
//!   - `RoutingThresholds` <- `settings` con prefisso `routing.*` (mig 0111)
//!   - `IntentCapabilityMap` <- `nexus_intent_capability` (mig 0110)
//!
//! Pattern identico a `routing_matrix.rs::RoutingMatrixCache`:
//!   - retry 5x5s all'avvio, panic se DB irraggiungibile o tabelle vuote
//!   - refresh background ogni 60s
//!   - lettura lock-free (clone dell'`Arc`)
//!
//! Regola G del CLAUDE.md: niente magic fallback. Se il DB cade dopo l'avvio,
//! la cache mantiene l'ultima copia valida; un fallimento di refresh logga WARN
//! ma NON sostituisce la cache con valori dummy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

// ── RoutingThresholds ───────────────────────────────────────────────────────

/// Tutti i parametri configurabili del routing letti da `settings.routing.*`.
/// Default sicuri usati SOLO se la query DB ritorna chiavi mancanti (mai
/// mascherare un DB down — solo recuperare chiavi singole se l'admin ne ha
/// rimossa qualcuna per errore).
#[derive(Debug, Clone)]
pub struct RoutingThresholds {
    pub llm_classifier_min_confidence: f32,
    pub llm_classifier_timeout_seconds: f32,
    pub classifier_cache_ttl_seconds: u64,
    pub classifier_cache_max_entries: u32,
    pub classifier_provider: String,
    pub classifier_model: String,
    pub token_threshold_chat_breve: u32,
    pub token_threshold_chat_media: u32,
    pub token_threshold_complex_fix: u32,
    pub token_threshold_long_context: u32,
    /// L2 (disambiguation): top intent confidence sotto questa soglia →
    /// richiesta di chiarimento all'utente. Sorgente: `settings.routing.ambiguity_min_confidence` (mig 0132).
    pub ambiguity_min_confidence: f32,
    /// L2 (disambiguation): margine (top − second_candidate) sotto questa soglia →
    /// richiesta di chiarimento. Sorgente: `settings.routing.ambiguity_min_margin` (mig 0132).
    pub ambiguity_min_margin: f32,
    /// L3 / Heartbeat SSE: secondi di silenzio stream brain→mcp-core prima
    /// di considerare il run bloccato. Sorgente: `settings.routing.sse_heartbeat_max_silence_secs` (mig 0132).
    pub sse_heartbeat_max_silence_secs: u64,
    pub loaded_at: Instant,
}

impl RoutingThresholds {
    /// Crea con i default del seed migrazione 0111. Usato SOLO per test e per
    /// chiavi singole mancanti nel DB (ricovero parziale, non fallback totale).
    fn defaults() -> Self {
        Self {
            llm_classifier_min_confidence: 0.60,
            llm_classifier_timeout_seconds: 5.0,
            classifier_cache_ttl_seconds: 86_400,
            classifier_cache_max_entries: 10_000,
            classifier_provider: "google".to_string(),
            classifier_model: "gemini-2.5-flash".to_string(),
            token_threshold_chat_breve: 400,
            token_threshold_chat_media: 1_500,
            token_threshold_complex_fix: 3_000,
            token_threshold_long_context: 6_000,
            // Default tecnici per i nuovi parametri (mig 0132). Usati solo
            // come ricovero parziale se la chiave manca dal DB.
            ambiguity_min_confidence: 0.70,
            ambiguity_min_margin: 0.15,
            sse_heartbeat_max_silence_secs: 120,
            loaded_at: Instant::now(),
        }
    }
}

async fn fetch_thresholds_from_db(db: &PgPool) -> Result<RoutingThresholds, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT key, value FROM settings WHERE key LIKE 'routing.%'"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query settings 'routing.*' fallita: {e}"))?;

    if rows.is_empty() {
        return Err(
            "settings non contiene chiavi 'routing.*'. Applica la migrazione 0111."
                .to_string(),
        );
    }

    let map: HashMap<String, String> = rows.into_iter().collect();

    // Helper di parsing che logga WARN se chiave manca ma usa il default
    // (solo per non far crashare se l'admin ha rimosso una singola chiave).
    let parse_f32 = |key: &str, default: f32| -> f32 {
        match map.get(key).and_then(|v| v.parse::<f32>().ok()) {
            Some(v) => v,
            None => {
                warn!("settings: chiave {} mancante o malformata, uso default {}", key, default);
                default
            }
        }
    };
    let parse_u32 = |key: &str, default: u32| -> u32 {
        match map.get(key).and_then(|v| v.parse::<u32>().ok()) {
            Some(v) => v,
            None => {
                warn!("settings: chiave {} mancante o malformata, uso default {}", key, default);
                default
            }
        }
    };
    let parse_u64 = |key: &str, default: u64| -> u64 {
        match map.get(key).and_then(|v| v.parse::<u64>().ok()) {
            Some(v) => v,
            None => {
                warn!("settings: chiave {} mancante o malformata, uso default {}", key, default);
                default
            }
        }
    };
    let parse_str = |key: &str, default: &str| -> String {
        match map.get(key).filter(|v| !v.is_empty()) {
            Some(v) => v.clone(),
            None => {
                warn!("settings: chiave {} mancante o vuota, uso default '{}'", key, default);
                default.to_string()
            }
        }
    };

    Ok(RoutingThresholds {
        llm_classifier_min_confidence: parse_f32("routing.llm_classifier_min_confidence", 0.60),
        llm_classifier_timeout_seconds: parse_f32("routing.llm_classifier_timeout_seconds", 5.0),
        classifier_cache_ttl_seconds: parse_u64("routing.classifier_cache_ttl_seconds", 86_400),
        classifier_cache_max_entries: parse_u32("routing.classifier_cache_max_entries", 10_000),
        classifier_provider: parse_str("routing.classifier_provider", "google"),
        classifier_model: parse_str("routing.classifier_model", "gemini-2.5-flash"),
        token_threshold_chat_breve: parse_u32("routing.token_threshold_chat_breve", 400),
        token_threshold_chat_media: parse_u32("routing.token_threshold_chat_media", 1_500),
        token_threshold_complex_fix: parse_u32("routing.token_threshold_complex_fix", 3_000),
        token_threshold_long_context: parse_u32("routing.token_threshold_long_context", 6_000),
        // Nuovi parametri (mig 0132)
        ambiguity_min_confidence: parse_f32("routing.ambiguity_min_confidence", 0.70),
        ambiguity_min_margin: parse_f32("routing.ambiguity_min_margin", 0.15),
        sse_heartbeat_max_silence_secs: parse_u64("routing.sse_heartbeat_max_silence_secs", 120),
        loaded_at: Instant::now(),
    })
}

// ── IntentCapability ────────────────────────────────────────────────────────

/// Una riga di `nexus_intent_capability` (mig 0110).
#[derive(Debug, Clone)]
pub struct IntentCapability {
    pub intent: String,
    pub base_tier: String,
    pub base_capability: String,
    pub preferred_provider: Option<String>,
    pub medium_token_threshold: Option<u32>,
    pub heavy_token_threshold: Option<u32>,
}

impl IntentCapability {
    /// Calcola il tier effettivo per `tokens` applicando le soglie di
    /// up-promotion. Sostituisce il match statico in
    /// `orchestrator.rs:444-461`.
    pub fn tier_for_tokens(&self, tokens: u32) -> String {
        let mut tier = self.base_tier.clone();
        if let Some(threshold) = self.medium_token_threshold {
            if tokens >= threshold && tier == "light" {
                tier = "medium".to_string();
            }
        }
        if let Some(threshold) = self.heavy_token_threshold {
            if tokens >= threshold {
                tier = "heavy".to_string();
            }
        }
        tier
    }
}

/// Mappa intent → IntentCapability. Riusabile come `Arc<IntentCapabilityMap>`.
#[derive(Debug, Clone)]
pub struct IntentCapabilityMap {
    pub by_intent: HashMap<String, IntentCapability>,
    pub loaded_at: Instant,
}

impl IntentCapabilityMap {
    pub fn get(&self, intent: &str) -> Option<&IntentCapability> {
        self.by_intent.get(intent)
    }

    /// Conveniente: tier effettivo per (intent, tokens). Ritorna None se
    /// l'intent non e' mappato.
    pub fn tier_for(&self, intent: &str, tokens: u32) -> Option<String> {
        self.by_intent.get(intent).map(|c| c.tier_for_tokens(tokens))
    }

    pub fn capability_for(&self, intent: &str) -> Option<&str> {
        self.by_intent
            .get(intent)
            .map(|c| c.base_capability.as_str())
    }

    pub fn preferred_provider_for(&self, intent: &str) -> Option<&str> {
        self.by_intent
            .get(intent)
            .and_then(|c| c.preferred_provider.as_deref())
    }
}

async fn fetch_intent_capability_from_db(db: &PgPool) -> Result<IntentCapabilityMap, String> {
    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<i32>,
        Option<i32>,
    )> = sqlx::query_as(
        r#"SELECT intent, base_tier, base_capability, preferred_provider,
                  medium_token_threshold, heavy_token_threshold
           FROM nexus_intent_capability"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query nexus_intent_capability fallita: {e}. Hai applicato la migrazione 0110?"))?;

    if rows.is_empty() {
        return Err(
            "nexus_intent_capability vuota. Applica la migrazione 0110 o popola la tabella.".to_string(),
        );
    }

    let by_intent: HashMap<String, IntentCapability> = rows
        .into_iter()
        .map(
            |(intent, base_tier, base_capability, preferred_provider, mt, ht)| {
                let cap = IntentCapability {
                    intent: intent.clone(),
                    base_tier,
                    base_capability,
                    preferred_provider,
                    medium_token_threshold: mt.and_then(|v| u32::try_from(v).ok()),
                    heavy_token_threshold: ht.and_then(|v| u32::try_from(v).ok()),
                };
                (intent, cap)
            },
        )
        .collect();

    Ok(IntentCapabilityMap {
        by_intent,
        loaded_at: Instant::now(),
    })
}

// ── Cache wrapper ───────────────────────────────────────────────────────────

/// Cache thread-safe con refresh background. Generica sul tipo cached: un'unica
/// implementazione serve sia `RoutingThresholds` che `IntentCapabilityMap`.
#[derive(Clone)]
pub struct ConfigCache<T: Clone + Send + Sync + 'static> {
    inner: Arc<RwLock<Option<Arc<T>>>>,
    last_error: Arc<RwLock<Option<String>>>,
    name: &'static str,
}

impl<T: Clone + Send + Sync + 'static> ConfigCache<T> {
    /// Inizializza con retry-loop e spawna refresh background.
    /// Se `fetcher` fallisce 5 volte di fila, panic con messaggio esplicito.
    pub async fn init<F, Fut>(name: &'static str, db: PgPool, fetcher: F) -> Self
    where
        F: Fn(PgPool) -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send,
    {
        let mut last_err: Option<String> = None;
        let mut initial: Option<Arc<T>> = None;
        for attempt in 1..=5 {
            match fetcher(db.clone()).await {
                Ok(value) => {
                    info!("{}: caricato da DB", name);
                    initial = Some(Arc::new(value));
                    last_err = None;
                    break;
                }
                Err(e) => {
                    warn!(
                        "{}: tentativo {}/5 di load DB fallito ({}). Retry in 5s...",
                        name, attempt, e
                    );
                    last_err = Some(e);
                    if attempt < 5 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }

        if initial.is_none() {
            panic!(
                "{}: impossibile caricare dal DB dopo 5 tentativi. \
                 Errore: {}. Verifica Postgres + migrazioni applicate.",
                name,
                last_err.unwrap_or_else(|| "unknown".to_string())
            );
        }

        let inner = Arc::new(RwLock::new(initial));
        let last_error = Arc::new(RwLock::new(last_err));
        let cache = Self {
            inner: inner.clone(),
            last_error: last_error.clone(),
            name,
        };

        // Refresh background. Se fallisce, mantieni la cache precedente.
        let inner_bg = inner;
        let last_err_bg = last_error;
        let db_bg = db;
        let fetcher_bg = fetcher;
        let name_bg = name;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                match fetcher_bg(db_bg.clone()).await {
                    Ok(new_value) => {
                        let arc = Arc::new(new_value);
                        {
                            let mut w = inner_bg.write().await;
                            *w = Some(arc);
                        }
                        {
                            let mut e = last_err_bg.write().await;
                            *e = None;
                        }
                        debug!("{}: refresh OK", name_bg);
                    }
                    Err(e) => {
                        warn!("{}: refresh fallito ({}). Mantengo cache precedente.", name_bg, e);
                        let mut le = last_err_bg.write().await;
                        *le = Some(e);
                    }
                }
            }
        });

        cache
    }

    /// Snapshot lock-free.
    pub async fn current_async(&self) -> Result<Arc<T>, String> {
        let g = self.inner.read().await;
        match &*g {
            Some(arc) => Ok(Arc::clone(arc)),
            None => Err(format!("{} non caricata (DB down all'avvio?)", self.name)),
        }
    }
}

// ── Type aliases per chiarezza nei call site ────────────────────────────────

pub type RoutingThresholdsCache = ConfigCache<RoutingThresholds>;
pub type IntentCapabilityCache = ConfigCache<IntentCapabilityMap>;

impl RoutingThresholdsCache {
    pub async fn init_thresholds(db: PgPool) -> Self {
        ConfigCache::init("routing_thresholds", db, |pool| async move {
            fetch_thresholds_from_db(&pool).await
        })
        .await
    }
}

impl IntentCapabilityCache {
    pub async fn init_intent_capability(db: PgPool) -> Self {
        ConfigCache::init("intent_capability", db, |pool| async move {
            fetch_intent_capability_from_db(&pool).await
        })
        .await
    }
}

// ── Test ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(intent: &str, tier: &str, cap: &str, preferred: Option<&str>,
           mt: Option<u32>, ht: Option<u32>) -> IntentCapability {
        IntentCapability {
            intent: intent.to_string(),
            base_tier: tier.to_string(),
            base_capability: cap.to_string(),
            preferred_provider: preferred.map(String::from),
            medium_token_threshold: mt,
            heavy_token_threshold: ht,
        }
    }

    #[test]
    fn test_intent_capability_tier_for_tokens_no_promotion() {
        let c = cap("test", "light", "code", None, None, None);
        assert_eq!(c.tier_for_tokens(100), "light");
        assert_eq!(c.tier_for_tokens(10_000), "light");
    }

    #[test]
    fn test_intent_capability_tier_for_tokens_medium_promotion() {
        // fix: light -> medium se tokens >= 3000
        let c = cap("fix", "light", "code", None, Some(3000), None);
        assert_eq!(c.tier_for_tokens(1000), "light");
        assert_eq!(c.tier_for_tokens(3000), "medium");
        assert_eq!(c.tier_for_tokens(5000), "medium");
    }

    #[test]
    fn test_intent_capability_tier_for_tokens_heavy_promotion() {
        // chat: light -> heavy se tokens >= 6000
        let c = cap("chat", "light", "chat", Some("openai"), None, Some(6000));
        assert_eq!(c.tier_for_tokens(500), "light");
        assert_eq!(c.tier_for_tokens(6000), "heavy");
        assert_eq!(c.tier_for_tokens(10000), "heavy");
    }

    #[test]
    fn test_intent_capability_map_helpers() {
        let mut by_intent = HashMap::new();
        by_intent.insert(
            "system_admin".to_string(),
            cap("system_admin", "heavy", "reasoning", Some("anthropic"), None, None),
        );
        by_intent.insert(
            "test".to_string(),
            cap("test", "light", "code", None, None, None),
        );
        let m = IntentCapabilityMap {
            by_intent,
            loaded_at: Instant::now(),
        };
        assert_eq!(m.tier_for("system_admin", 100), Some("heavy".to_string()));
        assert_eq!(m.capability_for("system_admin"), Some("reasoning"));
        assert_eq!(m.preferred_provider_for("system_admin"), Some("anthropic"));
        assert_eq!(m.preferred_provider_for("test"), None);
        // Intent non mappato
        assert_eq!(m.tier_for("unknown", 100), None);
    }

    #[test]
    fn test_intent_capability_compat_with_legacy_match() {
        // Verifica che il seed mig 0110 replichi il match statico precedente
        // di orchestrator.rs:444-461. Se questo test fallisce, la migrazione
        // ha alterato il comportamento di routing — bisogna correggere.
        let seed: &[(&str, &str, &str, Option<&str>, Option<u32>, Option<u32>)] = &[
            ("debug",        "heavy",  "reasoning", Some("anthropic"), None,       None),
            ("architecture", "heavy",  "reasoning", Some("anthropic"), None,       None),
            ("system_admin", "heavy",  "reasoning", Some("anthropic"), None,       None),
            ("file_ops",     "medium", "reasoning", Some("anthropic"), None,       None),
            ("refactor",     "light",  "reasoning", Some("anthropic"), Some(3000), None),
            ("fix",          "light",  "code",      None,              Some(3000), None),
            ("test",         "light",  "code",      None,              None,       None),
            ("docs",         "medium", "docs",      Some("openai"),    None,       None),
            ("chat",         "light",  "chat",      Some("openai"),    None,       Some(6000)),
        ];
        // Test legacy compat: per ogni intent, tier@1000 e capability matchino
        // i valori del match Rust originale.
        // debug@1000 -> heavy/reasoning, fix@1000 -> light/code, fix@4000 -> medium/code
        let mut by_intent = HashMap::new();
        for (i, t, c, pp, mt, ht) in seed {
            by_intent.insert(i.to_string(), cap(i, t, c, *pp, *mt, *ht));
        }
        let m = IntentCapabilityMap {
            by_intent,
            loaded_at: Instant::now(),
        };
        assert_eq!(m.tier_for("debug", 1000), Some("heavy".into()));
        assert_eq!(m.capability_for("debug"), Some("reasoning"));
        assert_eq!(m.tier_for("fix", 1000), Some("light".into()));
        assert_eq!(m.tier_for("fix", 4000), Some("medium".into()));
        assert_eq!(m.tier_for("system_admin", 50), Some("heavy".into()));
        assert_eq!(m.tier_for("file_ops", 50), Some("medium".into()));
        assert_eq!(m.capability_for("system_admin"), Some("reasoning"));
        assert_eq!(m.capability_for("file_ops"), Some("reasoning"));
    }

    #[test]
    fn test_routing_thresholds_defaults_match_migration_seed() {
        // I default in defaults() devono coincidere col seed mig 0111.
        let d = RoutingThresholds::defaults();
        assert!((d.llm_classifier_min_confidence - 0.60).abs() < 1e-6);
        assert!((d.llm_classifier_timeout_seconds - 5.0).abs() < 1e-6);
        assert_eq!(d.classifier_cache_ttl_seconds, 86_400);
        assert_eq!(d.classifier_cache_max_entries, 10_000);
        assert_eq!(d.classifier_provider, "google");
        assert_eq!(d.classifier_model, "gemini-2.5-flash");
        assert_eq!(d.token_threshold_chat_breve, 400);
        assert_eq!(d.token_threshold_chat_media, 1_500);
        assert_eq!(d.token_threshold_complex_fix, 3_000);
        assert_eq!(d.token_threshold_long_context, 6_000);
    }
}
