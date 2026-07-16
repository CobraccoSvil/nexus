//! Qualificazione EMPIRICA dei modelli (fase 4 del gate, mig 0591/0593).
//!
//! Il catalog DICHIARA (capabilities scritte da sync/migrazioni/admin); questo
//! modulo PROVA: esegue sulla riga candidata la batteria di profili di
//! `ai_model_probe_profile` (configurazione, regola G) con richieste REALI
//! (schemi tool veri, system prompt veri), registra ogni tentativo in
//! `ai_model_probe_evidence` (segnali strutturati, regola M) e deriva
//! `qualification_state` + `qualified_capabilities` col punto unico PURO
//! [`derive_capabilities`] (regola L). E' l'UNICO writer della promozione a
//! `qualified` (il CHECK `chk_qualified_implies_probe` blocca chiunque altro).
//!
//! Root cause coperta (incidenti 2026-07-14/15): un modello entrava nel
//! routing agentico sulla sola parola del catalog; la prima richiesta di
//! produzione faceva da probe e la pagavano le figure del consiglio.
//!
//! Prudenza ereditata dal probe (regola H): un esito TRANSIENT/provider-wide
//! non e' MAI punitivo — il giro e' inconclusivo, backoff e si ritenta. Solo
//! un fallimento MODEL-SPECIFIC conclusivo (es. `empty_completion`) squalifica.

use std::time::Duration;

use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use crate::orchestrator::Orchestrator;

/// Chiavi settings (mig 0591/0593, regola G).
const KEY_ROUND_ENABLED: &str = "agent.model_qualification.round_enabled";
const KEY_MAX_PER_ROUND: &str = "agent.model_qualification.max_models_per_round";
const KEY_TTL_DAYS: &str = "agent.model_qualification.requalify_ttl_days";
const KEY_BACKOFF_HOURS: &str = "agent.model_qualification.backoff_hours";

/// Cap del backoff esponenziale (7 giorni): oltre non ha senso attendere di piu'.
const BACKOFF_CAP_HOURS: i64 = 168;
/// Lock `probing` stantio: oltre questa eta' il claim e' di un worker morto.
const STALE_PROBING_MINUTES: i64 = 15;

/// Capability MISURATE dalla suite v1 (P0 chat + P2 agentic): vengono SOLO dai
/// `grants` dei profili superati. Le altre (es. `reasoning`, `vision`) non sono
/// ancora misurate direttamente: vengono EREDITATE dal dichiarato SOLO quando
/// l'intera batteria bloccante passa (il modello regge il carico agentico
/// reale). La misura diretta del reasoning arriva con `thinking_matrix`
/// (fase 5): finche' non c'e', un'eredita' condizionata al probe superato e'
/// il compromesso DICHIARATO — mai un'eredita' incondizionata.
const MEASURED_V1: [&str; 2] = ["chat", "code"];

/// Un profilo della batteria (riga di `ai_model_probe_profile`).
#[derive(Debug, Clone)]
pub(crate) struct ProbeProfile {
    pub profile_key: String,
    pub suite_version: i32,
    pub kind: String,
    pub is_blocking: bool,
    pub applies_when: Option<Value>,
    pub grants: Vec<String>,
    pub payload: Value,
    pub pass_predicate: Value,
    /// La banda che questo profilo certifica se superato (mig 0599). `None` = il
    /// profilo qualifica ma non dice nulla sul tier (es. tool_smoke).
    pub certifies_tier: Option<String>,
}

/// Esito STRUTTURATO di UN tentativo (regola M): deriva da error_class /
/// stop_reason / tool_use_blocks / content, mai dalla prosa.
#[derive(Debug, Clone)]
pub(crate) struct AttemptOutcome {
    pub pass: bool,
    /// `true` = esito non attribuibile al modello (transient, provider-wide,
    /// timeout): non conta ne' come pass ne' come fail conclusivo.
    pub inconclusive: bool,
    pub reason: String,
    pub error_class: Option<String>,
    pub tool_call_count: i64,
    pub content_chars: i64,
    pub stop_reason: String,
}

/// error_class dal segnale STRUTTURATO del gateway (regola M): downcast del
/// `GatewayHttpError` tipizzato -> `details.primary_cause` mappata dal punto
/// unico `error_class_from_primary_cause`. In piu', SPECIFICO del
/// qualificatore: un `client_error` con status 404 sulla richiesta PINNATA e'
/// un rifiuto del MODELLO (Vertex "Publisher model not found") -> classe
/// canonica `model_not_found` (fail conclusivo). Senza questo ponte il 404
/// arrivava al classificatore TESTUALE come 'error' generico -> Transient ->
/// giro inconclusivo perpetuo (misurato sul primo giro reale post-deploy:
/// 15 evidence tutte transient:error sui modelli 404).
/// `None` -> il chiamante ricade sul classificatore testuale.
fn error_class_from_gateway(err: &anyhow::Error) -> Option<String> {
    let gw = err
        .chain()
        .find_map(|c| c.downcast_ref::<crate::nexus_gateway::GatewayHttpError>())?;
    let details = gw.details.as_ref()?;
    let primary = details.get("primary_cause").and_then(Value::as_str)?;
    if let Some(mapped) =
        crate::provider_error_classifier::error_class_from_primary_cause(primary)
    {
        return Some(mapped.to_string());
    }
    if primary == "client_error" {
        // Status STRUTTURATO del primo fallimento (mai il testo): col pin la
        // chain ha un solo elemento.
        let first_status = details
            .get("failures")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|f| f.get("status"))
            .and_then(Value::as_i64);
        if first_status == Some(404) {
            return Some("model_not_found".to_string());
        }
    }
    None
}

/// Valuta UN turno di probe contro il `pass_predicate` del profilo. PURA.
pub(crate) fn evaluate_attempt(turn: &Value, predicate: &Value, latency_ms: i64) -> AttemptOutcome {
    let stop_reason = turn
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_call_count = turn
        .get("tool_use_blocks")
        .and_then(Value::as_array)
        .map(|a| a.len() as i64)
        .unwrap_or(0);
    // Il campo e' `content`: e' cosi' che lo nomina `agent_turn_value_from_gw`
    // (neural_client.rs), l'UNICO produttore di questo Value. Prima leggeva
    // `result` — una chiave che nessuno scrive — quindi `content_chars` era 0
    // per COSTRUZIONE e `min_content_chars: 1` non era soddisfacibile da alcun
    // modello: la batteria bocciava per "empty_content" modelli che rispondevano
    // regolarmente (misurato: mistral-medium-3.5, codestral, open-mistral-nemo e
    // x-ai/grok-4.5 rispondono 'ok' in <2s alla richiesta IDENTICA del probe).
    // Con enforce_routing_gate ogni bocciatura esce dal routing: la batteria
    // stava smontando il parco 4 modelli per giro. Il test che avrebbe dovuto
    // proteggere costruiva a mano un turno con la STESSA chiave inventata:
    // codice e test condividevano l'errore (verifica cieca). Ora il contratto e'
    // ancorato al produttore da `evaluate_attempt_legge_il_turno_reale_del_gateway`.
    let content_chars = turn
        .get("content")
        .and_then(Value::as_str)
        .map(|s| s.trim().chars().count() as i64)
        .unwrap_or(0);
    let error_class = turn
        .get("error_class")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // 1. Errore classificato: la classificazione canonica decide se e' colpa
    //    del modello (conclusivo) o no (inconclusivo). Punto unico riusato dal
    //    probe (regola L).
    if let Some(ec) = &error_class {
        use crate::model_health_probe::Classification;
        let (pass, inconclusive, reason) =
            match crate::model_health_probe::classification_from_error_class(ec) {
                Classification::ModelSpecific(kind, _) => {
                    (false, false, format!("error_class:{kind}"))
                }
                Classification::ProviderWide(kind, _) => {
                    (false, true, format!("provider_wide:{kind}"))
                }
                Classification::Transient(kind, _) => (false, true, format!("transient:{kind}")),
                Classification::Ok => (false, false, format!("error_class:{ec}")),
            };
        return AttemptOutcome {
            pass,
            inconclusive,
            reason,
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        };
    }
    // 2. stop_reason=error senza classe: inconclusivo (stessa prudenza del probe).
    if stop_reason == "error" {
        return AttemptOutcome {
            pass: false,
            inconclusive: true,
            reason: "stop_reason_error".into(),
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        };
    }
    // 3. Predicato del profilo (soglie dal DB, regola G).
    let min_tool_calls = predicate
        .get("min_tool_calls")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let min_content_chars = predicate
        .get("min_content_chars")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let max_latency_ms = predicate.get("max_latency_ms").and_then(Value::as_i64);
    let mut fail_reason: Option<String> = None;
    if tool_call_count < min_tool_calls {
        fail_reason = Some(format!(
            "no_tool_call:{tool_call_count}<{min_tool_calls}{}",
            if stop_reason.is_empty() {
                String::new()
            } else {
                format!(":{stop_reason}")
            }
        ));
    } else if content_chars < min_content_chars {
        fail_reason = Some(format!("empty_content:{content_chars}<{min_content_chars}"));
    } else if let Some(cap) = max_latency_ms {
        if latency_ms > cap {
            fail_reason = Some(format!("latency:{latency_ms}>{cap}"));
        }
    }
    match fail_reason {
        None => AttemptOutcome {
            pass: true,
            inconclusive: false,
            reason: "ok".into(),
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        },
        Some(reason) => AttemptOutcome {
            pass: false,
            inconclusive: false,
            reason,
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        },
    }
}

/// Esito aggregato di UN profilo eseguito (`repeat` tentativi).
#[derive(Debug, Clone)]
pub(crate) struct ProfileRun {
    pub profile_key: String,
    pub grants: Vec<String>,
    pub is_blocking: bool,
    pub passes: u32,
    pub conclusive_fails: u32,
    pub inconclusive: u32,
    /// Pass minimi per promuovere (dal `pass_predicate`, default = repeat).
    pub promote_min: u32,
    pub first_fail_reason: Option<String>,
}

impl ProfileRun {
    fn passed(&self) -> bool {
        self.passes >= self.promote_min
    }
}

/// Stato derivato dall'esecuzione della batteria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DerivedState {
    Qualified,
    Disqualified,
    /// Nessun verdetto attribuibile al modello (transient/provider-wide):
    /// stato invariato + backoff, MAI punitivo (regola H).
    Inconclusive,
}

#[derive(Debug, Clone)]
pub(crate) struct Derived {
    pub state: DerivedState,
    pub qualified_capabilities: Vec<String>,
    pub reason: String,
    /// Policy thinking DERIVATA dalla `thinking_matrix` (fase 5):
    /// `Some((agentic_thinking_policy, uses_thinking_mode))`. `None` = matrice
    /// non eseguita o inconclusiva: la policy del catalog resta invariata.
    pub thinking: Option<(&'static str, bool)>,
    /// La banda MISURATA (mig 0599): il tier piu' alto certificato dalla batteria,
    /// con isteresi. `None` = nessuna banda certificata -> il catalog tiene il suo
    /// `facts_prior` e non viene toccato.
    pub measured_tier: Option<String>,
}

/// Esito AGGREGATO di una configurazione della thinking_matrix (fase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigOutcome {
    Pass,
    FailConclusive,
    Inconclusive,
}

impl ConfigOutcome {
    fn from_run(run: &ProfileRun) -> Self {
        if run.passed() {
            Self::Pass
        } else if run.conclusive_fails > 0 {
            Self::FailConclusive
        } else {
            Self::Inconclusive
        }
    }
}

/// PUNTO UNICO PURO (regola L) della matrice thinking (fase 5 del design):
/// DERIVA `agentic_thinking_policy` dai FATTI osservati (il modello lavora con
/// thinking spento? acceso?) invece di dichiararla a mano — era il campo che
/// nessuno verificava (GAP-5: glm dichiarato reasoning con policy 'none' inerte).
///
/// | off  | native | -> policy (uses_thinking_mode)                |
/// |------|--------|-----------------------------------------------|
/// | PASS | PASS   | none (il thinking non serve: piu' economico)  |
/// | PASS | FAIL   | disable_for_tools (dual-mode: spegni nei tool)|
/// | FAIL | PASS   | native, uses=true (rifiuta thinking spento)   |
/// | FAIL | FAIL   | exclude (non regge il carico in nessun modo)  |
///
/// Qualunque esito inconclusivo -> `None`: nessuna scrittura (mai derivare una
/// policy da un giro non attribuibile al modello, regola H). Match esaustivo:
/// un esito nuovo non compila finche' non ne dichiara la semantica.
pub(crate) fn derive_thinking_policy(
    off: ConfigOutcome,
    native: ConfigOutcome,
) -> Option<(&'static str, bool)> {
    use ConfigOutcome::*;
    match (off, native) {
        (Pass, Pass) => Some(("none", false)),
        (Pass, FailConclusive) => Some(("disable_for_tools", false)),
        (FailConclusive, Pass) => Some(("native", true)),
        (FailConclusive, FailConclusive) => Some(("exclude", false)),
        (Inconclusive, _) | (_, Inconclusive) => None,
    }
}

/// I fatti del catalog su cui il PRIOR puo' esprimersi. Sono i dati che il
/// FORNITORE dichiara (prezzo, finestra) piu' le capability gia' PROVATE dalla
/// batteria — mai il nome del modello.
#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogFacts {
    /// $/M token in input. `None` se il listino non lo dichiara (pricing_state
    /// 'unknown'): un costo 0 non raffinato NON e' "gratis", e' "non lo so".
    pub input_cost: Option<f64>,
    pub context_window: i64,
    /// Capability gia' PROVATE dalla batteria (`qualified_capabilities`), non
    /// quelle dichiarate: il prior non si fida della parola del catalog.
    pub proven_capabilities: Vec<String>,
    /// `agentic_index` di Artificial Analysis (mig 0600), `None` se il modello non
    /// e' coperto o se l'indice e' STANTIO (il chiamante lo azzera oltre
    /// `max_age_hours`: la fonte e' undocumented e puo' sparire — un indice vecchio
    /// non deve passare per fresco).
    pub agentic_index: Option<f64>,
}

/// Le soglie del prior, dal DB (regola G, mig 0599 + 0600).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TierPriorThresholds {
    pub frontier_min_input_cost: f64,
    pub heavy_min_input_cost: f64,
    pub high_min_input_cost: f64,
    pub long_context_tokens: i64,
    /// Soglie sull'`agentic_index`. ASSOLUTE su un indice VERSIONATO (AA v4.1 ha
    /// cambiato 3 benchmark su 9 rispetto a v4.0): per questo stanno nel DB e
    /// vanno riviste a ogni cambio di metodologia.
    pub agentic_index_frontier_min: f64,
    pub agentic_index_heavy_min: f64,
    pub agentic_index_high_min: f64,
    pub agentic_index_medium_min: f64,
}

/// TIER DAI FATTI DICHIARATI (`facts_prior`). PURA.
///
/// E' un RIPIEGO ONESTO in attesa della misura, non una verita': appena la
/// batteria certifica una banda, `measured` lo sostituisce. Ma e' fondato su
/// DATI (quello che il fornitore dichiara col listino, e quello che la batteria
/// ha gia' provato) invece che sul NOME — che e' scorrelato dalla capacita':
/// misurato sugli indici reali, il token `mini` copre intelligence 6.8-50.2
/// (spread 43.4) e `-pro` 14.1-46.5. Un `mini` puo' valere piu' di un `pro`.
///
/// Il prezzo e' il segnale piu' informativo che il fornitore emette: e' LUI a
/// posizionare il modello nel proprio listino, e lo fa col denaro — non con un
/// aggettivo nel nome. Non e' una misura di capacita' (un modello caro puo'
/// essere scarso), per questo il prior cede il passo alla prima banda misurata.
///
/// `None` = nessun fatto utile: il tier resta NULL e il sistema DICE che non lo
/// sa, invece di scrivere 'medium' e far finta (regola G: niente fallback
/// magico). Un tier NULL non e' pericoloso: col gate acceso un modello non
/// qualificato e' gia' fuori dal pool agentico, e resta solo candidato di ultima
/// istanza del failover (che non filtra per tier).
pub(crate) fn derive_tier_prior(
    facts: &CatalogFacts,
    t: &TierPriorThresholds,
) -> Option<&'static str> {
    // 1. L'agentic_index MISURA la capacita' agentica (Agents 34% + Coding 24%
    //    dell'Intelligence Index, su harness con tool): e' il nostro uso esatto.
    //    Vince sul prezzo, che e' solo il posizionamento commerciale del
    //    fornitore. Misurato: col solo prezzo, gpt-5.4-mini (agentic 30.2)
    //    diventava 'heavy' perche' costa >$2, e claude-opus-4-8 (47.2) veniva
    //    DECLASSATO da frontier a heavy. Un mini caro e un frontier economico
    //    rompono la scala del prezzo; l'indice no.
    //    Il chiamante passa `None` se l'indice e' stantio (fonte undocumented).
    if let Some(idx) = facts.agentic_index {
        return Some(if idx >= t.agentic_index_frontier_min {
            "frontier"
        } else if idx >= t.agentic_index_heavy_min {
            "heavy"
        } else if idx >= t.agentic_index_high_min {
            "high"
        } else if idx >= t.agentic_index_medium_min {
            "medium"
        } else {
            "light"
        });
    }
    // 2. RIPIEGO sul prezzo, per il 61% del parco che l'indice non copre. Batte
    //    comunque il nome (misurato: 64 -> 31 inversioni), ma sbaglia: e' un
    //    ripiego dichiarato, non una verita'.
    //
    // Senza prezzo dichiarato il prior non si esprime: `pricing_state='unknown'`
    // significa "placeholder", non "gratis". Dedurre 'light' da un costo 0 non
    // raffinato sarebbe la stessa bugia del nome, con un'altra faccia.
    let cost = facts.input_cost?;
    if cost <= 0.0 {
        return None;
    }
    let base = if cost >= t.frontier_min_input_cost {
        "frontier"
    } else if cost >= t.heavy_min_input_cost {
        "heavy"
    } else if cost >= t.high_min_input_cost {
        "high"
    } else {
        "light"
    };
    // La finestra lunga alza di UN gradino, mai oltre 'heavy': una finestra
    // grande e' un fatto dichiarato dal fornitore, NON una prova che il modello
    // la sappia usare — quella la da' solo il probe `agentic_longctx`. Promuovere
    // a 'frontier' per la sola finestra sarebbe tornare a credere alle etichette.
    let with_ctx = if facts.context_window >= t.long_context_tokens {
        match base {
            "light" => "medium",
            "high" => "heavy",
            other => other,
        }
    } else {
        base
    };
    // Il reasoning PROVATO (non dichiarato) alza di un gradino fino a 'heavy':
    // e' l'unico segnale del prior che viene da una misura nostra.
    let with_reasoning = if facts
        .proven_capabilities
        .iter()
        .any(|c| c.eq_ignore_ascii_case("reasoning"))
    {
        match with_ctx {
            "light" => "medium",
            "medium" => "high",
            other => other,
        }
    } else {
        with_ctx
    };
    Some(with_reasoning)
}

/// TIER MISURATO (`measured`): la banda PIU' ALTA certificata dalla batteria.
/// PURA.
///
/// Criterion-referenced: ogni banda chiede una capacita' che la precedente non
/// ha dimostrato (`high` concatena tool con dipendenza, `heavy` recupera da un
/// errore strutturato, `frontier` regge 100k con retrieval verificabile). Un
/// `heavy` ha FATTO qualcosa che un `medium` non ha fatto, e l'evidenza sta nella
/// riga di `ai_model_probe_evidence`.
///
/// ISTERESI (il meccanismo esisteva gia' nel vocabolario dei predicati, mig 0593:
/// `promote_min_passes` > `hold_min_passes`; qui viene applicato al TIER):
///   - si PROMUOVE con `promote_min` pass su K;
///   - si MANTIENE la banda gia' acquisita con `hold_min` (soglia piu' bassa);
///   - si RETROCEDE solo sotto `hold_min` E con fallimenti CONCLUSIVI.
/// Il gap fra le due soglie E' l'isteresi: senza, un modello oscillerebbe di
/// fascia a ogni riqualifica e destabilizzerebbe il routing.
///
/// Un esito INCONCLUSIVO non declassa mai (regola H: un transiente non e' colpa
/// del modello). Overlap = pareggio: la riclassificazione ha bisogno di evidenza,
/// la conservazione no.
///
/// `current` = il tier gia' acquisito (per l'isteresi). `None` -> nessuna banda
/// certificata: il chiamante ricade sul prior.
pub(crate) fn derive_tier_measured(
    runs: &[(ProfileRun, Option<String>)],
    current: Option<&str>,
    hold_min: u32,
) -> Option<String> {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let mut migliore: Option<&str> = None;
    for (run, certifies) in runs {
        let Some(banda) = certifies.as_deref() else {
            continue; // il profilo non certifica un tier (es. tool_smoke)
        };
        let acquisita = current.is_some_and(|c| tier_rank(banda) <= tier_rank(c));
        // Soglia asimmetrica: piu' bassa per CONSERVARE una banda gia' acquisita,
        // piu' alta per conquistarne una nuova.
        let soglia = if acquisita {
            hold_min.min(run.promote_min)
        } else {
            run.promote_min
        };
        if run.passes >= soglia && migliore.is_none_or(|m| tier_rank(banda) > tier_rank(m)) {
            migliore = Some(banda);
        }
    }
    migliore.map(str::to_string)
}

/// PUNTO UNICO PURO (regola L): l'evidenza diventa stato + capability PROVATE.
/// `declared` = jsonb `capabilities` della riga (il dichiarato).
pub(crate) fn derive_capabilities(declared: &[String], runs: &[ProfileRun]) -> Derived {
    // Un blocking con fallimenti CONCLUSIVI sotto soglia squalifica.
    for r in runs {
        if r.is_blocking && !r.passed() && r.conclusive_fails > 0 {
            return Derived {
                state: DerivedState::Disqualified,
                qualified_capabilities: Vec::new(),
                reason: format!(
                    "{}:{}",
                    r.profile_key,
                    r.first_fail_reason.as_deref().unwrap_or("failed")
                ),
                thinking: None,
                measured_tier: None,
            };
        }
    }
    // Nessun fallimento conclusivo ma qualche blocking non ha raggiunto la
    // soglia (troppi inconclusivi): giro non attribuibile al modello.
    if runs.iter().any(|r| r.is_blocking && !r.passed()) {
        return Derived {
            state: DerivedState::Inconclusive,
            qualified_capabilities: Vec::new(),
            reason: "inconclusive_round".into(),
            thinking: None,
            measured_tier: None,
        };
    }
    // Batteria superata: il PROVATO = unione dei grants dei profili passati,
    // piu' i tag dichiarati che la suite v1 non misura (vedi MEASURED_V1).
    let mut caps: Vec<String> = Vec::new();
    for r in runs.iter().filter(|r| r.passed()) {
        for g in &r.grants {
            if !caps.contains(g) {
                caps.push(g.clone());
            }
        }
    }
    for d in declared {
        if !MEASURED_V1.contains(&d.as_str()) && !caps.contains(d) {
            caps.push(d.clone());
        }
    }
    Derived {
        state: DerivedState::Qualified,
        qualified_capabilities: caps,
        reason: "suite_passed".into(),
        thinking: None,
        // La banda MISURATA non si deriva qui: `derive_capabilities` non vede i
        // `certifies_tier` dei profili. La calcola il chiamante con
        // `derive_tier_measured` e la deposita qui — un solo punto che decide il
        // tier (regola L), separato da quello che decide le capability.
        measured_tier: None,
    }
}

// ── Orchestrazione (I/O) ────────────────────────────────────────────────────

async fn setting_i64(db: &PgPool, key: &str, default: i64) -> i64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Carica i profili ENABLED della batteria, ordinati per `ord`.
async fn load_profiles(db: &PgPool) -> Vec<ProbeProfile> {
    let rows = sqlx::query(
        "SELECT profile_key, suite_version, kind, is_blocking, applies_when, \
                grants, payload, pass_predicate \
           FROM ai_model_probe_profile WHERE enabled = TRUE ORDER BY ord",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| ProbeProfile {
            profile_key: r.get("profile_key"),
            suite_version: r.get("suite_version"),
            kind: r.get("kind"),
            is_blocking: r.get("is_blocking"),
            certifies_tier: r.get("certifies_tier"),
            applies_when: r.get("applies_when"),
            grants: r
                .get::<Value, _>("grants")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            payload: r.get("payload"),
            pass_predicate: r.get("pass_predicate"),
        })
        .collect()
}

/// `applies_when.declared_capabilities_contains`: il profilo gira solo se il
/// dichiarato contiene il tag (es. thinking_matrix solo sui reasoning).
fn profile_applies(profile: &ProbeProfile, declared: &[String]) -> bool {
    let Some(cond) = &profile.applies_when else {
        return true;
    };
    match cond
        .get("declared_capabilities_contains")
        .and_then(Value::as_str)
    {
        Some(tag) => declared.iter().any(|c| c == tag),
        None => true,
    }
}

/// Costruisce `(tools_json, messages_json, system_text)` per il profilo.
/// `Err(reason)` se il profilo non e' costruibile (es. template mancante):
/// esito INCONCLUSIVO visibile, mai un fallback silenzioso (regola G/H).
async fn build_profile_request(
    db: &PgPool,
    profile: &ProbeProfile,
) -> Result<(String, String, String), String> {
    match profile.kind.as_str() {
        "chat" => Ok((
            "[]".to_string(),
            json!([{ "role": "user",
                     "content": "Verifica operativa: rispondi con la sola parola: ok" }])
            .to_string(),
            "Sei in una verifica di raggiungibilita'. Rispondi in modo conciso.".to_string(),
        )),
        "tool_minimal" => Ok(crate::model_health_probe::build_tool_probe_request()),
        "tool_realistic" | "thinking_matrix" => {
            let tool_names: Vec<String> = profile
                .payload
                .get("tool_names")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if tool_names.is_empty() {
                return Err("payload.tool_names vuoto".into());
            }
            // Schemi REALI dal catalogo statico (punto unico, regola L): la
            // prova usa gli artefatti di produzione, non repliche giocattolo.
            let tools = crate::agent_tools::subagent_native::build_tools_json(&tool_names);
            if tools.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                return Err("nessun tool della whitelist esiste nel catalogo statico".into());
            }
            let template_key = profile
                .payload
                .get("system_template_key")
                .and_then(Value::as_str)
                .unwrap_or("");
            if template_key.is_empty() {
                return Err("payload.system_template_key assente".into());
            }
            let system: Option<String> = sqlx::query_scalar(
                "SELECT content FROM nexus_prompt_templates \
                  WHERE key = $1 ORDER BY version DESC LIMIT 1",
            )
            .bind(template_key)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("lettura template '{template_key}': {e}"))?;
            let Some(system) = system.filter(|s| !s.trim().is_empty()) else {
                return Err(format!("template '{template_key}' assente o vuoto"));
            };
            let history_chars = profile
                .payload
                .get("history_chars")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0) as usize;
            // Filler DETERMINISTICO che simula la history reale di una figura
            // (contesto progetto + richiesta): dimensiona il carico, non il
            // contenuto.
            let filler_unit = "Contesto di progetto: applicazione web con autenticazione JWT, \
                               database Postgres, servizi containerizzati e pipeline di build. ";
            let filler: String = filler_unit
                .chars()
                .cycle()
                .take(history_chars)
                .collect();
            let messages = json!([
                { "role": "user",
                  "content": format!("Materiale di contesto per l'analisi:\n{filler}") },
                { "role": "assistant",
                  "content": "Ho letto il contesto. Procedo con l'analisi richiesta." },
                { "role": "user",
                  "content": "Analizza i rischi dell'autenticazione del progetto: inizia \
                              ispezionando i file rilevanti con i tool a disposizione, poi \
                              dichiara il tuo parere strutturato." }
            ]);
            Ok((tools.to_string(), messages.to_string(), system))
        }
        other => Err(format!("kind profilo non implementato: {other}")),
    }
}

/// Registra un tentativo in `ai_model_probe_evidence` e ritorna l'id.
#[allow(clippy::too_many_arguments)]
async fn insert_evidence(
    db: &PgPool,
    provider: &str,
    model: &str,
    profile: &ProbeProfile,
    attempt: i32,
    latency_ms: i64,
    outcome: &AttemptOutcome,
) -> Option<i64> {
    let verdict = if outcome.inconclusive {
        "inconclusive"
    } else if outcome.pass {
        "pass"
    } else {
        "fail"
    };
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO ai_model_probe_evidence \
         (provider, model, profile_key, suite_version, attempt, latency_ms, error_class, \
          tool_call_count, content_chars, stop_reason, verdict, verdict_reason) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id",
    )
    .bind(provider)
    .bind(model)
    .bind(&profile.profile_key)
    .bind(profile.suite_version)
    .bind(attempt)
    .bind(latency_ms)
    .bind(&outcome.error_class)
    .bind(outcome.tool_call_count)
    .bind(outcome.content_chars)
    .bind(&outcome.stop_reason)
    .bind(verdict)
    .bind(&outcome.reason)
    .fetch_one(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "model_qualification: insert evidence fallita");
        e
    })
    .ok()
}

/// Esegue `repeat` tentativi di UNA richiesta di profilo con configurazione
/// thinking opzionale e aggrega l'esito. `label` distingue le configurazioni
/// della thinking_matrix nell'evidence (prefisso del verdict_reason); vuoto
/// per i profili ordinari. Punto unico del ciclo tentativo->evidence (regola L).
#[allow(clippy::too_many_arguments)]
async fn run_profile_attempts(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    profile: &ProbeProfile,
    request: &(String, String, String),
    repeat: u32,
    timeout_s: u64,
    max_tokens: u32,
    promote_min: u32,
    thinking: Option<crate::nexus_gateway::GwThinkingConfig>,
    label: &str,
    last_evidence: &mut Option<i64>,
) -> ProfileRun {
    let (tools_json, messages_json, system_text) = request;
    let mut run = ProfileRun {
        profile_key: profile.profile_key.clone(),
        grants: profile.grants.clone(),
        is_blocking: profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: 0,
        promote_min,
        first_fail_reason: None,
    };
    for attempt in 1..=repeat {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_s),
            orchestrator.neural.generate_agent_turn_with_thinking(
                provider,
                model,
                messages_json,
                tools_json,
                max_tokens,
                system_text,
                thinking.clone(),
            ),
        )
        .await;
        let latency_ms = started.elapsed().as_millis() as i64;
        let mut outcome = match result {
            Ok(Ok(turn)) => evaluate_attempt(&turn, &profile.pass_predicate, latency_ms),
            Ok(Err(e)) => {
                // Errore di chiamata: PRIMA il segnale strutturato del gateway
                // (regola M), poi il classificatore testuale come fallback.
                let ec = match error_class_from_gateway(&e) {
                    Some(ec) => ec,
                    None => {
                        orchestrator
                            .neural
                            .classify_error(&e.to_string(), provider)
                            .await
                    }
                };
                evaluate_attempt(
                    &json!({ "error_class": ec }),
                    &profile.pass_predicate,
                    latency_ms,
                )
            }
            Err(_elapsed) => AttemptOutcome {
                pass: false,
                inconclusive: true,
                reason: format!("probe_timeout:{timeout_s}s"),
                error_class: None,
                tool_call_count: 0,
                content_chars: 0,
                stop_reason: String::new(),
            },
        };
        if !label.is_empty() {
            outcome.reason = format!("{label}{}", outcome.reason);
        }
        if let Some(id) = insert_evidence(
            db,
            provider,
            model,
            profile,
            attempt as i32,
            latency_ms,
            &outcome,
        )
        .await
        {
            *last_evidence = Some(id);
        }
        if outcome.inconclusive {
            run.inconclusive += 1;
        } else if outcome.pass {
            run.passes += 1;
        } else {
            run.conclusive_fails += 1;
            if run.first_fail_reason.is_none() {
                run.first_fail_reason = Some(outcome.reason.clone());
            }
        }
    }
    run
}

/// Esegue la batteria su UN modello candidato (gia' claimato `probing`).
/// Ritorna (Derived, ultimo evidence id).
async fn qualify_one(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    declared: &[String],
    profiles: &[ProbeProfile],
) -> (Derived, Option<i64>) {
    let mut runs: Vec<ProfileRun> = Vec::new();
    let mut last_evidence: Option<i64> = None;
    let mut thinking_derived: Option<(&'static str, bool)> = None;
    for profile in profiles.iter().filter(|p| profile_applies(p, declared)) {
        let repeat = profile
            .payload
            .get("repeat")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .clamp(1, 5) as u32;
        let timeout_s = profile
            .payload
            .get("timeout_s")
            .and_then(Value::as_i64)
            .unwrap_or(90)
            .clamp(10, 300) as u64;
        let max_tokens = profile
            .payload
            .get("max_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(512)
            .clamp(16, 16384) as u32;
        let promote_min = profile
            .pass_predicate
            .get("promote_min_passes")
            .and_then(Value::as_i64)
            .map(|n| n.clamp(1, repeat as i64) as u32)
            .unwrap_or(repeat);

        let request = match build_profile_request(db, profile).await {
            Err(reason) => {
                // Profilo non costruibile: giro inconclusivo VISIBILE.
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    profile = %profile.profile_key,
                    reason = %reason,
                    "model_qualification: profilo non costruibile -> inconclusivo"
                );
                runs.push(ProfileRun {
                    profile_key: profile.profile_key.clone(),
                    grants: profile.grants.clone(),
                    is_blocking: profile.is_blocking,
                    passes: 0,
                    conclusive_fails: 0,
                    inconclusive: repeat,
                    promote_min,
                    first_fail_reason: None,
                });
                continue;
            }
            Ok(r) => r,
        };
        if profile.kind == "thinking_matrix" {
            // FASE 5: la matrice PROVA il modello in DUE configurazioni thinking
            // esplicite (off e native) e DERIVA agentic_thinking_policy dai
            // fatti — mai ereditare la policy del catalog che stiamo derivando.
            let budget = profile
                .payload
                .get("thinking_budget_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(2048)
                .clamp(256, 32768) as u32;
            let off = run_profile_attempts(
                orchestrator,
                db,
                provider,
                model,
                profile,
                &request,
                repeat,
                timeout_s,
                max_tokens,
                promote_min,
                Some(crate::nexus_gateway::GwThinkingConfig {
                    enabled: false,
                    budget_tokens: None,
                    mandatory: false,
                }),
                "off:",
                &mut last_evidence,
            )
            .await;
            let native = run_profile_attempts(
                orchestrator,
                db,
                provider,
                model,
                profile,
                &request,
                repeat,
                timeout_s,
                max_tokens,
                promote_min,
                Some(crate::nexus_gateway::GwThinkingConfig {
                    enabled: true,
                    budget_tokens: Some(budget),
                    mandatory: true,
                }),
                "native:",
                &mut last_evidence,
            )
            .await;
            thinking_derived = derive_thinking_policy(
                ConfigOutcome::from_run(&off),
                ConfigOutcome::from_run(&native),
            );
            runs.push(off);
            runs.push(native);
            continue;
        }
        let run = run_profile_attempts(
            orchestrator,
            db,
            provider,
            model,
            profile,
            &request,
            repeat,
            timeout_s,
            max_tokens,
            promote_min,
            None,
            "",
            &mut last_evidence,
        )
        .await;
        let blocking_conclusive_fail = run.is_blocking && !run.passed() && run.conclusive_fails > 0;
        runs.push(run);
        if blocking_conclusive_fail {
            // Early-stop: i profili successivi non cambiano il verdetto.
            break;
        }
    }
    let mut derived = derive_capabilities(declared, &runs);
    derived.thinking = thinking_derived;
    // LA BANDA MISURATA (mig 0599). Solo su un esito QUALIFIED: una squalifica o
    // un giro inconclusivo non misurano nulla, e un tier scritto su un esito non
    // attribuibile al modello sarebbe la stessa bugia del nome.
    if derived.state == DerivedState::Qualified {
        // Accoppia ogni run con la banda che il suo profilo certifica. `runs` e
        // `profiles` non sono allineati per indice (i profili non applicabili
        // vengono saltati da `profile_applies`, e l'early-stop tronca `runs`):
        // il join va fatto sulla CHIAVE, mai sulla posizione.
        let con_bande: Vec<(ProfileRun, Option<String>)> = runs
            .iter()
            .map(|r| {
                let banda = profiles
                    .iter()
                    .find(|p| p.profile_key == r.profile_key)
                    .and_then(|p| p.certifies_tier.clone());
                (r.clone(), banda)
            })
            .collect();
        // Il tier ATTUALE serve all'isteresi: conservare una banda gia' acquisita
        // costa meno che conquistarne una nuova.
        let current: Option<String> = sqlx::query_scalar(
            "SELECT performance_tier FROM ai_price_catalog \
              WHERE provider = $1 AND model = $2 LIMIT 1",
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten();
        let hold_min = hold_min_passes(profiles);
        derived.measured_tier = derive_tier_measured(&con_bande, current.as_deref(), hold_min);
    }
    (derived, last_evidence)
}

/// `hold_min_passes` dal `pass_predicate` dei profili (mig 0593: la soglia doppia
/// asimmetrica esiste gia' nel vocabolario). Il MINIMO fra i profili: conservare
/// una banda non deve dipendere da quale profilo la certifica. Default 2 = il
/// valore del seed.
fn hold_min_passes(profiles: &[ProbeProfile]) -> u32 {
    profiles
        .iter()
        .filter_map(|p| {
            p.pass_predicate
                .get("hold_min_passes")
                .and_then(Value::as_u64)
        })
        .min()
        .unwrap_or(2) as u32
}

/// Scrive lo stato derivato sulla riga (writer UNICO della promozione).
async fn apply_derived(
    db: &PgPool,
    provider: &str,
    model: &str,
    profiles_suite: i32,
    derived: &Derived,
    evidence_id: Option<i64>,
    ttl_days: i64,
    backoff_base_hours: i64,
) {
    let res = match derived.state {
        DerivedState::Qualified => {
            // Policy thinking DERIVATA dalla matrice (fase 5): scritta solo se
            // presente e MAI sopra una curatela (capability_locked). Il trigger
            // di invalidazione (0591) non scatta: NEW.capability_source='probe'.
            let (policy, uses_thinking): (Option<&str>, Option<bool>) = match derived.thinking {
                Some((p, u)) => (Some(p), Some(u)),
                None => (None, None),
            };
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = 'qualified', \
                     qualified_capabilities = $3, \
                     capability_source = 'probe', \
                     qualified_at = NOW(), \
                     qualification_expires_at = NOW() + make_interval(days => $4::int), \
                     qualification_suite_version = $5, \
                     qualification_reason = $6, \
                     qualification_evidence_id = $7, \
                     agentic_thinking_policy = CASE \
                         WHEN capability_locked THEN agentic_thinking_policy \
                         ELSE COALESCE($8, agentic_thinking_policy) END, \
                     uses_thinking_mode = CASE \
                         WHEN capability_locked THEN uses_thinking_mode \
                         ELSE COALESCE($9, uses_thinking_mode) END, \
                     performance_tier = CASE \
                         WHEN $10::text IS NULL THEN performance_tier \
                         WHEN tier_source = 'manual' THEN performance_tier \
                         ELSE $10 END, \
                     tier_source = CASE \
                         WHEN $10::text IS NULL THEN tier_source \
                         WHEN tier_source = 'manual' THEN tier_source \
                         ELSE 'measured' END, \
                     qualification_started_at = NULL, \
                     qualification_attempts = 0, \
                     qualification_backoff_until = NULL \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(json!(derived.qualified_capabilities))
            .bind(ttl_days as i32)
            .bind(profiles_suite)
            .bind(&derived.reason)
            .bind(evidence_id)
            .bind(policy)
            .bind(uses_thinking)
            // La banda MISURATA: NULL = nessuna certificata -> il tier resta
            // quello che c'e' (il facts_prior). Il CASE protegge la curatela
            // ('manual' vince sempre) e promuove tier_source a 'measured' solo
            // quando una banda e' stata davvero dimostrata.
            .bind(derived.measured_tier.as_deref())
            .execute(db)
            .await
        }
        DerivedState::Disqualified => {
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = 'disqualified', \
                     qualified_capabilities = '[]'::jsonb, \
                     qualification_reason = $3, \
                     qualification_evidence_id = $4, \
                     qualification_started_at = NULL, \
                     qualification_attempts = qualification_attempts + 1, \
                     qualification_backoff_until = NOW() + make_interval(hours => \
                         LEAST($5::int * (1 << LEAST(qualification_attempts, 6)), $6::int)) \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(&derived.reason)
            .bind(evidence_id)
            .bind(backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(db)
            .await
        }
        DerivedState::Inconclusive => {
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = CASE \
                         WHEN qualification_state = 'qualified' THEN 'qualified' \
                         ELSE 'unqualified' END, \
                     qualification_reason = $3, \
                     qualification_started_at = NULL, \
                     qualification_attempts = qualification_attempts + 1, \
                     qualification_backoff_until = NOW() + make_interval(hours => \
                         LEAST($4::int * (1 << LEAST(qualification_attempts, 6)), $5::int)) \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(&derived.reason)
            .bind(backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(db)
            .await
        }
    };
    if let Err(e) = res {
        tracing::warn!(
            provider = %provider,
            model = %model,
            error = %e,
            "model_qualification: scrittura stato derivato fallita"
        );
    }
}

/// FASE 0 del worker `model_health_probe`: un giro di qualificazione.
/// Candidati (cap per giro): unqualified / qualified scaduti / quarantined /
/// probing stantii, fuori backoff, SOLO righe che il routing agentico userebbe
/// (enabled + tool_use). Claim CAS `FOR UPDATE SKIP LOCKED`: niente doppio
/// probe tra worker concorrenti.
pub(crate) async fn run_qualification_round(orchestrator: &Orchestrator, db: &PgPool) -> usize {
    let enabled = crate::settings::get_setting(db, KEY_ROUND_ENABLED)
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false);
    if !enabled {
        return 0;
    }
    let max_per_round = setting_i64(db, KEY_MAX_PER_ROUND, 4).await;
    let ttl_days = setting_i64(db, KEY_TTL_DAYS, 30).await;
    let backoff_hours = setting_i64(db, KEY_BACKOFF_HOURS, 24).await;

    let profiles = load_profiles(db).await;
    if profiles.is_empty() {
        tracing::warn!(
            "model_qualification: nessun profilo enabled in ai_model_probe_profile \
             (applicare mig 0593): giro saltato"
        );
        return 0;
    }
    let suite_version = profiles.iter().map(|p| p.suite_version).max().unwrap_or(1);

    // Claim CAS. I candidati GIA' qualified (scaduti o con suite vecchia) sono
    // ri-provati IN SHADOW: lo state resta 'qualified' (il pool non si svuota
    // durante la ri-qualificazione); il lock del claim e' qualification_started_at
    // (stantio oltre STALE_PROBING_MINUTES = worker morto, riclaimabile).
    let claimed: Vec<(String, String, Value)> = sqlx::query_as(
        "UPDATE ai_price_catalog c SET \
             qualification_state = CASE WHEN c.qualification_state = 'qualified' \
                                        THEN 'qualified' ELSE 'probing' END, \
             qualification_started_at = NOW() \
         FROM ( \
             SELECT provider, model FROM ai_price_catalog \
              WHERE is_enabled = TRUE AND supports_tool_use = TRUE \
                AND (qualification_backoff_until IS NULL OR qualification_backoff_until < NOW()) \
                AND (qualification_started_at IS NULL \
                     OR qualification_started_at < NOW() - make_interval(mins => $2::int)) \
                AND (qualification_state IN ('unqualified','quarantined','probing') \
                     OR (qualification_state = 'qualified' \
                         AND (qualification_expires_at < NOW() \
                              OR qualification_suite_version < $3))) \
              ORDER BY (qualification_state = 'unqualified') DESC, \
                       qualification_expires_at ASC NULLS FIRST \
              LIMIT $1 \
              FOR UPDATE SKIP LOCKED \
         ) cand \
         WHERE c.provider = cand.provider AND c.model = cand.model \
         RETURNING c.provider, c.model, c.capabilities",
    )
    .bind(max_per_round)
    .bind(STALE_PROBING_MINUTES as i32)
    .bind(suite_version)
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "model_qualification: claim candidati fallito");
        Vec::new()
    });
    if claimed.is_empty() {
        return 0;
    }
    tracing::info!(
        candidati = claimed.len(),
        suite_version,
        "model_qualification: giro di qualificazione avviato"
    );
    let mut done = 0usize;
    for (provider, model, caps) in &claimed {
        // Provider in cooldown: non sprecare il giro (esito non attribuibile).
        if crate::provider_cooldown::is_provider_in_cooldown(provider) {
            apply_derived(
                db,
                provider,
                model,
                suite_version,
                &Derived {
                    state: DerivedState::Inconclusive,
                    qualified_capabilities: Vec::new(),
                    reason: "provider_in_cooldown".into(),
                    thinking: None,
                    // Un giro non attribuibile al modello non misura nessuna banda.
                    measured_tier: None,
                },
                None,
                ttl_days,
                backoff_hours,
            )
            .await;
            continue;
        }
        let declared: Vec<String> = caps
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let (derived, evidence_id) =
            qualify_one(orchestrator, db, provider, model, &declared, &profiles).await;
        tracing::info!(
            provider = %provider,
            model = %model,
            state = ?derived.state,
            reason = %derived.reason,
            qualified_capabilities = %json!(derived.qualified_capabilities),
            "model_qualification: verdetto"
        );
        apply_derived(
            db,
            provider,
            model,
            suite_version,
            &derived,
            evidence_id,
            ttl_days,
            backoff_hours,
        )
        .await;
        done += 1;
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_run(
        key: &str,
        grants: &[&str],
        blocking: bool,
        passes: u32,
        fails: u32,
        inconclusive: u32,
        promote_min: u32,
        first_fail: Option<&str>,
    ) -> ProfileRun {
        ProfileRun {
            profile_key: key.into(),
            grants: grants.iter().map(|s| s.to_string()).collect(),
            is_blocking: blocking,
            passes,
            conclusive_fails: fails,
            inconclusive,
            promote_min,
            first_fail_reason: first_fail.map(str::to_owned),
        }
    }

    /// FIXTURE DI REGRESSIONE dell'incidente reale (design §3.3): il modello
    /// "glm-like" passa chat e tool-smoke ma produce SOLO empty_completion sul
    /// carico agentico reale -> squalificato, zero capability provate. Se la
    /// batteria smette di scartarlo, questo test diventa rosso.
    #[test]
    fn fixture_glm_empty_completion_viene_squalificato() {
        let declared = vec!["chat".into(), "code".into(), "reasoning".into()];
        let runs = vec![
            profile_run("chat_smoke", &["chat"], true, 1, 0, 0, 1, None),
            profile_run("tool_smoke", &[], true, 1, 0, 0, 1, None),
            profile_run(
                "agentic_real",
                &["chat", "code"],
                true,
                0,
                3,
                0,
                3,
                Some("error_class:empty_completion"),
            ),
        ];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Disqualified);
        assert!(d.qualified_capabilities.is_empty());
        assert_eq!(d.reason, "agentic_real:error_class:empty_completion");
    }

    /// FIXTURE gemella: il modello "deepseek-like" supera l'intera batteria ->
    /// qualificato con le capability MISURATE dai grants + il `reasoning`
    /// dichiarato (non ancora misurato dalla suite v1, ereditato SOLO a
    /// batteria superata).
    #[test]
    fn fixture_deepseek_suite_superata_viene_promosso() {
        let declared = vec!["chat".into(), "code".into(), "reasoning".into()];
        let runs = vec![
            profile_run("chat_smoke", &["chat"], true, 1, 0, 0, 1, None),
            profile_run("tool_smoke", &[], true, 1, 0, 0, 1, None),
            profile_run("agentic_real", &["chat", "code"], true, 3, 0, 0, 3, None),
        ];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Qualified);
        assert_eq!(
            d.qualified_capabilities,
            vec!["chat".to_string(), "code".to_string(), "reasoning".to_string()]
        );
    }

    /// Il "millantatore": dichiara reasoning ma FALLISCE la batteria -> il tag
    /// dichiarato NON viene mai ereditato (l'eredita' e' condizionata al probe).
    #[test]
    fn tag_dichiarato_non_ereditato_se_batteria_fallita() {
        let declared = vec!["reasoning".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            1,
            2,
            0,
            3,
            Some("no_tool_call:0<1"),
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Disqualified);
        assert!(d.qualified_capabilities.is_empty());
    }

    /// Transient/provider-wide NON e' mai punitivo (regola H, stessa prudenza
    /// del probe): giro inconclusivo, nessuna squalifica.
    #[test]
    fn giro_inconclusivo_non_squalifica() {
        let declared = vec!["chat".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            1,
            0,
            2,
            3,
            None,
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Inconclusive);
    }

    /// Isteresi: `hold_min_passes` non e' usato dalla promozione (promote_min
    /// = 3/3) ma 2 pass + 1 inconclusivo non squalifica (nessun fail conclusivo).
    #[test]
    fn due_pass_su_tre_con_un_transient_resta_inconclusivo() {
        let declared = vec!["chat".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            2,
            0,
            1,
            3,
            None,
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Inconclusive);
    }

    // ── evaluate_attempt: il verdetto di UN tentativo dai segnali strutturati ──

    #[test]
    fn attempt_empty_completion_e_fail_conclusivo() {
        let turn = json!({ "error_class": "empty_completion" });
        let out = evaluate_attempt(&turn, &json!({"min_tool_calls": 1}), 100);
        assert!(!out.pass);
        assert!(!out.inconclusive, "empty_completion e' MODEL-specific");
        assert!(out.reason.starts_with("error_class:"));
    }

    #[test]
    fn attempt_rate_limit_e_inconclusivo() {
        let turn = json!({ "error_class": "rate_limit" });
        let out = evaluate_attempt(&turn, &json!({}), 100);
        assert!(!out.pass);
        assert!(out.inconclusive, "rate_limit non e' colpa del modello");
    }

    #[test]
    fn attempt_tool_call_richiesta_e_verificata() {
        let ok = json!({ "stop_reason": "tool_use",
                         "tool_use_blocks": [{"name": "read_file"}] });
        let out = evaluate_attempt(&ok, &json!({"min_tool_calls": 1}), 100);
        assert!(out.pass, "{}", out.reason);
        let ko = json!({ "stop_reason": "end_turn", "content": "chiacchiere" });
        let out = evaluate_attempt(&ko, &json!({"min_tool_calls": 1}), 100);
        assert!(!out.pass);
        assert!(out.reason.starts_with("no_tool_call"));
    }

    /// IL TEST DI CONTRATTO (incidente 2026-07-15). Non costruisce il turno a
    /// mano: lo fa produrre a `agent_turn_value_from_gw`, l'UNICO produttore
    /// reale, partendo da una `GwResponse` come quella che il gateway restituisce
    /// davvero. E' l'unico modo di accorgersi che il probe legge una chiave che
    /// nessuno scrive.
    ///
    /// Il difetto che blinda: `evaluate_attempt` leggeva `turn["result"]` mentre
    /// il produttore scrive `turn["content"]` -> content_chars = 0 SEMPRE ->
    /// `min_content_chars: 1` insoddisfacibile da qualunque modello -> la
    /// batteria bocciava per "empty_content" modelli sani (misurato su
    /// mistral-medium-3.5, codestral-2508, open-mistral-nemo, x-ai/grok-4.5: tutti
    /// rispondono 'ok' alla richiesta identica del probe) e, con
    /// enforce_routing_gate acceso, li ESCLUDEVA dal routing 4 per giro.
    ///
    /// I test preesistenti non lo vedevano perche' inventavano il JSON con la
    /// stessa chiave sbagliata del codice: codice e test condividevano l'errore.
    #[test]
    fn evaluate_attempt_legge_il_turno_reale_del_gateway() {
        use crate::nexus_gateway::{GwResponse, GwUsage};
        // La risposta REALE misurata sul gateway per il probe chat_smoke:
        // content='ok', 505ms, nessuna tool-call.
        let resp = GwResponse {
            content: "ok".to_string(),
            tool_calls: None,
            usage: GwUsage {
                input_tokens: 49,
                output_tokens: 2,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "mistral-medium-3.5".to_string(),
            provider_used: "mistral".to_string(),
            latency_ms: 505,
            finish_reason: "stop".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
        };
        let turn = crate::orchestrator::neural_client::agent_turn_value_from_gw(
            "mistral",
            "mistral-medium-3.5",
            &resp,
        );
        // Il predicato REALE del profilo chat_smoke (mig 0593).
        let out = evaluate_attempt(&turn, &json!({"min_content_chars": 1}), 505);
        assert!(
            out.pass,
            "un modello che risponde 'ok' DEVE passare chat_smoke; verdetto: {} \
             (turno prodotto dal gateway: {turn})",
            out.reason
        );
        assert_eq!(
            out.content_chars, 2,
            "i caratteri devono essere contati dal campo che il produttore \
             scrive davvero, non da una chiave inventata"
        );
    }

    #[test]
    fn attempt_latency_oltre_il_cap_fallisce() {
        let turn = json!({ "stop_reason": "tool_use",
                           "tool_use_blocks": [{"name": "read_file"}] });
        let out = evaluate_attempt(&turn, &json!({"min_tool_calls": 1, "max_latency_ms": 30000}), 45000);
        assert!(!out.pass);
        assert!(out.reason.starts_with("latency:"));
    }

    // ── thinking_matrix (fase 5): la policy DERIVATA dai fatti ───────────────

    #[test]
    fn matrice_thinking_deriva_le_quattro_policy() {
        use ConfigOutcome::*;
        // Il modello lavora in entrambe le modalita': niente thinking (economia).
        assert_eq!(derive_thinking_policy(Pass, Pass), Some(("none", false)));
        // Dual-mode che degenera col thinking sotto tool: spegnilo nei tool-loop.
        assert_eq!(
            derive_thinking_policy(Pass, FailConclusive),
            Some(("disable_for_tools", false))
        );
        // Il caso gemini-3 (rifiuta thinkingBudget=0): thinking OBBLIGATORIO.
        assert_eq!(
            derive_thinking_policy(FailConclusive, Pass),
            Some(("native", true))
        );
        // Non regge il carico agentico in NESSUNA configurazione: fuori.
        assert_eq!(
            derive_thinking_policy(FailConclusive, FailConclusive),
            Some(("exclude", false))
        );
    }

    #[test]
    fn matrice_inconclusiva_non_scrive_policy() {
        use ConfigOutcome::*;
        // Qualunque lato inconclusivo -> nessuna scrittura (mai derivare una
        // policy da un giro non attribuibile al modello).
        assert_eq!(derive_thinking_policy(Inconclusive, Pass), None);
        assert_eq!(derive_thinking_policy(Pass, Inconclusive), None);
        assert_eq!(derive_thinking_policy(Inconclusive, FailConclusive), None);
        assert_eq!(derive_thinking_policy(Inconclusive, Inconclusive), None);
    }

    // ── error_class_from_gateway: il ponte strutturato del qualificatore ─────

    fn gw_error(body: Value) -> anyhow::Error {
        crate::nexus_gateway::GatewayHttpError::from_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            body.to_string(),
        )
        .into()
    }

    /// Il caso MISURATO sul primo giro reale post-deploy: Vertex 404 sul pin
    /// arrivava come 'error' testuale -> Transient -> inconclusivo perpetuo.
    /// Dal segnale strutturato (status 404 del primo fallimento) deve derivare
    /// `model_not_found` (fail conclusivo -> Disqualified).
    #[test]
    fn gateway_404_sul_pin_diventa_model_not_found() {
        let err = gw_error(json!({
            "error": "tutti i provider hanno fallito -> google (google HTTP 404: ...)",
            "code": "PROVIDER_ERROR",
            "details": {
                "primary_cause": "client_error",
                "failures": [{"provider": "google", "class": "client_error",
                               "status": 404, "code": "not_found", "message": "x"}]
            }
        }));
        assert_eq!(
            error_class_from_gateway(&err).as_deref(),
            Some("model_not_found")
        );
    }

    #[test]
    fn gateway_primary_cause_mappata_vince() {
        let err = gw_error(json!({
            "error": "x", "code": "PROVIDER_ERROR",
            "details": { "primary_cause": "empty_completion", "failures": [] }
        }));
        assert_eq!(
            error_class_from_gateway(&err).as_deref(),
            Some("empty_completion")
        );
    }

    #[test]
    fn gateway_client_error_non_404_resta_al_fallback() {
        // Un 400 puo' essere history/schema (colpa della richiesta, non del
        // modello): nessuna classe derivata, si ricade sul testuale.
        let err = gw_error(json!({
            "error": "x", "code": "PROVIDER_ERROR",
            "details": {
                "primary_cause": "client_error",
                "failures": [{"provider": "google", "class": "client_error",
                               "status": 400, "code": "invalid_request_error", "message": "x"}]
            }
        }));
        assert_eq!(error_class_from_gateway(&err), None);
        // E un errore NON-gateway (nessun downcast) idem.
        assert_eq!(error_class_from_gateway(&anyhow::anyhow!("boom")), None);
    }

    #[test]
    fn config_outcome_da_profile_run() {
        // passes >= promote_min -> Pass; fail conclusivi -> FailConclusive;
        // solo inconclusivi -> Inconclusive.
        let pass = profile_run("m", &[], false, 2, 0, 0, 2, None);
        assert_eq!(ConfigOutcome::from_run(&pass), ConfigOutcome::Pass);
        let fail = profile_run("m", &[], false, 1, 1, 0, 2, Some("empty"));
        assert_eq!(ConfigOutcome::from_run(&fail), ConfigOutcome::FailConclusive);
        let inc = profile_run("m", &[], false, 1, 0, 1, 2, None);
        assert_eq!(ConfigOutcome::from_run(&inc), ConfigOutcome::Inconclusive);
    }
    fn soglie() -> TierPriorThresholds {
        // I valori del seed (mig 0599).
        TierPriorThresholds {
            frontier_min_input_cost: 8.0,
            heavy_min_input_cost: 2.0,
            high_min_input_cost: 0.5,
            long_context_tokens: 200_000,
            // Le soglie dell'indice dal seed (mig 0600).
            agentic_index_frontier_min: 45.0,
            agentic_index_heavy_min: 35.0,
            agentic_index_high_min: 25.0,
            agentic_index_medium_min: 10.0,
        }
    }

    fn fatti(cost: Option<f64>, ctx: i64, proven: &[&str]) -> CatalogFacts {
        CatalogFacts {
            input_cost: cost,
            context_window: ctx,
            proven_capabilities: proven.iter().map(|s| s.to_string()).collect(),
            agentic_index: None,
        }
    }

    fn con_indice(idx: f64, cost: Option<f64>) -> CatalogFacts {
        CatalogFacts {
            agentic_index: Some(idx),
            ..fatti(cost, 8192, &[])
        }
    }

    /// L'agentic_index VINCE sul prezzo: MISURA la capacita' agentica, mentre il
    /// prezzo e' il posizionamento commerciale del fornitore.
    ///
    /// I due casi REALI che il solo prezzo sbagliava (misurati sul parco il 16/07):
    /// gpt-5.4-mini costa da heavy (>$2) ma vale 30.2 -> e' 'high', non 'heavy';
    /// claude-opus-4-8 vale 47.2 -> resta 'frontier', il prezzo lo declassava.
    #[test]
    fn l_agentic_index_vince_sul_prezzo() {
        // gpt-5.4-mini: prezzo da heavy, indice da high. Vince l'indice.
        assert_eq!(
            derive_tier_prior(&con_indice(30.2, Some(3.0)), &soglie()),
            Some("high"),
            "un MINI caro non e' heavy: il prezzo diceva heavy, l'indice dice la verita'"
        );
        // claude-opus-4-8: 47.2 -> frontier (il prezzo lo faceva scendere a heavy).
        assert_eq!(derive_tier_prior(&con_indice(47.2, Some(5.0)), &soglie()), Some("frontier"));
        // gpt-5.6-sol: il migliore del parco.
        assert_eq!(derive_tier_prior(&con_indice(54.0, Some(3.0)), &soglie()), Some("frontier"));
        // mistral-large: costa poco E vale poco -> light. Qui prezzo e indice
        // concordano, ed e' il caso in cui l'euristica sul nome aveva ragione.
        assert_eq!(derive_tier_prior(&con_indice(5.5, Some(0.1)), &soglie()), Some("light"));
    }

    /// Senza indice si ricade sul prezzo (61% del parco): un ripiego DICHIARATO,
    /// che batte comunque il nome (misurato: 64 -> 31 inversioni).
    #[test]
    fn senza_indice_il_prior_ricade_sul_prezzo() {
        let f = CatalogFacts { agentic_index: None, ..fatti(Some(3.0), 8192, &[]) };
        assert_eq!(derive_tier_prior(&f, &soglie()), Some("heavy"), "nessun indice -> prezzo");
        // E se manca anche il prezzo, TACE.
        let f = CatalogFacts { agentic_index: None, ..fatti(None, 8192, &[]) };
        assert_eq!(derive_tier_prior(&f, &soglie()), None);
    }

    /// Il prior NON si esprime senza fatti: meglio NULL che una bugia.
    /// E' la differenza fra "non lo so" e "e' medium", che il DEFAULT 'medium'
    /// (rimosso dalla mig 0599) rendeva indistinguibili.
    #[test]
    fn il_prior_tace_se_non_ha_fatti() {
        assert_eq!(derive_tier_prior(&fatti(None, 128_000, &[]), &soglie()), None,
            "prezzo non dichiarato (pricing_state='unknown') -> nessun tier, non 'medium'");
        assert_eq!(derive_tier_prior(&fatti(Some(0.0), 128_000, &[]), &soglie()), None,
            "un costo 0 NON raffinato non e' 'gratis', e' 'non lo so': dedurne 'light'              sarebbe la stessa bugia del nome con un'altra faccia");
    }

    /// Il prezzo e' il segnale che il FORNITORE emette col denaro, non con un
    /// aggettivo nel nome.
    #[test]
    fn il_prior_legge_il_listino_del_fornitore() {
        assert_eq!(derive_tier_prior(&fatti(Some(30.0), 8192, &[]), &soglie()), Some("frontier"));
        assert_eq!(derive_tier_prior(&fatti(Some(3.0), 8192, &[]), &soglie()), Some("heavy"));
        assert_eq!(derive_tier_prior(&fatti(Some(1.0), 8192, &[]), &soglie()), Some("high"));
        assert_eq!(derive_tier_prior(&fatti(Some(0.1), 8192, &[]), &soglie()), Some("light"));
    }

    /// La finestra dichiarata alza di UN gradino, MAI a frontier: e' un numero
    /// del fornitore, non la prova che il modello la sappia usare — quella la da'
    /// solo il probe agentic_longctx.
    #[test]
    fn la_finestra_dichiarata_non_regala_il_frontier() {
        // light + 1M di finestra -> medium, non frontier.
        assert_eq!(derive_tier_prior(&fatti(Some(0.1), 1_000_000, &[]), &soglie()), Some("medium"));
        // heavy + finestra enorme resta heavy: il gradino non scavalca la misura.
        assert_eq!(derive_tier_prior(&fatti(Some(3.0), 1_000_000, &[]), &soglie()), Some("heavy"));
    }

    /// Il reasoning PROVATO (non dichiarato) e' l'unico segnale del prior che
    /// viene da una misura nostra.
    #[test]
    fn il_prior_usa_il_reasoning_provato_non_quello_dichiarato() {
        assert_eq!(
            derive_tier_prior(&fatti(Some(0.1), 8192, &["reasoning"]), &soglie()),
            Some("medium"),
            "light + reasoning PROVATO -> medium"
        );
        // Il dichiarato non entra: `proven_capabilities` sono i qualified_capabilities.
        assert_eq!(derive_tier_prior(&fatti(Some(0.1), 8192, &["chat"]), &soglie()), Some("light"));
    }

    fn run(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
        ProfileRun {
            profile_key: key.to_string(),
            grants: vec![],
            is_blocking: false,
            passes,
            conclusive_fails: 4 - passes,
            inconclusive: 0,
            promote_min,
            first_fail_reason: None,
        }
    }

    /// La banda PIU' ALTA certificata vince: e' criterion-referenced, un heavy ha
    /// DIMOSTRATO qualcosa che un medium non ha fatto.
    #[test]
    fn il_measured_prende_la_banda_piu_alta_certificata() {
        let runs = vec![
            (run("chat_smoke", 4, 3), Some("light".to_string())),
            (run("agentic_real", 4, 3), Some("medium".to_string())),
            (run("agentic_chain", 3, 3), Some("high".to_string())),
            (run("agentic_recovery", 1, 3), Some("heavy".to_string())), // NON superata
        ];
        assert_eq!(derive_tier_measured(&runs, None, 2), Some("high".to_string()),
            "3/4 su agentic_chain promuove a high; agentic_recovery con 1/4 non certifica heavy");
    }

    /// Un profilo che non certifica un tier (tool_smoke) non influenza la banda.
    #[test]
    fn un_profilo_senza_banda_non_influenza_il_tier() {
        let runs = vec![
            (run("tool_smoke", 4, 3), None),
            (run("chat_smoke", 4, 3), Some("light".to_string())),
        ];
        assert_eq!(derive_tier_measured(&runs, None, 2), Some("light".to_string()));
    }

    /// L'ISTERESI: la soglia per CONSERVARE una banda gia' acquisita e' piu'
    /// bassa di quella per conquistarla. Senza, un modello oscillerebbe di fascia
    /// a ogni riqualifica e destabilizzerebbe il routing.
    #[test]
    fn l_isteresi_conserva_la_banda_acquisita_ma_non_ne_regala_di_nuove() {
        // 2 pass su 4: sotto promote_min (3) ma sopra hold_min (2).
        let runs = vec![(run("agentic_recovery", 2, 3), Some("heavy".to_string()))];
        // Chi e' GIA' heavy la mantiene (2 >= hold_min).
        assert_eq!(derive_tier_measured(&runs, Some("heavy"), 2), Some("heavy".to_string()),
            "banda acquisita: si conserva con hold_min, la conservazione non ha              bisogno di evidenza nuova");
        // Chi e' medium NON la conquista (2 < promote_min): serve piu' evidenza
        // per salire che per restare.
        assert_eq!(derive_tier_measured(&runs, Some("medium"), 2), None,
            "banda NUOVA: serve promote_min. Il gap fra le due soglie E' l'isteresi");
    }
    /// IL CERCHIO SI CHIUDE: la batteria SCRIVE il tier misurato, e la curatela
    /// dell'admin vince sempre.
    ///
    /// E' l'ultimo anello: senza, `derive_tier_measured` calcolava una banda che
    /// nessuno scriveva, e il tier restava per sempre al `facts_prior` (una stima
    /// sul prezzo) anche dopo che la batteria aveva DIMOSTRATO la capacita'.
    #[sqlx::test]
    async fn la_batteria_scrive_il_tier_misurato_ma_non_tocca_la_curatela(pool: PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "ALTER TABLE ai_price_catalog                ADD COLUMN tier_source TEXT,                ADD COLUMN capability_locked BOOLEAN NOT NULL DEFAULT false,                ADD COLUMN capability_source TEXT NOT NULL DEFAULT 'auto',                ADD COLUMN qualified_at TIMESTAMPTZ,                ADD COLUMN qualification_suite_version INT,                ADD COLUMN qualification_reason TEXT,                ADD COLUMN qualification_evidence_id BIGINT,                ADD COLUMN qualification_started_at TIMESTAMPTZ,                ADD COLUMN qualification_attempts INT NOT NULL DEFAULT 0,                ADD COLUMN qualification_backoff_until TIMESTAMPTZ",
        )
        .execute(&pool)
        .await
        .expect("colonne");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source) VALUES              ('p', 'stimato', 'medium', 'facts_prior'),              ('p', 'curato',  'light',  'manual')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let derived = Derived {
            state: DerivedState::Qualified,
            qualified_capabilities: vec!["chat".into()],
            reason: "suite_passed".into(),
            thinking: None,
            measured_tier: Some("heavy".to_string()),
        };
        for model in ["stimato", "curato"] {
            apply_derived(&pool, "p", model, 2, &derived, None, 30, 24).await;
        }

        let stimato: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='stimato'",
        )
        .fetch_one(&pool)
        .await
        .expect("stimato");
        assert_eq!(
            stimato,
            (Some("heavy".into()), Some("measured".into())),
            "la banda DIMOSTRATA sostituisce la stima sul prezzo, e tier_source lo dice"
        );

        let curato: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='curato'",
        )
        .fetch_one(&pool)
        .await
        .expect("curato");
        assert_eq!(
            curato,
            (Some("light".into()), Some("manual".into())),
            "la CURATELA dell'admin vince sempre: la batteria non la tocca"
        );
    }

    /// Nessuna banda certificata -> il tier NON viene toccato: resta il prior.
    /// (Il difetto opposto sarebbe azzerare il tier a ogni giro senza bande.)
    #[sqlx::test]
    async fn senza_banda_certificata_il_tier_resta_il_prior(pool: PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "ALTER TABLE ai_price_catalog                ADD COLUMN tier_source TEXT,                ADD COLUMN capability_locked BOOLEAN NOT NULL DEFAULT false,                ADD COLUMN capability_source TEXT NOT NULL DEFAULT 'auto',                ADD COLUMN qualified_at TIMESTAMPTZ,                ADD COLUMN qualification_suite_version INT,                ADD COLUMN qualification_reason TEXT,                ADD COLUMN qualification_evidence_id BIGINT,                ADD COLUMN qualification_started_at TIMESTAMPTZ,                ADD COLUMN qualification_attempts INT NOT NULL DEFAULT 0,                ADD COLUMN qualification_backoff_until TIMESTAMPTZ",
        )
        .execute(&pool)
        .await
        .expect("colonne");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source)              VALUES ('p', 'm', 'high', 'facts_prior')",
        )
        .execute(&pool)
        .await
        .expect("seed");
        let derived = Derived {
            state: DerivedState::Qualified,
            qualified_capabilities: vec!["chat".into()],
            reason: "suite_passed".into(),
            thinking: None,
            measured_tier: None,
        };
        apply_derived(&pool, "p", "m", 2, &derived, None, 30, 24).await;
        let r: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='m'",
        )
        .fetch_one(&pool)
        .await
        .expect("m");
        assert_eq!(
            r,
            (Some("high".into()), Some("facts_prior".into())),
            "nessuna banda certificata: il tier resta il prior, non viene azzerato"
        );
    }
}
