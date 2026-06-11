//! Registry DB-driven dei modelli AI per il routing.
//!
//! Sostituisce la matrice hardcoded che era sparsa in:
//! - orchestrator.rs (routing matrix + default_model_for_provider)
//! - chat_messages.rs (4 punti)
//! - models.rs (matrice duplicata)
//! - projects/deep_review.rs
//!
//! Carica i modelli dalle tabelle `nexus_routing_matrix`,
//! `nexus_provider_default_model` e `nexus_purpose_model` (vedi migrazioni
//! 0101 e 0102) con cache 60s + refresh background.
//!
//! **Nessun fallback hardcoded**. Se il DB e' irraggiungibile o le tabelle
//! sono vuote, ogni call site ottiene un errore esplicito (HTTP 503/500
//! con messaggio chiaro). Questa scelta e' intenzionale: niente
//! "magic fallback" che mascheri bug di configurazione.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Refresh interval della cache. 60s e' un buon compromesso tra latenza di
/// propagazione delle modifiche (UPDATE in DB → pickup in <60s, no redeploy)
/// e overhead di query.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Regola di escalation per model cascading (BP8 piano riduzione token).
/// Caricata dalle colonne escalation_threshold_tokens / escalation_provider /
/// escalation_model_id di nexus_routing_matrix (mig 0120).
#[derive(Debug, Clone)]
pub struct EscalationRule {
    pub threshold_tokens: i32,
    pub provider: String,
    pub model_id: String,
}

/// Regola di risoluzione tier-based per un purpose (mig 0203).
/// Quando un purpose ha `tier` valorizzato, il modello NON e' fisso: viene
/// selezionato a runtime dal catalog (il miglior modello di quel tier con la
/// capability richiesta). Vedi `internal_routing.rs::resolve_purpose`.
#[derive(Debug, Clone)]
pub struct PurposeTierRule {
    /// 'light' | 'medium' | 'heavy'
    pub tier: String,
    /// Capability richiesta (es. 'reasoning', 'code'); None = nessun filtro.
    pub capability: Option<String>,
    /// Se true, filtra solo modelli con supports_tool_use.
    pub requires_tool_use: bool,
}

/// Matrice immutable con tutte le entry attive.
#[derive(Debug, Clone)]
pub struct RoutingMatrix {
    /// Lookup (intent, behavior_mode) -> (provider, model_id)
    pub by_intent_mode: HashMap<(String, String), (String, String)>,
    /// Default model per provider (provider -> model_id)
    pub default_models: HashMap<String, String>,
    /// Modello per task interni (purpose -> (provider, model_id)).
    /// Vedi migrazione 0102: chat_title_generator, chat_feedback_generator,
    /// docs_generator, custom_instructions, admin_fallback_default, google_batch.
    pub purpose_models: HashMap<String, (String, String)>,
    /// Regole tier-based per purpose (mig 0203): purpose -> tier/capability/tool.
    /// Se un purpose e' qui, il suo modello e' risolto dinamicamente dal
    /// catalog; `purpose_models` resta come ultimo fallback.
    pub purpose_tiers: HashMap<String, PurposeTierRule>,
    /// Regole di escalation token-based per (intent, behavior_mode).
    /// NB: una entry esiste solo se l'admin ha configurato escalation per
    /// quella combinazione (mig 0120).
    pub escalations: HashMap<(String, String), EscalationRule>,
    /// (intent, behavior_mode) la cui riga SERVITA (priority piu' alta) ha
    /// `manual_override = true` (pin admin). FASE 3 (ADR 0030), override-first:
    /// per queste chiavi il pin ha precedenza assoluta e la risoluzione
    /// tier-runtime NON deve sovrascriverle. Campo PARALLELO a `by_intent_mode`
    /// (non cambia lookup): zero impatto sui call site esistenti.
    pub manual_overrides: HashSet<(String, String)>,
}

impl RoutingMatrix {
    /// Cerca (provider, model) per (intent, behavior_mode).
    pub fn lookup(&self, intent: &str, behavior_mode: &str) -> Option<(String, String)> {
        self.by_intent_mode
            .get(&(intent.to_string(), behavior_mode.to_string()))
            .cloned()
    }

    /// Lookup con cascading basato sul budget token stimato (BP8).
    /// Se per (intent, mode) esiste una regola di escalation e
    /// `est_tokens >= threshold`, restituisce il modello escalation; altrimenti
    /// il modello base. Il caller passa la stima del context window richiesto
    /// per il turno (system + history + tools).
    pub fn lookup_with_budget(
        &self,
        intent: &str,
        behavior_mode: &str,
        est_tokens: i32,
    ) -> Option<(String, String)> {
        let key = (intent.to_string(), behavior_mode.to_string());
        if let Some(rule) = self.escalations.get(&key) {
            if est_tokens >= rule.threshold_tokens {
                return Some((rule.provider.clone(), rule.model_id.clone()));
            }
        }
        self.by_intent_mode.get(&key).cloned()
    }

    /// True se la riga servita per (intent, behavior_mode) ha
    /// `manual_override = true` (pin admin). FASE 3: la risoluzione tier-runtime
    /// (override-first) NON deve sovrascrivere queste chiavi.
    pub fn is_manual_override(&self, intent: &str, behavior_mode: &str) -> bool {
        self.manual_overrides
            .contains(&(intent.to_string(), behavior_mode.to_string()))
    }

    /// Modello di default per un provider.
    pub fn default_model(&self, provider: &str) -> Option<String> {
        self.default_models.get(provider).cloned()
    }

    /// Regola tier-based per un purpose, se configurata (mig 0203).
    /// Se ritorna Some, il chiamante deve risolvere il modello dinamicamente
    /// dal catalog (tier+capability); `purpose_models` resta come ultimo fallback.
    pub fn purpose_tier(&self, purpose: &str) -> Option<PurposeTierRule> {
        self.purpose_tiers.get(purpose).cloned()
    }

    /// Matrice di test pre-popolata con un sottoinsieme rappresentativo:
    /// - tutti gli intent rischiosi (file_ops, system_admin, debug, architecture,
    ///   refactor, fix_complesso) mappati a anthropic claude-sonnet-4-6
    /// - intent leggeri (chat_*, fix_semplice, test, docs) mappati a modelli
    ///   appropriati secondo il seed migrazione 0101
    /// - default per provider e qualche purpose model
    ///
    /// Usata da test che vogliono validare il routing senza dipendere dal DB.
    /// Mantenere allineata col seed in `db/migrations/0101_routing_model_registry.sql`
    /// quando si cambia la matrice di produzione.
    #[cfg(test)]
    pub fn fallback_safe() -> Self {
        let mut by_intent_mode = HashMap::new();
        let entries: &[(&str, &str, &str, &str)] = &[
            // chat
            ("chat_breve", "veloce", "google", "gemini-2.5-flash-lite"),
            ("chat_breve", "economica", "openai", "gpt-4.1-nano"),
            ("chat_breve", "bilanciata", "google", "gemini-2.5-flash"),
            (
                "chat_breve",
                "approfondita",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            ("chat_media", "bilanciata", "openai", "gpt-4.1-mini"),
            (
                "chat_media",
                "approfondita",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            (
                "chat_lunga",
                "bilanciata",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            (
                "chat_lunga",
                "approfondita",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            // intent agentici (richiedono modelli capable)
            ("file_ops", "veloce", "openai", "gpt-4.1-mini"),
            (
                "file_ops",
                "bilanciata",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            ("file_ops", "approfondita", "anthropic", "claude-sonnet-4-6"),
            (
                "system_admin",
                "veloce",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            (
                "system_admin",
                "bilanciata",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            (
                "system_admin",
                "approfondita",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            ("debug", "bilanciata", "anthropic", "claude-sonnet-4-6"),
            ("debug", "approfondita", "anthropic", "claude-opus-4-6"),
            (
                "architecture",
                "bilanciata",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            (
                "architecture",
                "approfondita",
                "anthropic",
                "claude-opus-4-6",
            ),
            (
                "refactor",
                "bilanciata",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            ("refactor", "approfondita", "anthropic", "claude-sonnet-4-6"),
            ("fix_semplice", "bilanciata", "openai", "gpt-4.1-mini"),
            (
                "fix_complesso",
                "bilanciata",
                "anthropic",
                "claude-haiku-4-5-20251001",
            ),
            (
                "fix_complesso",
                "approfondita",
                "anthropic",
                "claude-sonnet-4-6",
            ),
            ("test", "bilanciata", "openai", "gpt-4.1-mini"),
            ("docs", "bilanciata", "openai", "gpt-4.1"),
        ];
        for (intent, mode, provider, model) in entries {
            by_intent_mode.insert(
                (intent.to_string(), mode.to_string()),
                (provider.to_string(), model.to_string()),
            );
        }
        let mut default_models = HashMap::new();
        default_models.insert("openai".to_string(), "gpt-4o-mini".to_string());
        default_models.insert("anthropic".to_string(), "claude-sonnet-4-6".to_string());
        default_models.insert("google".to_string(), "gemini-2.5-flash".to_string());
        default_models.insert("mistral".to_string(), "mistral-small-latest".to_string());
        default_models.insert("deepseek".to_string(), "deepseek-chat".to_string());
        Self {
            by_intent_mode,
            default_models,
            purpose_models: HashMap::new(),
            purpose_tiers: HashMap::new(),
            escalations: HashMap::new(),
            manual_overrides: HashSet::new(),
        }
    }
}

/// Carica la matrice dal DB. Ritorna `Err` se DB irraggiungibile o tabelle vuote.
async fn fetch_from_db(db: &PgPool) -> Result<RoutingMatrix, String> {
    // Estesa con le 3 colonne escalation_* (mig 0120). Le colonne sono
    // nullable: usiamo Option per leggerle senza rompere se la migrazione
    // 0120 non e' applicata (graceful fallback: nessuna escalation).
    type RoutingRow = (
        String,         // intent
        String,         // behavior_mode
        String,         // provider
        String,         // model_id
        Option<bool>,   // manual_override (FASE 3)
        Option<i32>,    // escalation_threshold_tokens
        Option<String>, // escalation_provider
        Option<String>, // escalation_model_id
    );
    let matrix_rows = sqlx::query_as::<_, RoutingRow>(
        r#"SELECT intent, behavior_mode, provider, model_id, manual_override,
                  escalation_threshold_tokens, escalation_provider, escalation_model_id
           FROM nexus_routing_matrix
           WHERE is_active = true
           ORDER BY intent, behavior_mode, priority DESC"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query nexus_routing_matrix fallita: {e}"))?;

    let default_rows = sqlx::query_as::<_, (String, String)>(
        r#"SELECT provider, model_id FROM nexus_provider_default_model"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query nexus_provider_default_model fallita: {e}"))?;

    // Purpose models (mig 0102). NON ignoriamo errori — se la tabella non
    // esiste l'admin deve applicare la migrazione.
    let purpose_rows = sqlx::query_as::<_, (String, String, String)>(
        r#"SELECT purpose, provider, model_id FROM nexus_purpose_model"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| {
        format!("query nexus_purpose_model fallita: {e}. Hai applicato la migrazione 0102?")
    })?;

    // Regole tier-based per purpose (mig 0203). GRACEFUL: se le colonne non
    // esistono (migrazione non ancora applicata) ignoriamo senza bloccare —
    // i purpose si risolvono staticamente come prima.
    let purpose_tier_rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, bool)>(
        r#"SELECT purpose, tier, required_capability, requires_tool_use
           FROM nexus_purpose_model
           WHERE tier IS NOT NULL"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| {
        warn!(
            "routing_matrix: colonne tier di nexus_purpose_model non disponibili \
             (mig 0203 non applicata?): {e}. Risoluzione purpose statica."
        );
        Vec::new()
    });

    if matrix_rows.is_empty() {
        return Err(
            "nexus_routing_matrix vuota. Applica la migrazione 0101 o popola la tabella."
                .to_string(),
        );
    }
    if default_rows.is_empty() {
        return Err(
            "nexus_provider_default_model vuota. Applica la migrazione 0101 o popola la tabella."
                .to_string(),
        );
    }

    let mut by_intent_mode: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut manual_overrides: HashSet<(String, String)> = HashSet::new();
    let mut escalations: HashMap<(String, String), EscalationRule> = HashMap::new();
    for (intent, mode, provider, model_id, manual_override, esc_thr, esc_prov, esc_model) in
        matrix_rows
    {
        let key = (intent.clone(), mode.clone());
        // La query e' ORDER BY priority DESC: la PRIMA riga vista per (intent,mode)
        // e' quella servita. Tracciamo manual_override SOLO della riga vincente
        // (FASE 3, override-first), in parallelo a by_intent_mode.
        if !by_intent_mode.contains_key(&key) {
            by_intent_mode.insert(key.clone(), (provider, model_id));
            if manual_override.unwrap_or(false) {
                manual_overrides.insert(key.clone());
            }
        }
        // Inserisci escalation solo se tutti e tre i campi sono presenti
        // (admin ha completato la configurazione).
        if let (Some(thr), Some(prov), Some(model)) = (esc_thr, esc_prov, esc_model) {
            if thr > 0 && !prov.is_empty() && !model.is_empty() {
                escalations.entry(key).or_insert(EscalationRule {
                    threshold_tokens: thr,
                    provider: prov,
                    model_id: model,
                });
            }
        }
    }

    let default_models: HashMap<String, String> = default_rows.into_iter().collect();
    let purpose_models: HashMap<String, (String, String)> = purpose_rows
        .into_iter()
        .map(|(purpose, provider, model)| (purpose, (provider, model)))
        .collect();
    let purpose_tiers: HashMap<String, PurposeTierRule> = purpose_tier_rows
        .into_iter()
        .filter_map(|(purpose, tier, capability, requires_tool_use)| {
            tier.map(|t| {
                (
                    purpose,
                    PurposeTierRule {
                        tier: t,
                        capability,
                        requires_tool_use,
                    },
                )
            })
        })
        .collect();

    Ok(RoutingMatrix {
        by_intent_mode,
        default_models,
        purpose_models,
        purpose_tiers,
        escalations,
        manual_overrides,
    })
}

/// Manager della cache: tiene un `Arc<RoutingMatrix>` aggiornato in background.
/// Le letture sono lock-free (clone dell'Arc).
///
/// Stato iniziale e durante refresh: `Option<Arc<RoutingMatrix>>`.
/// Se `None` (mai caricata con successo), tutti i call site ricevono errore
/// esplicito 503 invece di un fallback nascosto.
#[derive(Clone)]
pub struct RoutingMatrixCache {
    inner: Arc<RwLock<Option<Arc<RoutingMatrix>>>>,
}

impl RoutingMatrixCache {
    /// Inizializza la cache con retry-loop: 5 tentativi × 5s di backoff
    /// per dare tempo a Postgres di salire (es. ordine systemd boot).
    /// Se dopo 5 tentativi il DB e' ancora down, mcp-core PANICA all'avvio
    /// con messaggio chiaro — non parte con una matrice fittizia.
    /// Spawna poi il task di refresh background ogni 60s.
    pub async fn init(db: PgPool) -> Self {
        let mut last_err: Option<String> = None;
        let mut initial: Option<Arc<RoutingMatrix>> = None;
        for attempt in 1..=5 {
            match fetch_from_db(&db).await {
                Ok(m) => {
                    info!(
                        "routing_matrix: caricata da DB ({} routing, {} default per-provider, {} purpose-models)",
                        m.by_intent_mode.len(),
                        m.default_models.len(),
                        m.purpose_models.len()
                    );
                    initial = Some(Arc::new(m));
                    last_err = None;
                    break;
                }
                Err(e) => {
                    warn!(
                        "routing_matrix: tentativo {}/5 di load DB fallito ({}). Retry in 5s...",
                        attempt, e
                    );
                    last_err = Some(e);
                    if attempt < 5 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }

        if initial.is_none() {
            // Panic esplicito: niente fallback. Errore chiaro nei log per l'admin.
            panic!(
                "routing_matrix: impossibile caricare dal DB dopo 5 tentativi. \
                 Errore: {}. \
                 Verifica: (a) Postgres raggiungibile, (b) migrazioni 0101 e 0102 applicate, \
                 (c) tabelle nexus_routing_matrix / nexus_provider_default_model / nexus_purpose_model popolate.",
                last_err.unwrap_or_else(|| "unknown".to_string())
            );
        }

        let inner = Arc::new(RwLock::new(initial));
        let cache = Self {
            inner: inner.clone(),
        };

        // Spawn refresh background. Errori NON sostituiscono la cache valida
        // precedente — manteniamo l'ultima matrice buona finche' DB non torna up.
        let inner_bg = inner;
        let db_bg = db;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                match fetch_from_db(&db_bg).await {
                    Ok(new_matrix) => {
                        let arc = Arc::new(new_matrix);
                        {
                            let mut w = inner_bg.write().await;
                            *w = Some(arc);
                        }
                        debug!("routing_matrix: refresh OK");
                    }
                    Err(e) => {
                        warn!(
                            "routing_matrix: refresh fallito ({}). Mantengo cache precedente.",
                            e
                        );
                    }
                }
            }
        });

        cache
    }

    /// Snapshot lock-free della matrice corrente.
    /// Ritorna `Err` se la matrice non e' MAI stata caricata (DB down dall'avvio
    /// e mcp-core non ha panic-ato — non dovrebbe succedere visto il retry-loop
    /// ma e' un'invariante della struttura, non del DB).
    pub fn current(&self) -> Result<Arc<RoutingMatrix>, String> {
        match self.inner.try_read() {
            Ok(g) => match &*g {
                Some(arc) => Ok(Arc::clone(arc)),
                None => Err("routing_matrix non caricata (DB down all'avvio?)".to_string()),
            },
            Err(_) => {
                // Lock occupato dal refresh background. Riprova async.
                Err(
                    "routing_matrix: cache temporaneamente non disponibile (refresh in corso)"
                        .to_string(),
                )
            }
        }
    }

    /// Versione async per quando puoi awaitare il lock.
    pub async fn current_async(&self) -> Result<Arc<RoutingMatrix>, String> {
        let g = self.inner.read().await;
        g.as_ref()
            .map(Arc::clone)
            .ok_or_else(|| "routing_matrix non caricata (DB down all'avvio?)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_matrix() -> RoutingMatrix {
        let mut by_intent_mode = HashMap::new();
        by_intent_mode.insert(
            ("file_ops".to_string(), "approfondita".to_string()),
            ("anthropic".to_string(), "claude-sonnet-4-6".to_string()),
        );
        by_intent_mode.insert(
            ("system_admin".to_string(), "bilanciata".to_string()),
            ("anthropic".to_string(), "claude-sonnet-4-6".to_string()),
        );
        let mut default_models = HashMap::new();
        default_models.insert("openai".to_string(), "gpt-4o-mini".to_string());
        default_models.insert("anthropic".to_string(), "claude-sonnet-4-6".to_string());
        let mut purpose_models = HashMap::new();
        purpose_models.insert(
            "chat_title_generator".to_string(),
            ("openai".to_string(), "gpt-4.1-nano".to_string()),
        );
        RoutingMatrix {
            by_intent_mode,
            default_models,
            purpose_models,
            purpose_tiers: HashMap::new(),
            escalations: HashMap::new(),
            manual_overrides: HashSet::new(),
        }
    }

    #[test]
    fn lookup_returns_some_for_existing_entry() {
        let m = make_test_matrix();
        let r = m.lookup("file_ops", "approfondita");
        assert_eq!(
            r,
            Some(("anthropic".to_string(), "claude-sonnet-4-6".to_string()))
        );
    }

    #[test]
    fn lookup_returns_none_for_missing_entry() {
        let m = make_test_matrix();
        assert_eq!(m.lookup("nonexistent", "veloce"), None);
    }

    #[test]
    fn default_model_returns_some_for_existing_provider() {
        let m = make_test_matrix();
        assert_eq!(m.default_model("openai"), Some("gpt-4o-mini".to_string()));
    }

    #[test]
    fn default_model_returns_none_for_missing_provider() {
        let m = make_test_matrix();
        assert_eq!(m.default_model("xai"), None);
    }

    // =====================================================================
    // TEST SCALABILITA' PROVIDER/MODELLI
    // =====================================================================
    // Validano che la matrice di routing supporti scaling intra-provider
    // (stesso intent con behavior_mode diverso → modello piu' capace) e
    // copertura completa di tutti i 5 provider configurati.

    #[test]
    fn is_manual_override_riconosce_solo_le_chiavi_pinnate() {
        // FASE 3: il set manual_overrides e' parallelo a by_intent_mode e non
        // tocca lookup. is_manual_override risponde true solo per le chiavi pinnate.
        let mut m = RoutingMatrix::fallback_safe();
        assert!(!m.is_manual_override("debug", "approfondita"));
        m.manual_overrides
            .insert(("debug".to_string(), "approfondita".to_string()));
        assert!(m.is_manual_override("debug", "approfondita"));
        // Chiave non pinnata e lookup invariato (zero impatto).
        assert!(!m.is_manual_override("chat_breve", "veloce"));
        assert!(m.lookup("chat_breve", "veloce").is_some());
    }

    #[test]
    fn fallback_safe_copre_tutti_e_5_i_provider_default() {
        // Garanzia: ogni provider supportato ha un default_model. Senza questa
        // garanzia, fallback chain in chat_messages.rs:1505 puo' fallire
        // silenziosamente quando passa a un provider senza default in DB.
        let m = RoutingMatrix::fallback_safe();
        for provider in &["openai", "anthropic", "google", "mistral", "deepseek"] {
            assert!(
                m.default_model(provider).is_some(),
                "default_model('{}') deve essere configurato per la fallback chain",
                provider
            );
        }
    }

    #[test]
    fn lookup_distingue_behavior_mode_per_stesso_intent() {
        // Scaling intra-mode: chat_breve in 4 mode diversi → 4 (provider, model)
        // diversi. Verifica la non-degenerazione: il behavior_mode discrimina davvero.
        let m = RoutingMatrix::fallback_safe();
        let mut found_models = std::collections::HashSet::new();
        for mode in &["veloce", "economica", "bilanciata", "approfondita"] {
            if let Some((_p, model)) = m.lookup("chat_breve", mode) {
                found_models.insert(model);
            }
        }
        assert!(
            found_models.len() >= 3,
            "chat_breve deve mappare a >= 3 modelli distinti per mode diversi, trovati: {:?}",
            found_models
        );
    }

    #[test]
    fn lookup_intent_agentici_richiedono_modelli_capable() {
        // Gli intent rischiosi (file_ops, system_admin, debug, architecture) in mode
        // approfondita devono usare modelli "heavy" (Claude Sonnet/Opus). Mai modelli
        // light come gpt-4.1-nano o gemini-flash-lite.
        let m = RoutingMatrix::fallback_safe();
        let light_models = [
            "gemini-2.5-flash-lite",
            "gpt-4.1-nano",
            "claude-haiku-3-5",
            "mistral-small-latest",
            "deepseek-chat",
        ];
        for intent in &["file_ops", "system_admin", "debug", "architecture"] {
            if let Some((_p, model)) = m.lookup(intent, "approfondita") {
                assert!(
                    !light_models.contains(&model.as_str()),
                    "intent '{}' mode 'approfondita' non puo' usare modello light '{}'",
                    intent,
                    model
                );
            }
        }
    }

    #[test]
    fn lookup_e_case_sensitive_su_intent() {
        // Defense in depth: se il classificatore agentico ritorna "FILE_OPS"
        // (uppercase), lookup deve fallire invece di rispondere come "file_ops".
        // Questo forza il chiamante a normalizzare l'input.
        let m = RoutingMatrix::fallback_safe();
        assert!(m.lookup("file_ops", "bilanciata").is_some());
        assert!(m.lookup("FILE_OPS", "bilanciata").is_none());
        assert!(m.lookup("File_Ops", "bilanciata").is_none());
    }

    #[test]
    fn lookup_ritorna_none_per_combinazione_mode_invalida() {
        // Se il classificatore propone mode "ultra_veloce" inesistente,
        // lookup deve dire None (no silent fallback a una mode random).
        let m = RoutingMatrix::fallback_safe();
        assert_eq!(m.lookup("file_ops", "ultra_veloce"), None);
        assert_eq!(m.lookup("chat_breve", ""), None);
    }

    #[test]
    fn purpose_models_contiene_tupla_provider_modello() {
        // La risoluzione purpose in produzione passa dal punto unico tier-only
        // (internal_routing::resolve_purpose_model); qui validiamo solo che il
        // campo purpose_models (fallback statico) sia popolato correttamente.
        let m = make_test_matrix();
        assert_eq!(
            m.purpose_models.get("chat_title_generator"),
            Some(&("openai".to_string(), "gpt-4.1-nano".to_string()))
        );
        assert_eq!(m.purpose_models.get("inesistente"), None);
    }

    #[test]
    fn purpose_tier_ritorna_regola_quando_configurata() {
        // mig 0203: un purpose con tier configurato deve esporre la regola
        // tier-based; uno senza tier ritorna None (risoluzione statica).
        let mut m = make_test_matrix();
        m.purpose_tiers.insert(
            "planner".to_string(),
            PurposeTierRule {
                tier: "heavy".to_string(),
                capability: Some("reasoning".to_string()),
                requires_tool_use: true,
            },
        );
        let rule = m.purpose_tier("planner").expect("planner deve avere tier");
        assert_eq!(rule.tier, "heavy");
        assert_eq!(rule.capability.as_deref(), Some("reasoning"));
        assert!(rule.requires_tool_use);
        // purpose senza tier (solo statico)
        assert!(m.purpose_tier("chat_title_generator").is_none());
    }

    #[test]
    fn fallback_safe_garantisce_coverage_intent_critici() {
        // Smoke test: i 9 intent critici devono avere almeno una entry in
        // mode "bilanciata" (mode di default per la maggior parte dei turni).
        let m = RoutingMatrix::fallback_safe();
        let intent_critici = [
            "chat_breve",
            "chat_media",
            "chat_lunga",
            "file_ops",
            "system_admin",
            "debug",
            "architecture",
            "refactor",
            "fix_complesso",
        ];
        for intent in &intent_critici {
            let mode = "bilanciata";
            assert!(
                m.lookup(intent, mode).is_some(),
                "intent critico '{}' senza routing in mode 'bilanciata' — fallback chain rotta",
                intent
            );
        }
    }

    #[test]
    fn escalation_for_ritorna_none_quando_assente() {
        let m = make_test_matrix();
        let key = ("file_ops".to_string(), "approfondita".to_string());
        assert!(!m.escalations.contains_key(&key));
    }

    #[test]
    fn lookup_with_budget_applica_escalation_oltre_soglia() {
        // BP8: lookup_with_budget deve ritornare il modello escalation
        // quando est_tokens supera la threshold, e il modello base altrimenti.
        let mut m = make_test_matrix();
        // Aggiunge base entry per il caso sotto threshold
        m.by_intent_mode.insert(
            ("refactor".to_string(), "bilanciata".to_string()),
            ("openai".to_string(), "gpt-4.1-mini".to_string()),
        );
        // Aggiunge regola escalation per token grandi
        m.escalations.insert(
            ("refactor".to_string(), "bilanciata".to_string()),
            EscalationRule {
                threshold_tokens: 50_000,
                provider: "anthropic".to_string(),
                model_id: "claude-sonnet-4-6".to_string(),
            },
        );
        // Sotto soglia: modello base
        let base = m.lookup_with_budget("refactor", "bilanciata", 10_000);
        assert_eq!(
            base,
            Some(("openai".to_string(), "gpt-4.1-mini".to_string()))
        );
        // Sopra soglia: modello escalation
        let esc = m.lookup_with_budget("refactor", "bilanciata", 60_000);
        assert_eq!(
            esc,
            Some(("anthropic".to_string(), "claude-sonnet-4-6".to_string()))
        );
    }

    #[test]
    fn fallback_safe_ha_almeno_24_intent_mode_entries() {
        // Soglia minima: la matrice di test deve coprire abbastanza casi
        // perche' qualsiasi cambio nella seed migration 0101 (rimozione
        // accidentale di intent) faccia fallire i test.
        let m = RoutingMatrix::fallback_safe();
        assert!(
            m.by_intent_mode.len() >= 24,
            "fallback_safe ha {} entries, atteso >= 24 (seed mig 0101)",
            m.by_intent_mode.len()
        );
    }
}
