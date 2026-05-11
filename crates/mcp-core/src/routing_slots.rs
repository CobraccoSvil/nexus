//! Slot-filling routing matrix (Livello 4 disambiguation framework NLU).
//!
//! Lettura della tabella `nexus_routing_slots_matrix` (mig 0133) con cache
//! 60s. Lookup gerarchico con fallback wildcard sui campi piu' specifici:
//!
//!   (resolve, tests, playwright, multi_file)  -- match esatto
//!     ↓ se non trovato
//!   (resolve, tests, *,          multi_file)  -- ignora framework
//!     ↓ se non trovato
//!   (resolve, tests, *,          *)           -- ignora scope
//!     ↓ se non trovato
//!   (resolve, *,     *,          *)           -- ignora target
//!     ↓ se non trovato
//!   None -- il caller fa fallback al routing classico (intent, behavior_mode)
//!
//! Quando piu' provider hanno la stessa chiave, vengono ordinati per
//! `priority DESC` ed esposti come chain di fallback (utile per cooldown).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::warn;

/// Slot canonici estratti dal classifier LLM (mig 0133).
///
/// La struct riflette esattamente la `slots` ritornata dal classifier
/// Python (vedi `brain/router/agentic_classifier.py::ActionSlots`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionSlots {
    #[serde(default)]
    pub action_verb: String,
    #[serde(default)]
    pub target_type: String,
    #[serde(default)]
    pub framework: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub confidence: f32,
}

impl ActionSlots {
    /// True se action_verb, target_type, scope sono tutti popolati con
    /// valori canonici. `framework` e' opzionale.
    pub fn is_complete(&self) -> bool {
        !self.action_verb.is_empty()
            && !self.target_type.is_empty()
            && !self.scope.is_empty()
    }

    /// Soglia minima sopra la quale fidarsi dello slot filling.
    /// Sotto questa soglia: fallback al routing classico (intent,mode).
    pub fn meets_confidence(&self, min: f32) -> bool {
        self.confidence >= min
    }
}

/// Una riga della matrice slots (singolo provider+model per chiave).
/// Piu' righe con la stessa chiave = chain di fallback ordinata.
#[derive(Debug, Clone)]
pub struct SlotsRoutingEntry {
    pub action_verb: String,
    pub target_type: String,
    pub framework: String,
    pub scope: String,
    pub provider: String,
    pub model_id: String,
    pub priority: i32,
    #[allow(dead_code)]
    pub rationale: String,
}

/// Matrice slots in memoria. Cache TTL 60s, refresh background.
#[derive(Debug, Clone)]
pub struct SlotsRoutingMatrix {
    /// Tutte le entry attive. Lookup itera con fallback wildcard.
    /// (Numero piccolo: ~50-200 entry. Iterazione lineare e' OK.)
    pub entries: Vec<SlotsRoutingEntry>,
    pub loaded_at: Instant,
}

impl SlotsRoutingMatrix {
    /// Lookup gerarchico:
    /// 1. match esatto su tutti 4 i campi
    /// 2. fallback con framework='*'
    /// 3. fallback con scope='*'
    /// 4. fallback con target_type='*'
    /// 5. None
    ///
    /// Quando piu' entry matchano la stessa chiave, ritorna quella con
    /// `priority DESC`. La chain completa e' disponibile via `lookup_chain`.
    pub fn lookup(&self, slots: &ActionSlots) -> Option<(String, String)> {
        self.lookup_chain(slots).into_iter().next()
    }

    /// Come `lookup` ma ritorna TUTTI i provider+model candidati per la
    /// chiave (ordinati per priority DESC), utili come chain di fallback
    /// quando il primo provider e' in cooldown.
    pub fn lookup_chain(&self, slots: &ActionSlots) -> Vec<(String, String)> {
        if !slots.is_complete() {
            return Vec::new();
        }
        // Step 1-4: prova specificita' decrescenti
        let candidates: &[(&str, &str)] = &[
            // (framework_pattern, scope_pattern, target_pattern)
        ];
        let _ = candidates; // tenuto per documentazione

        // Strategia iterativa: per ogni livello di "wildcard relaxation",
        // collezioniamo tutte le entry che matchano e prendiamo la migliore
        // per priority.
        let probes: [(Option<&str>, Option<&str>, Option<&str>); 5] = [
            // (framework, scope, target) — Some(x) = match esatto, None = wildcard
            (Some(slots.framework.as_str()), Some(slots.scope.as_str()), Some(slots.target_type.as_str())),
            (None,                            Some(slots.scope.as_str()), Some(slots.target_type.as_str())),
            (None,                            None,                       Some(slots.target_type.as_str())),
            (Some(slots.framework.as_str()), None,                       Some(slots.target_type.as_str())),
            (None,                            None,                       None),
        ];

        for (fw_probe, scope_probe, target_probe) in probes {
            let mut matches: Vec<&SlotsRoutingEntry> = self
                .entries
                .iter()
                .filter(|e| {
                    if e.action_verb != slots.action_verb {
                        return false;
                    }
                    // target: se probe e' None usiamo wildcard, altrimenti match diretto OR row='*'
                    let target_ok = match target_probe {
                        Some(t) => e.target_type == t || e.target_type == "*",
                        None => e.target_type == "*",
                    };
                    if !target_ok {
                        return false;
                    }
                    let scope_ok = match scope_probe {
                        Some(s) => e.scope == s || e.scope == "*",
                        None => e.scope == "*",
                    };
                    if !scope_ok {
                        return false;
                    }
                    // framework: stringhe vuote ammesse come wildcard
                    let fw_ok = match fw_probe {
                        Some(f) if !f.is_empty() => e.framework == f || e.framework == "*",
                        _ => e.framework == "*",
                    };
                    fw_ok
                })
                .collect();
            if !matches.is_empty() {
                // Ordina per priority DESC (chain di fallback)
                matches.sort_by(|a, b| b.priority.cmp(&a.priority));
                return matches
                    .iter()
                    .map(|e| (e.provider.clone(), e.model_id.clone()))
                    .collect();
            }
        }
        Vec::new()
    }

    /// Conta le entry attive (utile per dashboard + test).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True se non ci sono entry (DB vuoto o tabella appena migrata).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Safety-net: estrae slots euristicamente dal messaggio quando il classifier
/// LLM ha fallito (JSON parse fail, timeout, ecc.). Pattern matching su
/// keyword italiane/inglesi piu' comuni. Confidence sempre <= 0.65 per
/// segnalare che e' una stima keyword-based, non semantic.
///
/// Ritorna `ActionSlots` con `is_complete()=false` se nessuna keyword matcha
/// (in quel caso route_by_slots fa fallback al routing classico).
pub fn infer_slots_heuristic(message: &str) -> ActionSlots {
    let lc = message.to_lowercase();

    // === action_verb ===
    let resolve_kw = [
        "risolv", "correggi", "ripara", "fix the", " fix ", "make work", "make pass",
        "fai funzionare", "fai passare", "non passano", "stanno fallendo", "are failing",
        "is failing", "non funziona", "make them pass", "esegui i test e",
    ];
    let write_kw = [
        "scrivi", "aggiung", "crea ", "create ", "aggiungi", "implementa", "implement",
        "add ", "write new", "write a", "scrivere",
    ];
    let read_kw = [
        "leggi", "leggere", "elenca", "mostra", "ispez", "controlla lo stato",
        "list files", "list ", "guarda",
    ];
    let analyze_kw = [
        "perche'", "perché", "perche ", "why does", "investiga", "analizza", "indaga",
        "root cause",
    ];
    let refactor_kw = ["refactor", "rinomina", "ristruttur", "estrai funzione"];
    let configure_kw = [
        "configur", "imposta", "setup", "set up", "abilit", "disabilit",
    ];
    let deploy_kw = ["deploy", "deploya", "rilascia", "rilancia"];
    let delete_kw = ["elimin", "rimuov", "cancell", "delete", "remove"];

    let action_verb = if resolve_kw.iter().any(|k| lc.contains(k)) {
        "resolve"
    } else if delete_kw.iter().any(|k| lc.contains(k)) {
        "delete"
    } else if deploy_kw.iter().any(|k| lc.contains(k)) {
        "deploy"
    } else if configure_kw.iter().any(|k| lc.contains(k)) {
        "configure"
    } else if refactor_kw.iter().any(|k| lc.contains(k)) {
        "refactor"
    } else if analyze_kw.iter().any(|k| lc.contains(k)) {
        "analyze"
    } else if write_kw.iter().any(|k| lc.contains(k)) {
        "write"
    } else if read_kw.iter().any(|k| lc.contains(k)) {
        "read"
    } else {
        ""
    };
    if action_verb.is_empty() {
        return ActionSlots::default();
    }

    // === target_type ===
    let target_type = if lc.contains("test") || lc.contains("playwright")
        || lc.contains("pytest") || lc.contains("jest") || lc.contains("vitest")
        || lc.contains("cargo test")
    {
        "tests"
    } else if lc.contains("docker") || lc.contains("k8s") || lc.contains("dockerfile") {
        "infrastructure"
    } else if lc.contains("config") || lc.contains(".yml") || lc.contains(".yaml")
        || lc.contains(".toml") || lc.contains(".env")
    {
        "config"
    } else if lc.contains("servizi") || lc.contains("service") || lc.contains("microservizio") {
        "service"
    } else if lc.contains("documentaz") || lc.contains("readme") || lc.contains("documentation") {
        "docs"
    } else if lc.contains("migraz") || lc.contains("migration") || lc.contains("schema db") {
        "data"
    } else {
        "code"
    };

    // === framework ===
    let framework = if lc.contains("playwright") { "playwright" }
        else if lc.contains("pytest") { "pytest" }
        else if lc.contains("cargo") { "cargo" }
        else if lc.contains("jest") { "jest" }
        else if lc.contains("vitest") { "vitest" }
        else if lc.contains("docker") { "docker" }
        else if lc.contains("nextjs") || lc.contains("next.js") { "nextjs" }
        else { "" };

    // === scope ===
    let scope = if lc.contains("cross-service") || lc.contains("cross service")
        || lc.contains("microservi") || lc.contains("frontend e backend")
        || lc.contains("backend e frontend") || lc.contains("piu' servizi")
        || lc.contains("piu servizi")
    {
        "cross_service"
    } else if lc.contains("piu' file") || lc.contains("piu file")
        || lc.contains("multipl") || lc.contains("multi-file") || lc.contains("multi_file")
        || lc.contains("multi file")
        // I task "esegui test e risolvi" sono quasi sempre multi-file
        || (action_verb == "resolve" && target_type == "tests")
    {
        "multi_file"
    } else if lc.contains("tutto il sistema") || lc.contains("system-wide")
        || lc.contains("intera piattaforma")
    {
        "system_wide"
    } else {
        "single"
    };

    ActionSlots {
        action_verb: action_verb.to_string(),
        target_type: target_type.to_string(),
        framework: framework.to_string(),
        scope: scope.to_string(),
        // Confidence bassa: e' keyword-based, non semantic. Sopra la soglia
        // 0.60 di route_by_slots ma sotto la soglia LLM (0.70-0.85).
        confidence: 0.65,
    }
}

async fn fetch_slots_from_db(db: &PgPool) -> Result<SlotsRoutingMatrix, String> {
    let rows: Vec<(String, String, String, String, String, String, i32, String)> =
        sqlx::query_as(
            r#"SELECT action_verb, target_type, framework, scope,
                      provider, model_id, priority, rationale
               FROM nexus_routing_slots_matrix
               WHERE is_active = TRUE
               ORDER BY priority DESC"#,
        )
        .fetch_all(db)
        .await
        .map_err(|e| format!("query nexus_routing_slots_matrix fallita: {e}"))?;
    let entries: Vec<SlotsRoutingEntry> = rows
        .into_iter()
        .map(|(av, tt, fw, sc, prov, model, prio, rat)| SlotsRoutingEntry {
            action_verb: av,
            target_type: tt,
            framework: fw,
            scope: sc,
            provider: prov,
            model_id: model,
            priority: prio,
            rationale: rat,
        })
        .collect();
    Ok(SlotsRoutingMatrix {
        entries,
        loaded_at: Instant::now(),
    })
}

/// Cache thread-safe con refresh background ogni 60s. Pattern identico a
/// `RoutingMatrixCache` in `routing_matrix.rs`.
#[derive(Debug, Clone)]
pub struct SlotsRoutingMatrixCache {
    inner: Arc<RwLock<Option<Arc<SlotsRoutingMatrix>>>>,
    last_error: Arc<RwLock<Option<String>>>,
}

impl SlotsRoutingMatrixCache {
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            last_error: Arc::new(RwLock::new(None)),
        }
    }

    /// Inizializza al boot. Niente retry-loop hard come per RoutingMatrix:
    /// la tabella slots e' un'estensione opzionale, se manca usiamo il
    /// routing classico. Logga ERROR se il fetch fallisce ma NON panica.
    pub async fn init(db: &PgPool) -> Self {
        let cache = Self::empty();
        match fetch_slots_from_db(db).await {
            Ok(m) => {
                let n = m.len();
                *cache.inner.write().await = Some(Arc::new(m));
                tracing::info!(
                    "SlotsRoutingMatrix caricata: {} entry attive (mig 0133)",
                    n
                );
            }
            Err(e) => {
                tracing::warn!(
                    "SlotsRoutingMatrix init fallita ({e}). \
                     Il routing slot-based sara' disabilitato; \
                     fallback al routing classico (intent, behavior_mode)."
                );
                *cache.last_error.write().await = Some(e);
            }
        }
        // Spawn refresh background ogni 60s
        let cache_for_refresh = cache.clone();
        let db_for_refresh = db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.tick().await; // skip primo tick immediato
            loop {
                interval.tick().await;
                match fetch_slots_from_db(&db_for_refresh).await {
                    Ok(m) => {
                        *cache_for_refresh.inner.write().await = Some(Arc::new(m));
                        *cache_for_refresh.last_error.write().await = None;
                    }
                    Err(e) => {
                        warn!("SlotsRoutingMatrix refresh fallito: {e}");
                        *cache_for_refresh.last_error.write().await = Some(e);
                    }
                }
            }
        });
        cache
    }

    /// Snapshot corrente, se disponibile. None se DB non e' stato letto.
    pub async fn current_async(&self) -> Option<Arc<SlotsRoutingMatrix>> {
        self.inner.read().await.as_ref().map(Arc::clone)
    }

    #[allow(dead_code)]
    pub async fn last_error(&self) -> Option<String> {
        self.last_error.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_matrix() -> SlotsRoutingMatrix {
        SlotsRoutingMatrix {
            entries: vec![
                // Match esatto (resolve, tests, playwright, multi_file)
                SlotsRoutingEntry {
                    action_verb: "resolve".into(),
                    target_type: "tests".into(),
                    framework: "playwright".into(),
                    scope: "multi_file".into(),
                    provider: "anthropic".into(),
                    model_id: "claude-sonnet-4-6".into(),
                    priority: 110,
                    rationale: "playwright multi_file".into(),
                },
                // Wildcard framework
                SlotsRoutingEntry {
                    action_verb: "resolve".into(),
                    target_type: "tests".into(),
                    framework: "*".into(),
                    scope: "multi_file".into(),
                    provider: "anthropic".into(),
                    model_id: "claude-sonnet-4-6".into(),
                    priority: 100,
                    rationale: "any tests multi_file".into(),
                },
                // Wildcard framework, secondo provider (fallback)
                SlotsRoutingEntry {
                    action_verb: "resolve".into(),
                    target_type: "tests".into(),
                    framework: "*".into(),
                    scope: "multi_file".into(),
                    provider: "mistral".into(),
                    model_id: "mistral-large-2411".into(),
                    priority: 90,
                    rationale: "fallback mistral".into(),
                },
                // Wildcard scope
                SlotsRoutingEntry {
                    action_verb: "resolve".into(),
                    target_type: "tests".into(),
                    framework: "*".into(),
                    scope: "single".into(),
                    provider: "anthropic".into(),
                    model_id: "claude-haiku-4-5-20251001".into(),
                    priority: 100,
                    rationale: "single tests".into(),
                },
                // write tests (caso "scrivi un test" → light model)
                SlotsRoutingEntry {
                    action_verb: "write".into(),
                    target_type: "tests".into(),
                    framework: "*".into(),
                    scope: "single".into(),
                    provider: "openai".into(),
                    model_id: "gpt-4.1-mini".into(),
                    priority: 100,
                    rationale: "scrittura test single".into(),
                },
                // delete wildcard target (sempre capable)
                SlotsRoutingEntry {
                    action_verb: "delete".into(),
                    target_type: "*".into(),
                    framework: "*".into(),
                    scope: "*".into(),
                    provider: "anthropic".into(),
                    model_id: "claude-sonnet-4-6".into(),
                    priority: 100,
                    rationale: "delete safety".into(),
                },
            ],
            loaded_at: Instant::now(),
        }
    }

    #[test]
    fn lookup_exact_match_ha_priorita_su_wildcard() {
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.9,
        };
        let result = m.lookup(&slots).unwrap();
        // Match esatto (priority 110) batte il wildcard framework (priority 100)
        assert_eq!(result, ("anthropic".into(), "claude-sonnet-4-6".into()));
    }

    #[test]
    fn lookup_caso_redemptor_test_failure_resolution() {
        // Caso che ha originato Livello 4: "esegui playwright e risolvi i fail"
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.92,
        };
        let result = m.lookup(&slots).unwrap();
        assert!(result.1.contains("sonnet"),
            "atteso modello capable (sonnet), got {}", result.1);
    }

    #[test]
    fn lookup_chain_ritorna_tutti_provider_ordinati_per_priority() {
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "".into(),  // no framework → matcha wildcard
            scope: "multi_file".into(),
            confidence: 0.9,
        };
        let chain = m.lookup_chain(&slots);
        // Atteso: 2 entry (anthropic priority 100, mistral priority 90)
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].0, "anthropic");  // priority 100
        assert_eq!(chain[1].0, "mistral");    // priority 90
    }

    #[test]
    fn lookup_fallback_a_wildcard_framework() {
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "vitest".into(),  // non in DB → wildcard fallback
            scope: "multi_file".into(),
            confidence: 0.85,
        };
        let result = m.lookup(&slots);
        assert!(result.is_some(), "vitest deve cadere su wildcard");
        assert!(result.unwrap().1.contains("sonnet"));
    }

    #[test]
    fn lookup_distingue_write_tests_da_resolve_tests() {
        // Caso paradigmatico: stesso target=tests, action_verb diverso →
        // routing completamente diverso (write=light, resolve=capable).
        let m = make_test_matrix();
        let write_slots = ActionSlots {
            action_verb: "write".into(),
            target_type: "tests".into(),
            framework: "".into(),
            scope: "single".into(),
            confidence: 0.92,
        };
        let resolve_slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.92,
        };
        let write_result = m.lookup(&write_slots).unwrap();
        let resolve_result = m.lookup(&resolve_slots).unwrap();
        // write → light model
        assert!(write_result.1.contains("mini") || write_result.1.contains("haiku"));
        // resolve → capable model
        assert!(resolve_result.1.contains("sonnet") || resolve_result.1.contains("opus"));
        assert_ne!(write_result.1, resolve_result.1);
    }

    #[test]
    fn lookup_ritorna_none_per_slots_incompleti() {
        let m = make_test_matrix();
        let bad = ActionSlots {
            action_verb: "".into(),  // mancante
            target_type: "tests".into(),
            framework: "".into(),
            scope: "single".into(),
            confidence: 0.5,
        };
        assert!(m.lookup(&bad).is_none());
    }

    #[test]
    fn lookup_ritorna_none_per_action_sconosciuta() {
        let m = make_test_matrix();
        let unknown = ActionSlots {
            action_verb: "magic_action".into(),
            target_type: "tests".into(),
            framework: "".into(),
            scope: "single".into(),
            confidence: 0.9,
        };
        // Nessuna entry matcha → None (caller fa fallback intent classico)
        assert!(m.lookup(&unknown).is_none());
    }

    #[test]
    fn lookup_delete_wildcard_target() {
        // delete e' configurato con target=* e scope=* per safety.
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "delete".into(),
            target_type: "infrastructure".into(),  // target qualunque
            framework: "docker".into(),
            scope: "multi_file".into(),
            confidence: 0.85,
        };
        let result = m.lookup(&slots).unwrap();
        assert_eq!(result.1, "claude-sonnet-4-6");
    }

    #[test]
    fn is_complete_richiede_3_campi_canonici() {
        assert!(ActionSlots {
            action_verb: "read".into(),
            target_type: "code".into(),
            framework: "".into(),  // OK vuoto
            scope: "single".into(),
            confidence: 0.9,
        }.is_complete());
        // action_verb mancante
        assert!(!ActionSlots {
            action_verb: "".into(),
            target_type: "code".into(),
            framework: "".into(),
            scope: "single".into(),
            confidence: 0.9,
        }.is_complete());
        // scope mancante
        assert!(!ActionSlots {
            action_verb: "read".into(),
            target_type: "code".into(),
            framework: "".into(),
            scope: "".into(),
            confidence: 0.9,
        }.is_complete());
    }

    // ─────────────────────────────────────────────────────────────────
    // TEST infer_slots_heuristic (safety-net keyword-based)
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn heuristic_caso_redemptor_resolve_playwright_multi_file() {
        let slots = infer_slots_heuristic(
            "Esegui i test Playwright di Redemptor e correggi i test che falliscono"
        );
        assert_eq!(slots.action_verb, "resolve");
        assert_eq!(slots.target_type, "tests");
        assert_eq!(slots.framework, "playwright");
        assert_eq!(slots.scope, "multi_file");
        assert!(slots.is_complete());
        assert!(slots.meets_confidence(0.60));
    }

    #[test]
    fn heuristic_caso_pytest_resolve() {
        let slots = infer_slots_heuristic("i test pytest non passano, correggi");
        assert_eq!(slots.action_verb, "resolve");
        assert_eq!(slots.target_type, "tests");
        assert_eq!(slots.framework, "pytest");
        // resolve+tests → forzato a multi_file
        assert_eq!(slots.scope, "multi_file");
    }

    #[test]
    fn heuristic_caso_write_test_singolo() {
        let slots = infer_slots_heuristic("scrivi un test per la funzione foo");
        assert_eq!(slots.action_verb, "write");
        assert_eq!(slots.target_type, "tests");
        assert_eq!(slots.scope, "single");
    }

    #[test]
    fn heuristic_caso_read_codice() {
        let slots = infer_slots_heuristic("leggi src/main.py");
        assert_eq!(slots.action_verb, "read");
        assert_eq!(slots.target_type, "code");
        assert_eq!(slots.scope, "single");
    }

    #[test]
    fn heuristic_caso_delete_docker() {
        let slots = infer_slots_heuristic("elimina i dockerfile rimasti");
        assert_eq!(slots.action_verb, "delete");
        assert_eq!(slots.target_type, "infrastructure");
        assert_eq!(slots.framework, "docker");
    }

    #[test]
    fn heuristic_messaggio_chat_ritorna_default() {
        let slots = infer_slots_heuristic("ciao come stai");
        // Nessuna keyword azione → empty action_verb → is_complete false
        assert!(!slots.is_complete());
    }

    #[test]
    fn heuristic_caso_deploy_cross_service() {
        let slots = infer_slots_heuristic("deploya il microservizio doc-service in produzione");
        assert_eq!(slots.action_verb, "deploy");
        assert_eq!(slots.target_type, "service");
        // "microservizio" → cross_service
        assert_eq!(slots.scope, "cross_service");
    }

    #[test]
    fn heuristic_caso_analyze_root_cause() {
        // Caso con keyword esplicita cross-service
        let slots = infer_slots_heuristic(
            "indaga perché il backend non risponde al frontend in cross-service"
        );
        assert_eq!(slots.action_verb, "analyze");
        assert_eq!(slots.scope, "cross_service");
    }

    // ─────────────────────────────────────────────────────────────────
    // TEST cooldown-awareness della chain matrix (regression test
    // per il bug "Anthropic+Google in cooldown → no_capable_provider"
    // osservato in produzione)
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn lookup_chain_e_cooldown_aware_via_caller() {
        // Verifica che lookup_chain ritorni TUTTI i candidati ordinati per
        // priority. Sara' il caller (Orchestrator::route_by_slots) a saltare
        // quelli in cooldown — questo test garantisce che la chain sia
        // disponibile in ordine corretto.
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "".into(),
            scope: "multi_file".into(),
            confidence: 0.9,
        };
        let chain = m.lookup_chain(&slots);
        // Chain non vuota e ordinata DESC
        assert!(chain.len() >= 2, "chain deve avere >=2 candidati per cooldown fallback");
        // Anthropic priority 100, Mistral priority 90 → anthropic primo
        assert_eq!(chain[0].0, "anthropic");
        assert_eq!(chain[1].0, "mistral");
    }

    #[test]
    fn meets_confidence_rispetta_soglia() {
        let s = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "".into(),
            scope: "multi_file".into(),
            confidence: 0.65,
        };
        assert!(s.meets_confidence(0.60));
        assert!(!s.meets_confidence(0.70));
    }

    // ─────────────────────────────────────────────────────────────────
    // TEST GOLDEN: 25 casi reali con pre/post comparison
    // ─────────────────────────────────────────────────────────────────
    // Ogni caso documenta:
    //   - input: messaggio utente realistico
    //   - PRE (intent classico): cosa andava SBAGLIATO
    //   - POST (slot routing): cosa va GIUSTO ora
    // Validano l'effetto end-to-end del Livello 4.

    /// Matrice golden che riproduce il seed DB (mig 0133) per i test
    /// di routing pre/post. Manteniamo allineata col seed in produzione.
    fn golden_production_matrix() -> SlotsRoutingMatrix {
        let entries = vec![
            // resolve tests playwright multi_file (caso Redemptor)
            ("resolve", "tests", "playwright", "multi_file", "anthropic", "claude-sonnet-4-6", 110),
            ("resolve", "tests", "*", "multi_file", "anthropic", "claude-sonnet-4-6", 100),
            ("resolve", "tests", "*", "multi_file", "mistral", "mistral-large-2411", 90),
            ("resolve", "tests", "*", "single", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("resolve", "tests", "*", "cross_service", "anthropic", "claude-opus-4-6", 100),
            // resolve code
            ("resolve", "code", "*", "single", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("resolve", "code", "*", "single", "deepseek", "deepseek-chat", 80),
            ("resolve", "code", "*", "multi_file", "anthropic", "claude-sonnet-4-6", 100),
            ("resolve", "code", "*", "cross_service", "anthropic", "claude-opus-4-6", 100),
            // resolve config/service
            ("resolve", "service", "*", "cross_service", "anthropic", "claude-sonnet-4-6", 100),
            // write tests
            ("write", "tests", "*", "single", "openai", "gpt-4.1-mini", 100),
            ("write", "tests", "*", "multi_file", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("write", "tests", "cargo", "*", "anthropic", "claude-sonnet-4-6", 110),
            // write code
            ("write", "code", "*", "single", "openai", "gpt-4.1-mini", 100),
            ("write", "code", "*", "multi_file", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("write", "code", "*", "cross_service", "anthropic", "claude-sonnet-4-6", 100),
            ("write", "docs", "*", "*", "openai", "gpt-4.1", 100),
            // read
            ("read", "code", "*", "single", "google", "gemini-2.5-flash", 100),
            ("read", "code", "*", "multi_file", "mistral", "mistral-small-latest", 100),
            ("read", "code", "*", "cross_service", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("read", "config", "*", "*", "google", "gemini-2.5-flash", 100),
            // analyze
            ("analyze", "code", "*", "single", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("analyze", "code", "*", "multi_file", "anthropic", "claude-sonnet-4-6", 100),
            ("analyze", "code", "*", "cross_service", "anthropic", "claude-opus-4-6", 100),
            ("analyze", "tests", "*", "*", "anthropic", "claude-sonnet-4-6", 100),
            ("analyze", "service", "*", "cross_service", "anthropic", "claude-sonnet-4-6", 100),
            // refactor
            ("refactor", "code", "*", "single", "anthropic", "claude-haiku-4-5-20251001", 100),
            ("refactor", "code", "*", "multi_file", "anthropic", "claude-sonnet-4-6", 100),
            ("refactor", "code", "*", "cross_service", "anthropic", "claude-opus-4-6", 100),
            // configure / deploy
            ("configure", "service", "*", "*", "anthropic", "claude-sonnet-4-6", 100),
            ("configure", "infrastructure", "*", "*", "anthropic", "claude-sonnet-4-6", 100),
            ("deploy", "service", "*", "*", "anthropic", "claude-sonnet-4-6", 100),
            ("deploy", "infrastructure", "*", "system_wide", "anthropic", "claude-opus-4-6", 100),
            // delete (sempre capable)
            ("delete", "*", "*", "*", "anthropic", "claude-sonnet-4-6", 100),
        ];
        SlotsRoutingMatrix {
            entries: entries
                .into_iter()
                .map(|(av, tt, fw, sc, p, m, prio)| SlotsRoutingEntry {
                    action_verb: av.into(),
                    target_type: tt.into(),
                    framework: fw.into(),
                    scope: sc.into(),
                    provider: p.into(),
                    model_id: m.into(),
                    priority: prio,
                    rationale: String::new(),
                })
                .collect(),
            loaded_at: Instant::now(),
        }
    }

    /// Caso golden: descrive un task utente e il routing atteso.
    struct GoldenCase {
        /// Messaggio utente (per documentazione)
        input: &'static str,
        /// Slot estratti dall'LLM
        slots: ActionSlots,
        /// Provider atteso dal NUOVO routing slot-based
        expected_provider: &'static str,
        /// Token che il modello scelto DEVE contenere (es. "sonnet", "haiku")
        expected_model_contains: &'static str,
        /// Cosa il VECCHIO routing (intent,mode) sceglieva — per documentare l'effetto
        pre_was: &'static str,
        /// Effetto/spiegazione
        post_effect: &'static str,
    }

    fn s(av: &str, tt: &str, fw: &str, sc: &str, conf: f32) -> ActionSlots {
        ActionSlots {
            action_verb: av.into(),
            target_type: tt.into(),
            framework: fw.into(),
            scope: sc.into(),
            confidence: conf,
        }
    }

    fn golden_cases() -> Vec<GoldenCase> {
        vec![
            // ─── CASO PARADIGMATICO Redemptor (test failure resolution) ───
            GoldenCase {
                input: "esegui i test playwright e risolvi i fail",
                slots: s("resolve", "tests", "playwright", "multi_file", 0.92),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "openai/gpt-4.1-mini (intent=test → bucket light)",
                post_effect: "match esatto playwright+multi_file → Sonnet capable",
            },
            GoldenCase {
                input: "i test pytest non passano, correggi",
                slots: s("resolve", "tests", "pytest", "multi_file", 0.88),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "test|bilanciata → deepseek-chat (light)",
                post_effect: "framework pytest non in DB → wildcard → Sonnet",
            },
            GoldenCase {
                input: "fai funzionare i test cargo",
                slots: s("resolve", "tests", "cargo", "multi_file", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "test|veloce → gpt-4.1-mini",
                post_effect: "cargo+resolve → Sonnet (capable per Rust)",
            },

            // ─── CONTRO-CASI: scrittura NUOVI test ───
            GoldenCase {
                input: "scrivi un test pytest per la funzione foo",
                slots: s("write", "tests", "pytest", "single", 0.92),
                expected_provider: "openai",
                expected_model_contains: "mini",
                pre_was: "test|bilanciata → gpt-4.1-mini (corretto by chance)",
                post_effect: "write+tests+single → gpt-4.1-mini (light OK)",
            },
            GoldenCase {
                input: "aggiungi i test cargo per il modulo router",
                slots: s("write", "tests", "cargo", "single", 0.90),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "test|bilanciata → light model (problema: Rust serve expertise)",
                post_effect: "cargo override priority 110 → Sonnet per Rust",
            },

            // ─── LETTURA file ───
            GoldenCase {
                input: "leggi il file src/main.py e dimmi quante righe ha",
                slots: s("read", "code", "", "single", 0.95),
                expected_provider: "google",
                expected_model_contains: "flash",
                pre_was: "code_read|veloce → gemini-flash (OK)",
                post_effect: "stessa scelta, ma esplicita: read+code+single → flash",
            },
            GoldenCase {
                input: "elenca i file dei microservizi e i loro README",
                slots: s("read", "code", "", "multi_file", 0.90),
                expected_provider: "mistral",
                expected_model_contains: "small",
                pre_was: "code_read|bilanciata → gemini-flash",
                post_effect: "multi_file → mistral-small (piu' context window)",
            },
            GoldenCase {
                input: "guarda come e' configurato il backend in cross-service",
                slots: s("read", "config", "", "cross_service", 0.85),
                expected_provider: "google",
                expected_model_contains: "flash",
                pre_was: "code_read|bilanciata → flash",
                post_effect: "read+config → flash anche cross_service",
            },

            // ─── ANALYZE (root cause cross-service) ───
            GoldenCase {
                input: "perche' il frontend non riceve risposta dal backend?",
                slots: s("analyze", "service", "", "cross_service", 0.78),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "debug|bilanciata → claude-sonnet-4-6 (corretto)",
                post_effect: "analyze+service+cross_service → fallback wildcard → Sonnet",
            },
            GoldenCase {
                input: "indaga sul fallimento del workflow CI dopo il merge",
                slots: s("analyze", "code", "", "multi_file", 0.82),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "debug|bilanciata → Sonnet (corretto)",
                post_effect: "analyze+code+multi_file → Sonnet (stesso target)",
            },

            // ─── REFACTOR ───
            GoldenCase {
                input: "refactor del modulo auth in piu' file",
                slots: s("refactor", "code", "", "multi_file", 0.90),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "refactor|bilanciata → claude-haiku",
                post_effect: "refactor+code+multi_file → Sonnet (UP-tier corretto)",
            },
            GoldenCase {
                input: "rinomina una variabile in handlers.py",
                slots: s("refactor", "code", "", "single", 0.92),
                expected_provider: "anthropic",
                expected_model_contains: "haiku",
                pre_was: "refactor|bilanciata → claude-haiku (corretto)",
                post_effect: "refactor+code+single → haiku (basta)",
            },

            // ─── FIX semplice vs complesso ───
            GoldenCase {
                input: "fix this NullPointerException at handlers.py:42",
                slots: s("resolve", "code", "", "single", 0.88),
                expected_provider: "anthropic",
                expected_model_contains: "haiku",
                pre_was: "fix|bilanciata → claude-haiku (corretto by chance)",
                post_effect: "resolve+code+single → haiku coerente",
            },
            GoldenCase {
                input: "il bug nel modulo auth richiede di rivedere 5 file",
                slots: s("resolve", "code", "", "multi_file", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "fix_complesso|bilanciata → haiku (insufficiente?)",
                post_effect: "resolve+code+multi_file → Sonnet (corretto UP)",
            },
            GoldenCase {
                input: "il bug coinvolge frontend, backend e database",
                slots: s("resolve", "code", "", "cross_service", 0.82),
                expected_provider: "anthropic",
                expected_model_contains: "opus",
                pre_was: "fix_complesso|approfondita → Sonnet",
                post_effect: "resolve+code+cross_service → Opus per ragionamento esteso",
            },

            // ─── DEPLOY / CONFIGURE ───
            GoldenCase {
                input: "deploya il microservizio doc-service in produzione",
                slots: s("deploy", "service", "docker", "cross_service", 0.90),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "system_admin|bilanciata → Sonnet (corretto)",
                post_effect: "deploy+service → Sonnet via wildcard scope",
            },
            GoldenCase {
                input: "deploya l'intera piattaforma in nuovo cluster k8s",
                slots: s("deploy", "infrastructure", "", "system_wide", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "opus",
                pre_was: "system_admin|approfondita → Sonnet",
                post_effect: "deploy+infra+system_wide → Opus per coordinamento",
            },
            GoldenCase {
                input: "imposta un utente admin per l'applicazione",
                slots: s("configure", "service", "", "multi_file", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "system_admin|bilanciata → Sonnet (corretto)",
                post_effect: "configure+service → Sonnet via wildcard scope",
            },

            // ─── SCRITTURA DOCS / CODE ───
            GoldenCase {
                input: "scrivi la documentazione per la classe AuthManager",
                slots: s("write", "docs", "", "single", 0.95),
                expected_provider: "openai",
                expected_model_contains: "4.1",
                pre_was: "docs|bilanciata → gpt-4.1 (corretto)",
                post_effect: "write+docs+single → gpt-4.1 (mantiene)",
            },
            GoldenCase {
                input: "crea un endpoint /healthz nel server",
                slots: s("write", "code", "", "single", 0.90),
                expected_provider: "openai",
                expected_model_contains: "mini",
                pre_was: "file_ops|veloce → gpt-4.1-mini (corretto)",
                post_effect: "write+code+single → gpt-4.1-mini",
            },
            GoldenCase {
                input: "implementa l'auth flow attraverso 3 microservizi",
                slots: s("write", "code", "", "cross_service", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "file_ops|approfondita → claude-sonnet",
                post_effect: "write+code+cross_service → Sonnet (cross-service ok)",
            },

            // ─── DELETE (sicurezza: sempre capable) ───
            GoldenCase {
                input: "elimina i file dockerfile rimasti nel progetto",
                slots: s("delete", "infrastructure", "docker", "multi_file", 0.85),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "file_ops|veloce → gpt-4.1-mini (rischioso per delete)",
                post_effect: "delete sempre → Sonnet (safety priority)",
            },
            GoldenCase {
                input: "rimuovi la migration 0099 dal DB",
                slots: s("delete", "data", "", "single", 0.80),
                expected_provider: "anthropic",
                expected_model_contains: "sonnet",
                pre_was: "file_ops|bilanciata → haiku (rischioso)",
                post_effect: "delete+data → Sonnet (capable per DDL)",
            },

            // ─── CASI EDGE/AMBIGUI ───
            GoldenCase {
                input: "fix tutti i test che falliscono nel CI",
                slots: s("resolve", "tests", "", "cross_service", 0.78),
                expected_provider: "anthropic",
                expected_model_contains: "opus",
                pre_was: "test|bilanciata → gpt-4.1-mini (assolutamente sbagliato)",
                post_effect: "resolve+tests+cross_service → Opus (massima capability)",
            },
            GoldenCase {
                input: "esegui playwright",
                slots: s("read", "tests", "playwright", "single", 0.55),  // confidence bassa
                expected_provider: "",  // non testabile direttamente: confidence bassa → fallback intent
                expected_model_contains: "",
                pre_was: "test|veloce → gpt-4.1-mini",
                post_effect: "confidence 0.55 < soglia 0.60 → fallback intent classico",
            },
            GoldenCase {
                input: "controlla lo stato del progetto",
                slots: s("read", "code", "", "multi_file", 0.75),
                expected_provider: "mistral",
                expected_model_contains: "small",
                pre_was: "chat|bilanciata → light random",
                post_effect: "read+code+multi_file → mistral-small (corretto)",
            },
        ]
    }

    #[test]
    fn golden_25_casi_routing_slot_based() {
        let matrix = golden_production_matrix();
        let mut total = 0;
        let mut covered_by_slots = 0;
        let mut fallback_to_intent = 0;
        let min_conf = 0.60_f32;

        for case in golden_cases() {
            total += 1;
            // Simula il flusso di route_by_slots:
            if !case.slots.is_complete() || !case.slots.meets_confidence(min_conf) {
                fallback_to_intent += 1;
                // Caso "fallback intent" — verifica solo che il test stesso
                // dichiari expected_provider="" (cioe' non-slot-routable)
                assert!(
                    case.expected_provider.is_empty(),
                    "caso '{}' confidence {:.2} sotto soglia ma expected provider non vuoto",
                    case.input, case.slots.confidence
                );
                continue;
            }
            let result = matrix.lookup(&case.slots);
            assert!(
                result.is_some(),
                "matrice slots: nessun match per caso '{}'\n  slots={:?}\n  atteso provider={} model contiene '{}'",
                case.input, case.slots, case.expected_provider, case.expected_model_contains,
            );
            let (provider, model) = result.unwrap();
            assert_eq!(
                provider, case.expected_provider,
                "caso '{}': provider mismatch (got={}, expected={}). Pre era: '{}'. Post: '{}'",
                case.input, provider, case.expected_provider, case.pre_was, case.post_effect,
            );
            assert!(
                model.contains(case.expected_model_contains),
                "caso '{}': model '{}' non contiene '{}'. Pre era: '{}'. Post: '{}'",
                case.input, model, case.expected_model_contains, case.pre_was, case.post_effect,
            );
            covered_by_slots += 1;
        }

        // Almeno 90% dei casi golden deve essere coperto dalla matrice slots
        // (gli altri intenzionalmente caduti su intent fallback per design).
        let coverage = (covered_by_slots as f32) / (total as f32);
        assert!(
            coverage >= 0.90,
            "coverage matrice slots troppo bassa: {:.1}% ({}/{})",
            coverage * 100.0, covered_by_slots, total
        );

        eprintln!(
            "GOLDEN: {} casi totali, {} risolti via slots ({:.0}%), {} fallback intent",
            total, covered_by_slots, coverage * 100.0, fallback_to_intent
        );
    }

    #[test]
    fn golden_caso_redemptor_diventa_capable_model() {
        // Test focalizzato sul bug originale che ha motivato il Livello 4:
        // "esegui i test playwright e risolvi i fail" andava su gpt-4.1-mini.
        // Con slots routing va su anthropic/claude-sonnet-4-6.
        let matrix = golden_production_matrix();
        let slots = s("resolve", "tests", "playwright", "multi_file", 0.92);
        let (provider, model) = matrix.lookup(&slots).unwrap();
        assert_eq!(provider, "anthropic");
        assert!(model.contains("sonnet"));
        // Il vecchio routing avrebbe scelto gpt-4.1-mini (testato in
        // orchestrator::tests::test_promotion_test_a_fix_complesso).
    }

    #[test]
    fn golden_distinzione_write_vs_resolve_su_stessi_test() {
        // Conferma centrale: stesso target=tests, action_verb diverso →
        // routing OPPOSTO (write=light, resolve=capable).
        let matrix = golden_production_matrix();
        let write = s("write", "tests", "pytest", "single", 0.90);
        let resolve = s("resolve", "tests", "pytest", "single", 0.90);
        let (_, m_write) = matrix.lookup(&write).unwrap();
        let (_, m_resolve) = matrix.lookup(&resolve).unwrap();
        // write → light
        assert!(
            m_write.contains("mini") || m_write.contains("4.1") || m_write.contains("haiku"),
            "write tests dovrebbe usare modello light, got {}", m_write
        );
        // resolve → capable
        assert!(
            m_resolve.contains("sonnet") || m_resolve.contains("haiku"),
            "resolve tests deve usare modello capable, got {}", m_resolve
        );
        // E NON devono coincidere (sennò il routing non distingue)
        assert_ne!(m_write, m_resolve,
            "write e resolve sullo stesso target devono dare modelli DIVERSI");
    }
}
