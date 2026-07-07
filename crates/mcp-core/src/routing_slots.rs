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

use std::sync::Arc;

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
        !self.action_verb.is_empty() && !self.target_type.is_empty() && !self.scope.is_empty()
    }

    /// Soglia minima sopra la quale fidarsi dello slot filling.
    /// Sotto questa soglia: fallback al routing classico (intent,mode).
    pub fn meets_confidence(&self, min: f32) -> bool {
        self.confidence >= min
    }
}

/// Una riga della matrice slots: una chiave (action, target, framework, scope)
/// -> requisito di routing per TIER. Niente provider/model pinnato (mig 0357):
/// la scelta concreta e' delegata al punto unico tier-based.
#[derive(Debug, Clone)]
pub struct SlotsRoutingEntry {
    pub action_verb: String,
    pub target_type: String,
    pub framework: String,
    pub scope: String,
    pub preferred_tier: String,
    pub required_capabilities: Vec<String>,
    pub requires_tool_use: bool,
    pub cost_direction: String,
}

/// Requisito di routing derivato dal lookup di una chiave slot. Non contiene
/// provider/model: il chiamante (`Orchestrator::route_by_slots`) lo passa al
/// punto unico `select_models_for_requirement` che sceglie provider+modello
/// concreto per tier+disponibilita'. Vedi mig 0357 + regola G/H/L.
#[derive(Debug, Clone)]
pub struct SlotRequirement {
    pub preferred_tier: String,
    pub required_capabilities: Vec<String>,
    pub requires_tool_use: bool,
    pub cost_direction: String,
}

/// Matrice slots in memoria. Cache TTL 60s, refresh background.
#[derive(Debug, Clone)]
pub struct SlotsRoutingMatrix {
    /// Tutte le entry attive. Lookup itera con fallback wildcard.
    /// (Numero piccolo: ~50-200 entry. Iterazione lineare e' OK.)
    pub entries: Vec<SlotsRoutingEntry>,
}

impl SlotsRoutingMatrix {
    /// Lookup gerarchico:
    /// 1. match esatto su tutti 4 i campi
    /// 2. fallback con framework='*'
    /// 3. fallback con scope='*'
    /// 4. fallback con target_type='*'
    /// 5. None
    ///
    /// Per ogni livello di specificita' decrescente, se una o piu' entry
    /// matchano la chiave, ritorna il requisito tier+capability della piu'
    /// specifica (match esatto preferito al wildcard). Niente provider/model:
    /// la scelta concreta e' del punto unico tier-based (mig 0357).
    pub fn lookup(&self, slots: &ActionSlots) -> Option<SlotRequirement> {
        if !slots.is_complete() {
            return None;
        }
        for (fw_probe, scope_probe, target_probe) in probe_sequence(slots) {
            if let Some(req) = self.match_probe(slots, fw_probe, scope_probe, target_probe) {
                return Some(req);
            }
        }
        None
    }

    /// Filtra le entry che matchano una singola probe e, in caso di collisione,
    /// ritorna il requisito della riga piu' specifica (campi esatti preferiti ai
    /// wildcard). None se nessuna entry matcha questo livello di specificita'.
    fn match_probe(
        &self,
        slots: &ActionSlots,
        fw_probe: Option<&str>,
        scope_probe: Option<&str>,
        target_probe: Option<&str>,
    ) -> Option<SlotRequirement> {
        let mut matches: Vec<&SlotsRoutingEntry> = self
            .entries
            .iter()
            .filter(|e| entry_matches_probe(e, slots, fw_probe, scope_probe, target_probe))
            .collect();
        if matches.is_empty() {
            return None;
        }
        // Preferenza al match piu' specifico (campi esatti sui wildcard)
        // se piu' righe collidono nello stesso probe.
        matches.sort_by_key(|e| {
            let exact = (e.target_type != "*") as u8
                + (e.scope != "*") as u8
                + (e.framework != "*") as u8;
            std::cmp::Reverse(exact)
        });
        let e = matches[0];
        Some(SlotRequirement {
            preferred_tier: e.preferred_tier.clone(),
            required_capabilities: e.required_capabilities.clone(),
            requires_tool_use: e.requires_tool_use,
            cost_direction: e.cost_direction.clone(),
        })
    }

    /// Conta le entry attive (utile per dashboard + test).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True se non ci sono entry (DB vuoto o tabella appena migrata).
    #[expect(
        dead_code,
        reason = "complemento richiesto da clippy::len_without_is_empty: len() e' usato in produzione"
    )]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Sequenza di probe a wildcard-relaxation crescente per il lookup gerarchico:
/// (framework, scope, target). `Some(x)` = match esatto o riga '*'; `None` =
/// solo riga '*'. L'ordine riflette la specificita' decrescente documentata su
/// `lookup`.
fn probe_sequence(slots: &ActionSlots) -> [(Option<&str>, Option<&str>, Option<&str>); 5] {
    [
        (
            Some(slots.framework.as_str()),
            Some(slots.scope.as_str()),
            Some(slots.target_type.as_str()),
        ),
        (
            None,
            Some(slots.scope.as_str()),
            Some(slots.target_type.as_str()),
        ),
        (None, None, Some(slots.target_type.as_str())),
        (
            Some(slots.framework.as_str()),
            None,
            Some(slots.target_type.as_str()),
        ),
        (None, None, None),
    ]
}

/// True se `e` matcha la chiave `slots` per la probe data. `None` su un campo
/// significa "solo riga '*'"; `Some(v)` significa "match diretto oppure riga
/// '*'". Il framework tratta le stringhe vuote come wildcard.
fn entry_matches_probe(
    e: &SlotsRoutingEntry,
    slots: &ActionSlots,
    fw_probe: Option<&str>,
    scope_probe: Option<&str>,
    target_probe: Option<&str>,
) -> bool {
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
    match fw_probe {
        Some(f) if !f.is_empty() => e.framework == f || e.framework == "*",
        _ => e.framework == "*",
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

    let action_verb = infer_action_verb(&lc);
    if action_verb.is_empty() {
        return ActionSlots::default();
    }
    let target_type = infer_target_type(&lc);
    let framework = infer_framework(&lc);
    let scope = infer_scope(&lc, action_verb, target_type);

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

// Keyword per categoria d'azione (estratte come const per tenere
// `infer_action_verb` sotto la soglia di lunghezza: sono dati, non logica).
const RESOLVE_KW: &[&str] = &[
    "risolv",
    "correggi",
    "ripara",
    "fix the",
    " fix ",
    "make work",
    "make pass",
    "fai funzionare",
    "fai passare",
    "non passano",
    "stanno fallendo",
    "are failing",
    "is failing",
    "non funziona",
    "make them pass",
    "esegui i test e",
];
const WRITE_KW: &[&str] = &[
    "scrivi",
    "aggiung",
    "crea ",
    "create ",
    "aggiungi",
    "implementa",
    "implement",
    "add ",
    "write new",
    "write a",
    "scrivere",
];
const READ_KW: &[&str] = &[
    "leggi",
    "leggere",
    "elenca",
    "mostra",
    "ispez",
    "controlla lo stato",
    "list files",
    "list ",
    "guarda",
];
const ANALYZE_KW: &[&str] = &[
    "perche'",
    "perché",
    "perche ",
    "why does",
    "investiga",
    "analizza",
    "indaga",
    "root cause",
];
const REFACTOR_KW: &[&str] = &["refactor", "rinomina", "ristruttur", "estrai funzione"];
const CONFIGURE_KW: &[&str] = &[
    "configur",
    "imposta",
    "setup",
    "set up",
    "abilit",
    "disabilit",
];
const DEPLOY_KW: &[&str] = &["deploy", "deploya", "rilascia", "rilancia"];
const DELETE_KW: &[&str] = &["elimin", "rimuov", "cancell", "delete", "remove"];

/// Deriva l'`action_verb` canonico dalle keyword nel messaggio (gia' in
/// lowercase). Ritorna "" se nessuna categoria d'azione matcha (in quel caso
/// il chiamante fa fallback ad `ActionSlots::default()`). Ordine di priorita'
/// preservato dalla catena `if/else` originale.
fn infer_action_verb(lc: &str) -> &'static str {
    let has_any = |kw: &[&str]| kw.iter().any(|k| lc.contains(k));
    if has_any(RESOLVE_KW) {
        "resolve"
    } else if has_any(DELETE_KW) {
        "delete"
    } else if has_any(DEPLOY_KW) {
        "deploy"
    } else if has_any(CONFIGURE_KW) {
        "configure"
    } else if has_any(REFACTOR_KW) {
        "refactor"
    } else if has_any(ANALYZE_KW) {
        "analyze"
    } else if has_any(WRITE_KW) {
        "write"
    } else if has_any(READ_KW) {
        "read"
    } else {
        ""
    }
}

/// Deriva il `target_type` canonico dalle keyword nel messaggio (lowercase).
/// Default "code" se nessun target specifico matcha. Ordine preservato.
fn infer_target_type(lc: &str) -> &'static str {
    if lc.contains("test")
        || lc.contains("playwright")
        || lc.contains("pytest")
        || lc.contains("jest")
        || lc.contains("vitest")
        || lc.contains("cargo test")
    {
        "tests"
    } else if lc.contains("docker") || lc.contains("k8s") || lc.contains("dockerfile") {
        "infrastructure"
    } else if lc.contains("config")
        || lc.contains(".yml")
        || lc.contains(".yaml")
        || lc.contains(".toml")
        || lc.contains(".env")
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
    }
}

/// Deriva il `framework` canonico dalle keyword nel messaggio (lowercase).
/// Ritorna "" (opzionale) se nessun framework noto matcha. Ordine preservato.
fn infer_framework(lc: &str) -> &'static str {
    if lc.contains("playwright") {
        "playwright"
    } else if lc.contains("pytest") {
        "pytest"
    } else if lc.contains("cargo") {
        "cargo"
    } else if lc.contains("jest") {
        "jest"
    } else if lc.contains("vitest") {
        "vitest"
    } else if lc.contains("docker") {
        "docker"
    } else if lc.contains("nextjs") || lc.contains("next.js") {
        "nextjs"
    } else {
        ""
    }
}

/// Deriva lo `scope` canonico dalle keyword nel messaggio (lowercase). Usa
/// anche `action_verb`/`target_type` gia' derivati per la regola "esegui test
/// e risolvi" (quasi sempre multi-file). Default "single". Ordine preservato.
fn infer_scope(lc: &str, action_verb: &str, target_type: &str) -> &'static str {
    if lc.contains("cross-service")
        || lc.contains("cross service")
        || lc.contains("microservi")
        || lc.contains("frontend e backend")
        || lc.contains("backend e frontend")
        || lc.contains("piu' servizi")
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
    } else if lc.contains("tutto il sistema")
        || lc.contains("system-wide")
        || lc.contains("intera piattaforma")
    {
        "system_wide"
    } else {
        "single"
    }
}

async fn fetch_slots_from_db(db: &PgPool) -> Result<SlotsRoutingMatrix, String> {
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        Vec<String>,
        bool,
        String,
    )> = sqlx::query_as(
        r#"SELECT action_verb, target_type, framework, scope,
                      preferred_tier, required_capabilities, requires_tool_use,
                      cost_direction
                 FROM nexus_routing_slots_matrix
                WHERE is_active = TRUE"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query nexus_routing_slots_matrix fallita: {e}"))?;
    let entries: Vec<SlotsRoutingEntry> = rows
        .into_iter()
        .map(
            |(av, tt, fw, sc, tier, caps, tool_use, cost)| SlotsRoutingEntry {
                action_verb: av,
                target_type: tt,
                framework: fw,
                scope: sc,
                preferred_tier: tier,
                required_capabilities: caps,
                requires_tool_use: tool_use,
                cost_direction: cost,
            },
        )
        .collect();
    Ok(SlotsRoutingMatrix { entries })
}

/// Cache thread-safe con refresh background ogni 60s. Pattern identico a
/// `RoutingMatrixCache` in `routing_matrix.rs`.
#[derive(Debug, Clone)]
pub struct SlotsRoutingMatrixCache {
    inner: Arc<RwLock<Option<Arc<SlotsRoutingMatrix>>>>,
}

impl SlotsRoutingMatrixCache {
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
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
                tracing::info!("SlotsRoutingMatrix caricata: {} entry attive (mig 0133)", n);
            }
            Err(e) => {
                tracing::warn!(
                    "SlotsRoutingMatrix init fallita ({e}). \
                     Il routing slot-based sara' disabilitato; \
                     fallback al routing classico (intent, behavior_mode)."
                );
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
                    }
                    Err(e) => {
                        warn!("SlotsRoutingMatrix refresh fallito: {e}");
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        av: &str,
        tt: &str,
        fw: &str,
        sc: &str,
        tier: &str,
        caps: &[&str],
        cost: &str,
    ) -> SlotsRoutingEntry {
        SlotsRoutingEntry {
            action_verb: av.into(),
            target_type: tt.into(),
            framework: fw.into(),
            scope: sc.into(),
            preferred_tier: tier.into(),
            required_capabilities: caps.iter().map(|s| s.to_string()).collect(),
            requires_tool_use: true,
            cost_direction: cost.into(),
        }
    }

    fn make_test_matrix() -> SlotsRoutingMatrix {
        // Matrice tier-based (mig 0357): ogni chiave -> tier, niente provider.
        SlotsRoutingMatrix {
            entries: vec![
                entry(
                    "resolve",
                    "tests",
                    "playwright",
                    "multi_file",
                    "medium",
                    &["code", "fix"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "tests",
                    "*",
                    "multi_file",
                    "medium",
                    &["code", "fix"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "tests",
                    "*",
                    "single",
                    "light",
                    &["code", "fix"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "code",
                    "*",
                    "cross_service",
                    "heavy",
                    &["code", "reasoning", "fix"],
                    "desc",
                ),
                entry("write", "tests", "*", "single", "light", &["code"], "asc"),
                entry("delete", "*", "*", "*", "medium", &["code"], "desc"),
            ],
        }
    }

    #[test]
    fn lookup_match_esatto_framework() {
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.9,
        };
        let req = m.lookup(&slots).unwrap();
        assert_eq!(req.preferred_tier, "medium");
    }

    #[test]
    fn lookup_caso_redemptor_test_failure_resolution() {
        // Caso che ha originato Livello 4: "esegui playwright e risolvi i fail".
        // resolve + tests + multi_file -> tier medium (modello capace).
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.92,
        };
        let req = m.lookup(&slots).unwrap();
        assert_eq!(req.preferred_tier, "medium");
        assert!(req.required_capabilities.iter().any(|c| c == "fix"));
    }

    #[test]
    fn lookup_fallback_a_wildcard_framework() {
        let m = make_test_matrix();
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "vitest".into(), // non in DB → wildcard fallback
            scope: "multi_file".into(),
            confidence: 0.85,
        };
        let req = m.lookup(&slots);
        assert!(req.is_some(), "vitest deve cadere su wildcard");
        assert_eq!(req.unwrap().preferred_tier, "medium");
    }

    #[test]
    fn lookup_distingue_write_tests_da_resolve_tests() {
        // Caso paradigmatico: stesso target=tests, action_verb diverso →
        // tier diverso (write single=light, resolve multi_file=medium).
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
        let write_req = m.lookup(&write_slots).unwrap();
        let resolve_req = m.lookup(&resolve_slots).unwrap();
        assert_eq!(write_req.preferred_tier, "light");
        assert_eq!(resolve_req.preferred_tier, "medium");
        assert_ne!(write_req.preferred_tier, resolve_req.preferred_tier);
    }

    #[test]
    fn lookup_ritorna_none_per_slots_incompleti() {
        let m = make_test_matrix();
        let bad = ActionSlots {
            action_verb: "".into(), // mancante
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
            target_type: "infrastructure".into(), // target qualunque
            framework: "docker".into(),
            scope: "multi_file".into(),
            confidence: 0.85,
        };
        let req = m.lookup(&slots).unwrap();
        assert_eq!(req.preferred_tier, "medium");
    }

    #[test]
    fn is_complete_richiede_3_campi_canonici() {
        assert!(ActionSlots {
            action_verb: "read".into(),
            target_type: "code".into(),
            framework: "".into(), // OK vuoto
            scope: "single".into(),
            confidence: 0.9,
        }
        .is_complete());
        // action_verb mancante
        assert!(!ActionSlots {
            action_verb: "".into(),
            target_type: "code".into(),
            framework: "".into(),
            scope: "single".into(),
            confidence: 0.9,
        }
        .is_complete());
        // scope mancante
        assert!(!ActionSlots {
            action_verb: "read".into(),
            target_type: "code".into(),
            framework: "".into(),
            scope: "".into(),
            confidence: 0.9,
        }
        .is_complete());
    }

    // ─────────────────────────────────────────────────────────────────
    // TEST infer_slots_heuristic (safety-net keyword-based)
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn heuristic_caso_redemptor_resolve_playwright_multi_file() {
        let slots = infer_slots_heuristic(
            "Esegui i test Playwright di Redemptor e correggi i test che falliscono",
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
            "indaga perché il backend non risponde al frontend in cross-service",
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
    fn lookup_preferenza_match_esatto_su_wildcard() {
        // Se per lo stesso probe collidono una riga esatta (framework) e una
        // wildcard, vince la piu' specifica.
        let m = SlotsRoutingMatrix {
            entries: vec![
                entry(
                    "resolve",
                    "tests",
                    "playwright",
                    "multi_file",
                    "heavy",
                    &["code"],
                    "desc",
                ),
                entry(
                    "resolve",
                    "tests",
                    "*",
                    "multi_file",
                    "light",
                    &["code"],
                    "asc",
                ),
            ],
        };
        let slots = ActionSlots {
            action_verb: "resolve".into(),
            target_type: "tests".into(),
            framework: "playwright".into(),
            scope: "multi_file".into(),
            confidence: 0.9,
        };
        assert_eq!(m.lookup(&slots).unwrap().preferred_tier, "heavy");
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
    // TEST GOLDEN tier-based: ogni slot mappa al tier atteso (mig 0357).
    // La scelta provider+modello concreto e' testata in
    // routing_matrix_auto_promoter (select_top_candidates), perche' dipende
    // dal catalog dinamico e non da valori fissi.
    // ─────────────────────────────────────────────────────────────────

    /// Riproduce il seed tier-based di mig 0357 per i casi chiave.
    fn golden_tier_matrix() -> SlotsRoutingMatrix {
        SlotsRoutingMatrix {
            entries: vec![
                entry("read", "code", "*", "single", "light", &["code"], "asc"),
                entry(
                    "read",
                    "code",
                    "*",
                    "multi_file",
                    "medium",
                    &["code"],
                    "asc",
                ),
                entry(
                    "read",
                    "code",
                    "*",
                    "cross_service",
                    "medium",
                    &["code"],
                    "asc",
                ),
                entry("write", "code", "*", "single", "light", &["code"], "asc"),
                entry(
                    "write",
                    "code",
                    "*",
                    "multi_file",
                    "medium",
                    &["code"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "code",
                    "*",
                    "single",
                    "light",
                    &["code", "fix"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "code",
                    "*",
                    "multi_file",
                    "medium",
                    &["code", "fix"],
                    "asc",
                ),
                entry(
                    "resolve",
                    "code",
                    "*",
                    "cross_service",
                    "heavy",
                    &["code", "reasoning", "fix"],
                    "desc",
                ),
                entry(
                    "analyze",
                    "code",
                    "*",
                    "cross_service",
                    "heavy",
                    &["code", "reasoning"],
                    "desc",
                ),
                entry(
                    "refactor",
                    "code",
                    "*",
                    "cross_service",
                    "heavy",
                    &["code", "reasoning"],
                    "desc",
                ),
                entry(
                    "deploy",
                    "infrastructure",
                    "*",
                    "system_wide",
                    "heavy",
                    &["code", "reasoning"],
                    "desc",
                ),
                entry("delete", "*", "*", "*", "medium", &["code"], "desc"),
            ],
        }
    }

    fn s(av: &str, tt: &str, fw: &str, sc: &str) -> ActionSlots {
        ActionSlots {
            action_verb: av.into(),
            target_type: tt.into(),
            framework: fw.into(),
            scope: sc.into(),
            confidence: 0.9,
        }
    }

    #[test]
    fn golden_slot_to_tier_mapping() {
        let m = golden_tier_matrix();
        // (slots) -> tier atteso. Casi chiave che coprono la regressione
        // "read multi_file degradava su modello light".
        let cases: &[(ActionSlots, &str)] = &[
            (s("read", "code", "", "single"), "light"),
            (s("read", "code", "", "multi_file"), "medium"), // era light (bug mistral-small)
            (s("read", "code", "", "cross_service"), "medium"),
            (s("write", "code", "", "single"), "light"),
            (s("write", "code", "", "multi_file"), "medium"),
            (s("resolve", "code", "", "single"), "light"),
            (s("resolve", "code", "", "multi_file"), "medium"),
            (s("resolve", "code", "", "cross_service"), "heavy"),
            (s("analyze", "code", "", "cross_service"), "heavy"),
            (s("deploy", "infrastructure", "", "system_wide"), "heavy"),
            (
                s("delete", "infrastructure", "docker", "multi_file"),
                "medium",
            ),
        ];
        for (slots, expected_tier) in cases {
            let req = m
                .lookup(slots)
                .unwrap_or_else(|| panic!("nessun match per slots {slots:?}"));
            assert_eq!(
                &req.preferred_tier, expected_tier,
                "slot {slots:?}: tier atteso {expected_tier}, ottenuto {}",
                req.preferred_tier
            );
        }
    }

    #[test]
    fn golden_read_multi_file_non_e_light() {
        // Regressione esplicita: read+code+multi_file NON deve piu' essere light
        // (era pinnato su mistral-small-latest, mig 0133) ma almeno medium
        // (mig 0357), cosi' il routing sceglie un Mistral capace
        // (mistral-large-latest) invece del piccolo.
        let m = golden_tier_matrix();
        let req = m.lookup(&s("read", "code", "", "multi_file")).unwrap();
        assert_ne!(req.preferred_tier, "light");
        assert_eq!(req.preferred_tier, "medium");
    }
}
