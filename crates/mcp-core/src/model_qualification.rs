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

// Qui viveva `error_class_from_gateway`, il ponte structured->error_class del
// qualificatore, con dentro la regola "404 sul pin = modello inesistente".
// Rimosso perche' NON POTEVA GIRARE: lo chiamava solo il ramo `Ok(Err(_))` di
// `single_attempt`, e `generate_agent_turn_with_thinking` non ritorna mai `Err`
// su un fallimento del provider — lo impacchetta in `Ok(turn)` con `error_class`
// gia' dentro. Quel ramo vede solo errori LOCALI (bridge non configurato, JSON
// invalido), che non sono `GatewayHttpError`: il downcast falliva sempre.
//
// Codice morto con tre test verdi che lo chiamavano a mano, mentre il sintomo che
// diceva di aver curato era vivo nel DB: 28/28 evidence di `agentic_longctx` e le
// righe google con `error_class='error'` su 404 conclamati. Il ponte ora sta in
// `neural_client::structured_error_class`, dove l'errore passa davvero, e la mappa
// status->classe e' in `provider_error_classifier::client_error_class_from_status`
// (punto unico, regola L).

/// Il fatto piantato nella history del profilo `long_context`. COSTANTE e
/// deterministico: il checker deve poterlo ricalcolare senza che il needle
/// viaggi nel predicato o nel system prompt (dove il modello lo leggerebbe invece
/// di cercarlo). Non e' un segreto: e' un ago, e il pagliaio e' la history.
pub(crate) const LONG_CTX_NEEDLE: &str = "NX7K2P9QW4";

/// Costruisce la history del `long_context`: ~`chars` di riempimento con UNA riga
/// che porta il needle, piantata a META'. PURA.
///
/// A meta' e non in fondo: molti modelli hanno un forte bias di recency e
/// ritroverebbero un fatto in coda senza usare davvero la finestra — il test
/// misurerebbe la posizione, non la capacita'. Il riempimento e' prosa neutra e
/// ripetitiva, senza altri codici che possano confondere il checker.
pub(crate) fn build_needle_history(chars: usize) -> String {
    const RIGA: &str = "Nota operativa di archivio: la pratica e' stata protocollata \
                        e non richiede ulteriori azioni da parte dell'ufficio.\n";
    let mut testo = String::with_capacity(chars + 64);
    let meta = chars / 2;
    while testo.len() < meta {
        testo.push_str(RIGA);
    }
    testo.push_str(&format!("CODICE-PRATICA: {LONG_CTX_NEEDLE}\n"));
    while testo.len() < chars {
        testo.push_str(RIGA);
    }
    testo
}

/// I segnali GREZZI di un turno di probe. Separare la LETTURA dal GIUDIZIO tiene
/// in un solo posto il contratto col produttore del Value (vedi `content` sotto),
/// che e' esattamente il punto in cui la batteria si era rotta.
struct TurnSignals {
    stop_reason: String,
    tool_call_count: i64,
    content_chars: i64,
    error_class: Option<String>,
}

impl TurnSignals {
    /// Compone l'esito portandosi dietro i segnali misurati. I campi di misura
    /// accompagnano OGNI verdetto: senza, un fallimento non sarebbe diagnosticabile.
    fn outcome(self, pass: bool, inconclusive: bool, reason: String) -> AttemptOutcome {
        AttemptOutcome {
            pass,
            inconclusive,
            reason,
            error_class: self.error_class,
            tool_call_count: self.tool_call_count,
            content_chars: self.content_chars,
            stop_reason: self.stop_reason,
        }
    }
}

fn read_turn_signals(turn: &Value) -> TurnSignals {
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
    TurnSignals {
        stop_reason: turn
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        tool_call_count: turn
            .get("tool_use_blocks")
            .and_then(Value::as_array)
            .map(|a| a.len() as i64)
            .unwrap_or(0),
        content_chars: turn
            .get("content")
            .and_then(Value::as_str)
            .map(|s| s.trim().chars().count() as i64)
            .unwrap_or(0),
        error_class: turn
            .get("error_class")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    }
}

/// La classificazione canonica decide se l'errore e' colpa del modello
/// (conclusivo) o no (inconclusivo). Punto unico riusato dal probe (regola L).
fn verdict_from_error_class(ec: &str) -> (bool, bool, String) {
    use crate::model_health_probe::Classification;
    match crate::model_health_probe::classification_from_error_class(ec) {
        Classification::ModelSpecific(kind, _) => (false, false, format!("error_class:{kind}")),
        Classification::ProviderWide(kind, _) => (false, true, format!("provider_wide:{kind}")),
        Classification::Transient(kind, _) => (false, true, format!("transient:{kind}")),
        Classification::Ok => (false, false, format!("error_class:{ec}")),
    }
}

/// Il predicato del profilo (soglie dal DB, regola G): `None` = superato.
fn predicate_fail_reason(
    turn: &Value,
    predicate: &Value,
    sig: &TurnSignals,
    latency_ms: i64,
) -> Option<String> {
    let min_tool_calls = predicate
        .get("min_tool_calls")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let min_content_chars = predicate
        .get("min_content_chars")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if sig.tool_call_count < min_tool_calls {
        let coda = if sig.stop_reason.is_empty() {
            String::new()
        } else {
            format!(":{}", sig.stop_reason)
        };
        return Some(format!(
            "no_tool_call:{}<{min_tool_calls}{coda}",
            sig.tool_call_count
        ));
    }
    if sig.content_chars < min_content_chars {
        return Some(format!(
            "empty_content:{}<{min_content_chars}",
            sig.content_chars
        ));
    }
    if needle_missing(turn, predicate) {
        return Some("needle_not_found".to_string());
    }
    match predicate.get("max_latency_ms").and_then(Value::as_i64) {
        Some(cap) if latency_ms > cap => Some(format!("latency:{latency_ms}>{cap}")),
        _ => None,
    }
}

/// FRONTIER (`long_context`): il modello deve aver RITROVATO il fatto piantato a
/// meta' di 100k. Verifica DETERMINISTICA sul testo esatto (regola M), non un
/// giudizio sulla qualita' della risposta: o il codice c'e', o non c'e'. Una
/// finestra dichiarata grande non prova nulla se il modello non la usa.
fn needle_missing(turn: &Value, predicate: &Value) -> bool {
    predicate
        .get("requires_needle")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !turn
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains(LONG_CTX_NEEDLE)
}

/// Valuta UN turno di probe contro il `pass_predicate` del profilo. PURA.
pub(crate) fn evaluate_attempt(turn: &Value, predicate: &Value, latency_ms: i64) -> AttemptOutcome {
    let sig = read_turn_signals(turn);
    if let Some(ec) = sig.error_class.clone() {
        let (pass, inconclusive, reason) = verdict_from_error_class(&ec);
        return sig.outcome(pass, inconclusive, reason);
    }
    // stop_reason=error senza classe: inconclusivo (stessa prudenza del probe).
    if sig.stop_reason == "error" {
        return sig.outcome(false, true, "stop_reason_error".into());
    }
    match predicate_fail_reason(turn, predicate, &sig, latency_ms) {
        None => sig.outcome(true, false, "ok".into()),
        Some(reason) => sig.outcome(false, false, reason),
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
    /// tier `synced` e non viene toccato.
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

/// Le soglie del tier `synced`, dal DB (regola G, mig 0600; il prezzo e' uscito
/// dalla classificazione con la mig 0608).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TierPriorThresholds {
    /// Soglie sull'`agentic_index`. ASSOLUTE su un indice VERSIONATO (AA v4.1 ha
    /// cambiato 3 benchmark su 9 rispetto a v4.0): per questo stanno nel DB e
    /// vanno riviste a ogni cambio di metodologia.
    pub agentic_index_frontier_min: f64,
    pub agentic_index_heavy_min: f64,
    pub agentic_index_high_min: f64,
    pub agentic_index_medium_min: f64,
}

/// TIER `synced` DALLA CLASSIFICAZIONE ESTERNA. PURA.
///
/// E' il SEME onesto in attesa della misura, non una verita': appena la
/// batteria certifica una banda, `measured` lo sostituisce. L'agentic_index
/// MISURA la capacita' agentica (Agents 34% + Coding 24% dell'Intelligence
/// Index, su harness con tool): e' il nostro uso esatto — a differenza del NOME
/// (il token `mini` copre intelligence 6.8-50.2) e del PREZZO (posizionamento
/// commerciale: gpt-5.4-mini a >$2 diventava 'heavy' con agentic 30.2, e
/// claude-opus-4-8 a 47.2 veniva DECLASSATO da frontier). Entrambi sono usciti
/// dalla classificazione: il nome con la mig 0599, il prezzo con la 0608.
fn tier_from_agentic_index(idx: f64, t: &TierPriorThresholds) -> &'static str {
    if idx >= t.agentic_index_frontier_min {
        "frontier"
    } else if idx >= t.agentic_index_heavy_min {
        "heavy"
    } else if idx >= t.agentic_index_high_min {
        "high"
    } else if idx >= t.agentic_index_medium_min {
        "medium"
    } else {
        "light"
    }
}

/// `None` = indice assente o stantio (il chiamante lo azzera oltre
/// `max_age_hours`: la fonte e' undocumented e puo' sparire — un indice vecchio
/// non deve passare per fresco). Il tier resta NULL e il sistema DICE che non lo
/// sa, invece di scrivere 'medium' e far finta (regola G: niente fallback
/// magico). Un tier NULL non e' pericoloso: col gate acceso un modello non
/// qualificato e' gia' fuori dal pool agentico, e la batteria gli dara' una
/// banda `measured` al primo giro utile.
pub(crate) fn derive_tier_prior(
    agentic_index: Option<f64>,
    t: &TierPriorThresholds,
) -> Option<&'static str> {
    agentic_index.map(|idx| tier_from_agentic_index(idx, t))
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
/// `current_measured` = la banda gia' GUADAGNATA dalla batteria (solo se
/// `tier_source='measured'`), per l'isteresi. `current_catalog` = il tier scritto
/// nel catalogo da QUALUNQUE fonte, che serve al guard anti-declassamento.
/// `None` -> nessuna banda certificata: il chiamante ricade sul prior.
pub(crate) fn derive_tier_measured(
    runs: &[(ProfileRun, Option<String>)],
    current_measured: Option<&str>,
    current_catalog: Option<&str>,
    hold_min: u32,
) -> Option<String> {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let mut migliore: Option<&str> = None;
    for (run, certifies) in runs {
        let Some(banda) = certifies.as_deref() else {
            continue; // il profilo non certifica un tier (es. tool_smoke)
        };
        if run.passes >= soglia_banda(run, banda, current_measured, hold_min)
            && !scala_rotta_sotto(runs, banda)
            && migliore.is_none_or(|m| tier_rank(banda) > tier_rank(m))
        {
            migliore = Some(banda);
        }
    }
    // IL SILENZIO NON DECLASSA. Se la banda certificata e' piu' bassa di quella
    // che il catalogo ha gia', si scende SOLO fino alla piu' alta che nessuno ha
    // negato — non fino a `migliore`. Senza questo, un parco intero crolla a
    // `medium` appena la batteria smette di poter certificare le bande alte: le
    // misure che mancano verrebbero lette come bocciature. E' misurato: al
    // 2026-07-17, con `agentic_chain`/`agentic_recovery` non implementati, 14
    // modelli su 29 sarebbero scesi di fascia — a partire da grok-4.5 (indice
    // 45.7, il migliore del parco), che nessun profilo ha mai contestato.
    match (migliore, current_catalog) {
        (Some(m), Some(c)) if tier_rank(m) < tier_rank(c) => {
            match banda_piu_alta_non_negata(runs, c) {
                // Nessuno ha contestato la banda che il modello ha gia': non si
                // declassa. E non si riscrive nemmeno `c` come `measured`, che
                // sarebbe un riciclo del prior — l'indice esterno si farebbe
                // certificare dalla batteria senza aver superato un solo probe, e
                // `measured` batte `synced`: l'autocorrezione dell'indice si
                // congelerebbe per sempre.
                Some(p) if p == c => None,
                // La banda del catalogo E' stata negata: si scende, ma solo fino
                // alla prima che nessuno ha contestato. `p >= m` per costruzione
                // (`m` e' certificato, quindi non e' negato).
                Some(p) => Some(p.to_string()),
                // `c` non appartiene alla scala (refuso, valore legacy): non e' un
                // gradino da cui scendere. Vale la banda certificata.
                None => Some(m.to_string()),
            }
        }
        (m, _) => m.map(str::to_string),
    }
}

/// I pass necessari perche' `banda` valga, con l'isteresi.
///
/// Asimmetrica per costruzione: conservare costa meno che conquistare, e il gap fra
/// le due soglie E' l'isteresi (senza, un modello oscillerebbe di fascia a ogni
/// riqualifica e destabilizzerebbe il routing).
///
/// La soglia bassa spetta solo a chi la banda l'ha GUADAGNATA dalla batteria
/// (`current_measured`), mai a un prior dell'indice: leggendo il tier del catalogo
/// senza guardarne la fonte, un modello con synced=heavy si teneva heavy con 2/4
/// invece di 3/4 — l'indice esterno si autocertificava con la nostra clemenza.
fn soglia_banda(
    run: &ProfileRun,
    banda: &str,
    current_measured: Option<&str>,
    hold_min: u32,
) -> u32 {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let acquisita = current_measured.is_some_and(|c| tier_rank(banda) <= tier_rank(c));
    if acquisita {
        hold_min.min(run.promote_min)
    } else {
        run.promote_min
    }
}

/// `true` se sotto `banda` c'e' un gradino NEGATO: allora `banda` non vale, perche'
/// la scala dev'essere una scala.
///
/// `agentic_longctx` certifica `frontier` (il vertice) ed e' l'unico profilo alto
/// implementato: senza questo, un modello che fallisce la catena e il recupero ma
/// ritrova l'ago si prende `frontier`, scavalcando i gradini che non ha salito.
/// Il test e' la NON-NEGAZIONE, non la certificazione positiva: pretendere che tutte
/// le bande inferiori passino congelerebbe quelle alte ogni volta che la rete cade
/// su un anello intermedio.
fn scala_rotta_sotto(runs: &[(ProfileRun, Option<String>)], banda: &str) -> bool {
    bande_inferiori(banda)
        .iter()
        .any(|b| banda_negata(runs, b))
}

/// `true` se il giro NEGA `banda` con evidenza attribuibile al MODELLO: il profilo
/// che la certifica e' girato, e' rimasto sotto la sua soglia e ha almeno un
/// fallimento CONCLUSIVO.
///
/// Un profilo mai girato, non costruibile (kind non implementato) o solo
/// inconclusivo NON e' una negazione: e' silenzio. La distinzione e' tutta qui —
/// "non l'ho provato" e "l'ha fallito" sono cose opposte, e trattarle uguale
/// declassa i modelli per i difetti della batteria invece che per i loro.
fn banda_negata(runs: &[(ProfileRun, Option<String>)], banda: &str) -> bool {
    runs.iter().any(|(r, b)| {
        b.as_deref() == Some(banda) && !r.passed() && r.conclusive_fails > 0
    })
}

/// Le bande sotto `banda` nella scala canonica (punto unico `PERFORMANCE_TIERS`,
/// regola L: la scala non si riscrive a mano).
fn bande_inferiori(banda: &str) -> Vec<&'static str> {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let r = tier_rank(banda);
    nexus_types::tiers::PERFORMANCE_TIERS
        .iter()
        .copied()
        .filter(|b| tier_rank(b) < r)
        .collect()
}

/// La banda del vocabolario canonico che corrisponde a `t`. `None` = valore fuori
/// scala (refuso, tier legacy): il chiamante non lo tratta come un gradino.
fn banda_canonica(t: &str) -> Option<&'static str> {
    nexus_types::tiers::PERFORMANCE_TIERS
        .iter()
        .copied()
        .find(|b| b.eq_ignore_ascii_case(t.trim()))
}

/// Scendendo da `tetto`, la prima banda che il giro non ha negato. E' il pavimento
/// del declassamento: si perde un gradino per volta, e solo dove c'e' una bocciatura
/// vera. `None` = `tetto` non e' una banda della scala.
fn banda_piu_alta_non_negata(
    runs: &[(ProfileRun, Option<String>)],
    tetto: &str,
) -> Option<&'static str> {
    let mut scesa = banda_canonica(tetto)?;
    loop {
        if !banda_negata(runs, scesa) {
            return Some(scesa);
        }
        match bande_inferiori(scesa).last() {
            Some(b) => scesa = b,
            // Negata anche la banda piu' bassa: non si scende sotto la scala.
            None => return Some(scesa),
        }
    }
}

/// Verdetto NEGATIVO della batteria, se c'e'. Distingue le due cause, che non
/// vanno confuse: un fallimento CONCLUSIVO e' del modello (squalifica), mentre
/// il non raggiungimento della soglia per troppi inconclusivi e' del giro (rete,
/// provider) e non gli e' attribuibile.
fn blocking_verdict(runs: &[ProfileRun]) -> Option<Derived> {
    for r in runs {
        if r.is_blocking && !r.passed() && r.conclusive_fails > 0 {
            return Some(Derived {
                state: DerivedState::Disqualified,
                qualified_capabilities: Vec::new(),
                reason: format!(
                    "{}:{}",
                    r.profile_key,
                    r.first_fail_reason.as_deref().unwrap_or("failed")
                ),
                thinking: None,
                measured_tier: None,
            });
        }
    }
    runs.iter()
        .any(|r| r.is_blocking && !r.passed())
        .then(|| Derived {
            state: DerivedState::Inconclusive,
            qualified_capabilities: Vec::new(),
            reason: "inconclusive_round".into(),
            thinking: None,
            measured_tier: None,
        })
}

/// Il PROVATO: unione dei grants dei profili passati, piu' i tag dichiarati che
/// la suite v1 non misura (vedi MEASURED_V1) — su quelli il dichiarato resta
/// l'unica evidenza disponibile.
fn union_proven_capabilities(declared: &[String], runs: &[ProfileRun]) -> Vec<String> {
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
    caps
}

/// PUNTO UNICO PURO (regola L): l'evidenza diventa stato + capability PROVATE.
/// `declared` = jsonb `capabilities` della riga (il dichiarato).
pub(crate) fn derive_capabilities(declared: &[String], runs: &[ProfileRun]) -> Derived {
    if let Some(negativo) = blocking_verdict(runs) {
        return negativo;
    }
    Derived {
        state: DerivedState::Qualified,
        qualified_capabilities: union_proven_capabilities(declared, runs),
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
        "SELECT profile_key, suite_version, kind, is_blocking, certifies_tier, \
                applies_when, grants, payload, pass_predicate \
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
/// Gli schemi REALI dal catalogo statico (punto unico, regola L): la prova usa
/// gli artefatti di produzione, non repliche giocattolo — un tool finto
/// misurerebbe il nostro mock, non il modello.
fn resolve_probe_tools(profile: &ProbeProfile) -> Result<Value, String> {
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
    let tools = crate::agent_tools::subagent_native::build_tools_json(&tool_names);
    if tools.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err("nessun tool della whitelist esiste nel catalogo statico".into());
    }
    Ok(tools)
}

/// Il system prompt REALE dal DB (regola G): la figura provata e' quella che gira
/// in produzione.
async fn resolve_probe_system(db: &PgPool, profile: &ProbeProfile) -> Result<String, String> {
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
    system
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("template '{template_key}' assente o vuoto"))
}

/// Filler DETERMINISTICO che simula la history reale di una figura (contesto
/// progetto + richiesta): dimensiona il CARICO, non il contenuto.
fn build_realistic_messages(profile: &ProbeProfile) -> String {
    let history_chars = profile
        .payload
        .get("history_chars")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as usize;
    let filler_unit = "Contesto di progetto: applicazione web con autenticazione JWT, \
                       database Postgres, servizi containerizzati e pipeline di build. ";
    let filler: String = filler_unit.chars().cycle().take(history_chars).collect();
    json!([
        { "role": "user",
          "content": format!("Materiale di contesto per l'analisi:\n{filler}") },
        { "role": "assistant",
          "content": "Ho letto il contesto. Procedo con l'analisi richiesta." },
        { "role": "user",
          "content": "Analizza i rischi dell'autenticazione del progetto: inizia \
                      ispezionando i file rilevanti con i tool a disposizione, poi \
                      dichiara il tuo parere strutturato." }
    ])
    .to_string()
}

/// FRONTIER: reggere il contesto lungo con retrieval VERIFICABILE. Non e' "quanto
/// e' grande la finestra dichiarata" (quello e' un numero del fornitore, e infatti
/// il prior lo usa solo per un gradino): e' se il modello la USA davvero. Un fatto
/// piantato a meta' della history, e il checker verifica che lo ritrovi ALLA
/// LETTERA — un needle-in-haystack.
///
/// Single-turn: non serve il loop conversazionale (che invece serve a tool_chain e
/// tool_recovery). Il needle e' DETERMINISTICO e verificabile programmaticamente,
/// mai un giudizio sulla prosa (regola M).
fn build_long_context_request(profile: &ProbeProfile) -> (String, String, String) {
    let history_chars = profile
        .payload
        .get("history_chars")
        .and_then(Value::as_i64)
        .unwrap_or(100_000)
        .clamp(1_000, 400_000) as usize;
    let messages = json!([
        { "role": "user", "content": build_needle_history(history_chars) },
        { "role": "user", "content":
            "Nel testo sopra c'e' esattamente una riga che inizia con \
             'CODICE-PRATICA:'. Rispondi SOLO con il codice che segue i due \
             punti, senza altro testo." }
    ]);
    // Il NEEDLE non compare MAI nel system prompt: il modello deve trovarlo nella
    // history, non leggerlo nelle istruzioni. Metterlo qui renderebbe il test una
    // formalita' che chiunque supera — misurerebbe la nostra ingenuita', non il
    // modello.
    (
        "[]".to_string(),
        messages.to_string(),
        "Sei in una verifica di recupero da contesto lungo. Rispondi in \
         modo conciso, senza commenti."
            .to_string(),
    )
}

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
            let tools = resolve_probe_tools(profile)?;
            let system = resolve_probe_system(db, profile).await?;
            Ok((
                tools.to_string(),
                build_realistic_messages(profile),
                system,
            ))
        }
        "long_context" => Ok(build_long_context_request(profile)),
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
/// Le soglie di UN profilo, gia' clampate. Vengono dal payload (regola G) e
/// restano fisse per tutte le passate su quel profilo.
#[derive(Clone, Copy)]
struct ProfileParams {
    repeat: u32,
    timeout_s: u64,
    max_tokens: u32,
    promote_min: u32,
}

/// Il contesto STABILE di una batteria su un modello: fra una passata e l'altra
/// cambiano solo la configurazione thinking e l'etichetta. Raggrupparlo qui
/// toglie i 13 parametri posizionali (e il `too_many_arguments` che li copriva)
/// dalle tre chiamate di `qualify_one`.
struct ProbeCtx<'a> {
    orchestrator: &'a Orchestrator,
    db: &'a PgPool,
    provider: &'a str,
    model: &'a str,
    profile: &'a ProbeProfile,
    /// `(tools_json, messages_json, system_text)`, come lo costruisce
    /// `build_profile_request`.
    request: &'a (String, String, String),
    params: ProfileParams,
}

impl ProbeCtx<'_> {
    /// La classe d'errore degli errori LOCALI di questo ramo.
    ///
    /// Qui non arrivano fallimenti del provider — quelli tornano dentro un
    /// `Ok(turn)` gia' classificati alla fonte da `structured_error_class`. Questo
    /// ramo vede solo cio' che rompe PRIMA della chiamata: bridge del gateway non
    /// configurato, `messages_json`/`tools_json` invalidi. Non essendo
    /// `GatewayHttpError`, non hanno segnale strutturato da leggere: resta il
    /// classificatore testuale, che qui e' appropriato perche' il testo e' NOSTRO
    /// (lo formatta `generate_agent_turn`), non prosa di un provider che puo'
    /// riscriverla quando vuole.
    async fn error_class_for(&self, e: &anyhow::Error) -> String {
        self.orchestrator
            .neural
            .classify_error(&e.to_string(), self.provider)
            .await
    }

    /// UN tentativo: chiama il modello e ne giudica il turno. La latenza e'
    /// misurata qui perche' e' parte dell'esito (il predicato puo' bocciare su
    /// `max_latency_ms`).
    async fn single_attempt(
        &self,
        thinking: Option<crate::nexus_gateway::GwThinkingConfig>,
    ) -> (AttemptOutcome, i64) {
        let (tools_json, messages_json, system_text) = self.request;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(self.params.timeout_s),
            self.orchestrator
                .neural
                .generate_agent_turn_with_thinking(
                    self.provider,
                    self.model,
                    messages_json,
                    tools_json,
                    self.params.max_tokens,
                    system_text,
                    thinking,
                ),
        )
        .await;
        let latency_ms = started.elapsed().as_millis() as i64;
        let outcome = match result {
            Ok(Ok(turn)) => evaluate_attempt(&turn, &self.profile.pass_predicate, latency_ms),
            Ok(Err(e)) => {
                let ec = self.error_class_for(&e).await;
                evaluate_attempt(
                    &json!({ "error_class": ec }),
                    &self.profile.pass_predicate,
                    latency_ms,
                )
            }
            Err(_elapsed) => AttemptOutcome {
                pass: false,
                inconclusive: true,
                reason: format!("probe_timeout:{}s", self.params.timeout_s),
                error_class: None,
                tool_call_count: 0,
                content_chars: 0,
                stop_reason: String::new(),
            },
        };
        (outcome, latency_ms)
    }
}

impl ProfileRun {
    /// Contabilizza UN esito. L'inconclusivo NON e' un fallimento: e' un giro non
    /// attribuibile al modello, e tenerli distinti e' cio' che impedisce alla
    /// batteria di squalificare per colpa della rete.
    fn tally(&mut self, outcome: &AttemptOutcome) {
        if outcome.inconclusive {
            self.inconclusive += 1;
        } else if outcome.pass {
            self.passes += 1;
        } else {
            self.conclusive_fails += 1;
            if self.first_fail_reason.is_none() {
                self.first_fail_reason = Some(outcome.reason.clone());
            }
        }
    }
}

async fn run_profile_attempts(
    ctx: &ProbeCtx<'_>,
    thinking: Option<crate::nexus_gateway::GwThinkingConfig>,
    label: &str,
    last_evidence: &mut Option<i64>,
) -> ProfileRun {
    let mut run = ProfileRun {
        profile_key: ctx.profile.profile_key.clone(),
        grants: ctx.profile.grants.clone(),
        is_blocking: ctx.profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: 0,
        promote_min: ctx.params.promote_min,
        first_fail_reason: None,
    };
    for attempt in 1..=ctx.params.repeat {
        let (mut outcome, latency_ms) = ctx.single_attempt(thinking.clone()).await;
        if !label.is_empty() {
            outcome.reason = format!("{label}{}", outcome.reason);
        }
        if let Some(id) = insert_evidence(
            ctx.db,
            ctx.provider,
            ctx.model,
            ctx.profile,
            attempt as i32,
            latency_ms,
            &outcome,
        )
        .await
        {
            *last_evidence = Some(id);
        }
        run.tally(&outcome);
    }
    run
}

/// Le soglie del profilo, clampate. I default vivono qui e non nel DB perche'
/// sono limiti di SICUREZZA del probe (un `repeat: 500` da payload non deve
/// poter partire), non configurazione di routing.
fn profile_params(profile: &ProbeProfile) -> ProfileParams {
    let repeat = profile
        .payload
        .get("repeat")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 5) as u32;
    ProfileParams {
        repeat,
        timeout_s: profile
            .payload
            .get("timeout_s")
            .and_then(Value::as_i64)
            .unwrap_or(90)
            .clamp(10, 300) as u64,
        max_tokens: profile
            .payload
            .get("max_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(512)
            .clamp(16, 16384) as u32,
        promote_min: profile
            .pass_predicate
            .get("promote_min_passes")
            .and_then(Value::as_i64)
            .map(|n| n.clamp(1, repeat as i64) as u32)
            .unwrap_or(repeat),
    }
}

/// Profilo non costruibile: il giro e' inconclusivo, non fallito. La differenza
/// e' sostanziale — un profilo che non si costruisce e' un difetto NOSTRO, e
/// addebitarlo al modello lo squalificherebbe a torto. Il warn sta qui perche' e'
/// parte del rendere l'inconclusivo VISIBILE: un giro che non misura nulla in
/// silenzio e' indistinguibile da un giro riuscito.
fn unbuildable_run(
    profile: &ProbeProfile,
    params: &ProfileParams,
    provider: &str,
    model: &str,
    reason: &str,
) -> ProfileRun {
    tracing::warn!(
        provider = %provider,
        model = %model,
        profile = %profile.profile_key,
        reason = %reason,
        "model_qualification: profilo non costruibile -> inconclusivo"
    );
    ProfileRun {
        profile_key: profile.profile_key.clone(),
        grants: profile.grants.clone(),
        is_blocking: profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: params.repeat,
        promote_min: params.promote_min,
        first_fail_reason: None,
    }
}

/// FASE 5: la matrice PROVA il modello in DUE configurazioni thinking esplicite
/// (off e native) e DERIVA agentic_thinking_policy dai fatti — mai ereditare la
/// policy del catalog che stiamo derivando.
async fn run_thinking_matrix(
    ctx: &ProbeCtx<'_>,
    last_evidence: &mut Option<i64>,
) -> (ProfileRun, ProfileRun, Option<(&'static str, bool)>) {
    let budget = ctx
        .profile
        .payload
        .get("thinking_budget_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(2048)
        .clamp(256, 32768) as u32;
    let off = run_profile_attempts(
        ctx,
        Some(crate::nexus_gateway::GwThinkingConfig {
            enabled: false,
            budget_tokens: None,
            mandatory: false,
        }),
        "off:",
        last_evidence,
    )
    .await;
    let native = run_profile_attempts(
        ctx,
        Some(crate::nexus_gateway::GwThinkingConfig {
            enabled: true,
            budget_tokens: Some(budget),
            mandatory: true,
        }),
        "native:",
        last_evidence,
    )
    .await;
    let policy =
        derive_thinking_policy(ConfigOutcome::from_run(&off), ConfigOutcome::from_run(&native));
    (off, native, policy)
}

/// LA BANDA MISURATA (mig 0599). Solo su un esito QUALIFIED: una squalifica o un
/// giro inconclusivo non misurano nulla, e un tier scritto su un esito non
/// attribuibile al modello sarebbe la stessa bugia del nome.
async fn attach_measured_tier(
    derived: &mut Derived,
    runs: &[ProfileRun],
    profiles: &[ProbeProfile],
    db: &PgPool,
    provider: &str,
    model: &str,
) {
    if derived.state != DerivedState::Qualified {
        return;
    }
    // Accoppia ogni run con la banda che il suo profilo certifica. `runs` e
    // `profiles` non sono allineati per indice (i profili non applicabili vengono
    // saltati da `profile_applies`, e l'early-stop tronca `runs`): il join va
    // fatto sulla CHIAVE, mai sulla posizione.
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
    let (tier_catalogo, banda_guadagnata) = tier_corrente(db, provider, model).await;
    derived.measured_tier = derive_tier_measured(
        &con_bande,
        banda_guadagnata.as_deref(),
        tier_catalogo.as_deref(),
        hold_min_passes(profiles),
    );
}

/// Il tier del modello nel catalogo, in DUE valori che non vanno confusi:
///   - `.0` il tier scritto da QUALUNQUE fonte: e' il pavimento del declassamento,
///     da li' si scende un gradino alla volta e solo dove c'e' una bocciatura vera;
///   - `.1` la banda GUADAGNATA dalla batteria (solo `tier_source='measured'`):
///     l'unica che ha diritto all'isteresi.
///
/// Leggendo il solo `performance_tier` — come faceva prima — un prior dell'indice
/// esterno si teneva la sua fascia con la soglia di conservazione: si autocertificava
/// con una clemenza pensata per chi la banda l'aveva dimostrata.
async fn tier_corrente(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> (Option<String>, Option<String>) {
    let riga: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT performance_tier, tier_source FROM ai_price_catalog \
          WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let (tier, fonte) = riga.unwrap_or((None, None));
    let guadagnata = match fonte.as_deref() {
        Some("measured") => tier.clone(),
        _ => None,
    };
    (tier, guadagnata)
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
        let params = profile_params(profile);
        let request = match build_profile_request(db, profile).await {
            Err(reason) => {
                runs.push(unbuildable_run(profile, &params, provider, model, &reason));
                continue;
            }
            Ok(r) => r,
        };
        let ctx = ProbeCtx {
            orchestrator,
            db,
            provider,
            model,
            profile,
            request: &request,
            params,
        };
        if profile.kind == "thinking_matrix" {
            let (off, native, policy) = run_thinking_matrix(&ctx, &mut last_evidence).await;
            thinking_derived = policy;
            runs.push(off);
            runs.push(native);
            continue;
        }
        let run = run_profile_attempts(&ctx, None, "", &mut last_evidence).await;
        let blocking_conclusive_fail = run.is_blocking && !run.passed() && run.conclusive_fails > 0;
        runs.push(run);
        if blocking_conclusive_fail {
            // Early-stop: i profili successivi non cambiano il verdetto.
            break;
        }
    }
    let mut derived = derive_capabilities(declared, &runs);
    derived.thinking = thinking_derived;
    attach_measured_tier(&mut derived, &runs, profiles, db, provider, model).await;
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

/// PROMOZIONE. Il `CASE` su `capability_locked` protegge la curatela: la matrice
/// thinking non sovrascrive una policy decisa a mano.
///
/// Il TIER non compare: lo scrive [`apply_tier`] (punto unico, regola L) nella
/// STESSA transazione di questa query. Prima la precedenza delle fonti era un
/// `CASE WHEN tier_source = 'manual'` scritto qui a mano, gemello della WHERE
/// scritta a mano in `refresh_tier_prior`: due formulazioni della stessa regola
/// in due linguaggi diversi, che reggevano solo finche' restavano allineate.
const SQL_QUALIFIED: &str = "UPDATE ai_price_catalog SET \
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
     qualification_started_at = NULL, \
     qualification_attempts = 0, \
     qualification_backoff_until = NULL \
 WHERE provider = $1 AND model = $2";

/// SQUALIFICA: backoff esponenziale sui tentativi, con tetto.
const SQL_DISQUALIFIED: &str = "UPDATE ai_price_catalog SET \
     qualification_state = 'disqualified', \
     qualified_capabilities = '[]'::jsonb, \
     qualification_reason = $3, \
     qualification_evidence_id = $4, \
     qualification_started_at = NULL, \
     qualification_attempts = qualification_attempts + 1, \
     qualification_backoff_until = NOW() + make_interval(hours => \
         LEAST($5::int * (1 << LEAST(qualification_attempts, 6)), $6::int)) \
 WHERE provider = $1 AND model = $2";

/// INCONCLUSIVO: un giro non attribuibile al modello NON declassa chi era gia'
/// qualificato (il CASE lo conserva). Si ritenta col backoff.
const SQL_INCONCLUSIVE: &str = "UPDATE ai_price_catalog SET \
     qualification_state = CASE \
         WHEN qualification_state = 'qualified' THEN 'qualified' \
         ELSE 'unqualified' END, \
     qualification_reason = $3, \
     qualification_started_at = NULL, \
     qualification_attempts = qualification_attempts + 1, \
     qualification_backoff_until = NOW() + make_interval(hours => \
         LEAST($4::int * (1 << LEAST(qualification_attempts, 6)), $5::int)) \
 WHERE provider = $1 AND model = $2";

/// Una scrittura dello stato derivato: raggruppa cio' che le tre query hanno in
/// comune, cosi' ogni ramo dichiara solo i propri bind.
struct DerivedWrite<'a> {
    db: &'a PgPool,
    provider: &'a str,
    model: &'a str,
    profiles_suite: i32,
    derived: &'a Derived,
    evidence_id: Option<i64>,
    ttl_days: i64,
    backoff_base_hours: i64,
}

type WriteResult = Result<sqlx::postgres::PgQueryResult, sqlx::Error>;

impl DerivedWrite<'_> {
    async fn qualified(&self) -> WriteResult {
        // Policy thinking DERIVATA dalla matrice (fase 5): scritta solo se
        // presente. Il trigger di invalidazione (0591) non scatta:
        // NEW.capability_source='probe'.
        let (policy, uses_thinking): (Option<&str>, Option<bool>) = match self.derived.thinking {
            Some((p, u)) => (Some(p), Some(u)),
            None => (None, None),
        };
        // Verdetto e banda misurata atterrano INSIEME o niente: il tier lo
        // scrive apply_tier (punto unico), ma dentro questa transazione. Con due
        // statement sciolti un errore fra l'uno e l'altro lascerebbe un modello
        // 'qualified' con la banda di ieri — uno stato che nessuno dei due
        // writer avrebbe voluto.
        let mut tx = self.db.begin().await?;
        let res = sqlx::query(SQL_QUALIFIED)
            .bind(self.provider)
            .bind(self.model)
            .bind(json!(self.derived.qualified_capabilities))
            .bind(self.ttl_days as i32)
            .bind(self.profiles_suite)
            .bind(&self.derived.reason)
            .bind(self.evidence_id)
            .bind(policy)
            .bind(uses_thinking)
            .execute(&mut *tx)
            .await?;
        // `None` = nessuna banda certificata da questo giro: il tier resta
        // quello che c'e' (regola H: un giro che non dimostra nulla non declassa
        // nessuno).
        if let Some(tier) = self.derived.measured_tier.as_deref() {
            crate::orchestrator::model_service::apply_tier(
                &mut *tx,
                self.provider,
                self.model,
                tier,
                crate::orchestrator::model_service::TierSource::Measured,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(res)
    }

    async fn disqualified(&self) -> WriteResult {
        sqlx::query(SQL_DISQUALIFIED)
            .bind(self.provider)
            .bind(self.model)
            .bind(&self.derived.reason)
            .bind(self.evidence_id)
            .bind(self.backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(self.db)
            .await
    }

    async fn inconclusive(&self) -> WriteResult {
        sqlx::query(SQL_INCONCLUSIVE)
            .bind(self.provider)
            .bind(self.model)
            .bind(&self.derived.reason)
            .bind(self.backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(self.db)
            .await
    }
}

/// Scrive lo stato derivato sulla riga (writer UNICO della promozione).
#[allow(clippy::too_many_arguments)]
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
    let w = DerivedWrite {
        db,
        provider,
        model,
        profiles_suite,
        derived,
        evidence_id,
        ttl_days,
        backoff_base_hours,
    };
    let res = match derived.state {
        DerivedState::Qualified => w.qualified().await,
        DerivedState::Disqualified => w.disqualified().await,
        DerivedState::Inconclusive => w.inconclusive().await,
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
    let cfg = RoundConfig {
        suite_version: profiles.iter().map(|p| p.suite_version).max().unwrap_or(1),
        ttl_days,
        backoff_hours,
    };
    let claimed = claim_candidates(db, max_per_round, cfg.suite_version).await;
    if claimed.is_empty() {
        return 0;
    }
    tracing::info!(
        candidati = claimed.len(),
        suite_version = cfg.suite_version,
        "model_qualification: giro di qualificazione avviato"
    );
    let mut done = 0usize;
    for (provider, model, caps) in &claimed {
        if qualify_claimed(orchestrator, db, provider, model, caps, &profiles, &cfg).await {
            done += 1;
        }
    }
    done
}

/// I parametri del giro, letti una volta dal DB (regola G).
struct RoundConfig {
    suite_version: i32,
    ttl_days: i64,
    backoff_hours: i64,
}

/// Claim CAS. I candidati GIA' qualified (scaduti o con suite vecchia) sono
/// ri-provati IN SHADOW: lo state resta 'qualified' (il pool non si svuota durante
/// la ri-qualificazione); il lock del claim e' qualification_started_at (stantio
/// oltre STALE_PROBING_MINUTES = worker morto, riclaimabile).
/// I candidati del giro. Il filtro sul COOLDOWN sta QUI, non solo a valle:
/// `qualify_claimed` controlla `is_provider_in_cooldown` col commento "non
/// sprecare il giro", ma a quel punto il giro e' gia' speso — il claim ha
/// consumato uno dei `max_per_round` posti per un modello che verra' scartato
/// in 10 millisecondi.
///
/// Misurato il 2026-07-16, dopo il fix del panic: due giri consecutivi hanno
/// reclamato 8 modelli, TUTTI di openai/anthropic (in cooldown per
/// `credit_balance_too_low`), e li hanno buttati tutti. Non e' sfortuna: quei
/// due provider sono 76 modelli su 116 e l'ORDER BY per scadenza li pesca
/// quasi sempre. A 4 per giro ogni 30 minuti servivano ~9 ore per smaltirli
/// prima di toccare i 34 modelli misurabili — cioe' il "tier dai fatti" non
/// avrebbe misurato nulla per un'intera giornata, con la batteria che girava
/// e sembrava sana.
///
/// Il filtro usa `nexus_provider_health.billing_cooldown_until` (la fonte
/// PERSISTENTE del cooldown lungo, ADR 0020/0030). Il cooldown breve vive in
/// memoria e non e' interrogabile da SQL: per quello resta il check a valle,
/// che da rete di sicurezza torna a essere cio' che deve essere — un caso raro,
/// non la norma.
const SQL_CLAIM: &str = "UPDATE ai_price_catalog c SET \
     qualification_state = CASE WHEN c.qualification_state = 'qualified' \
                                THEN 'qualified' ELSE 'probing' END, \
     qualification_started_at = NOW() \
 FROM ( \
     SELECT provider, model FROM ai_price_catalog p \
      WHERE is_enabled = TRUE AND supports_tool_use = TRUE \
        AND (qualification_backoff_until IS NULL OR qualification_backoff_until < NOW()) \
        AND (qualification_started_at IS NULL \
             OR qualification_started_at < NOW() - make_interval(mins => $2::int)) \
        AND (qualification_state IN ('unqualified','quarantined','probing') \
             OR (qualification_state = 'qualified' \
                 AND (qualification_expires_at < NOW() \
                      OR qualification_suite_version < $3))) \
        AND NOT EXISTS (SELECT 1 FROM nexus_provider_health h \
                         WHERE h.provider = p.provider \
                           AND h.billing_cooldown_until > NOW()) \
      ORDER BY (qualification_state = 'unqualified') DESC, \
               qualification_expires_at ASC NULLS FIRST \
      LIMIT $1 \
      FOR UPDATE SKIP LOCKED \
 ) cand \
 WHERE c.provider = cand.provider AND c.model = cand.model \
 RETURNING c.provider, c.model, c.capabilities";

async fn claim_candidates(
    db: &PgPool,
    max_per_round: i64,
    suite_version: i32,
) -> Vec<(String, String, Value)> {
    sqlx::query_as(SQL_CLAIM)
        .bind(max_per_round)
        .bind(STALE_PROBING_MINUTES as i32)
        .bind(suite_version)
        .fetch_all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "model_qualification: claim candidati fallito");
            Vec::new()
        })
}

/// Il provider e' in cooldown: il giro NON e' un fallimento del modello, e
/// registrarlo come tale lo squalificherebbe per colpa d'altri.
async fn mark_provider_cooldown(db: &PgPool, provider: &str, model: &str, cfg: &RoundConfig) {
    let inconclusive = Derived {
        state: DerivedState::Inconclusive,
        qualified_capabilities: Vec::new(),
        reason: "provider_in_cooldown".into(),
        thinking: None,
        // Un giro non attribuibile al modello non misura nessuna banda.
        measured_tier: None,
    };
    apply_derived(
        db,
        provider,
        model,
        cfg.suite_version,
        &inconclusive,
        None,
        cfg.ttl_days,
        cfg.backoff_hours,
    )
    .await;
}

/// Qualifica UN candidato gia' claimato. `false` = giro non speso (il modello non
/// e' stato provato).
async fn qualify_claimed(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    caps: &Value,
    profiles: &[ProbeProfile],
    cfg: &RoundConfig,
) -> bool {
    // Provider in cooldown: non sprecare il giro (esito non attribuibile).
    if crate::provider_cooldown::is_provider_in_cooldown(provider) {
        mark_provider_cooldown(db, provider, model, cfg).await;
        return false;
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
        qualify_one(orchestrator, db, provider, model, &declared, profiles).await;
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
        cfg.suite_version,
        &derived,
        evidence_id,
        cfg.ttl_days,
        cfg.backoff_hours,
    )
    .await;
    true
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

    // I tre test del ponte `error_class_from_gateway` vivevano qui. Sono stati
    // rimossi con la funzione che coprivano: la chiamavano DIRETTAMENTE, con un
    // `anyhow::Error` costruito a mano, e restavano verdi mentre in produzione
    // quella funzione non veniva mai raggiunta (vedi il commento sulla rimozione,
    // in testa al file). Codice e test condividevano l'assunto "l'errore del
    // provider arriva come Err", e il produttore reale non entrava nel test: la
    // stessa forma di cecita' dell'incidente `turn['result']`.
    //
    // Il ponte vivo e' ora coperto in `orchestrator::neural_client`, dove il test
    // attraversa il produttore vero del turno.


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
        // Le soglie dell'indice dal seed (mig 0600).
        TierPriorThresholds {
            agentic_index_frontier_min: 45.0,
            agentic_index_heavy_min: 35.0,
            agentic_index_high_min: 25.0,
            agentic_index_medium_min: 10.0,
        }
    }

    /// L'agentic_index e' l'UNICO seme del tier `synced` (mig 0608): MISURA la
    /// capacita' agentica, mentre il prezzo era il posizionamento commerciale
    /// del fornitore e il nome un aggettivo di marketing.
    ///
    /// I due casi REALI che il prezzo sbagliava (misurati sul parco il 16/07):
    /// gpt-5.4-mini costa da heavy (>$2) ma vale 30.2 -> e' 'high', non 'heavy';
    /// claude-opus-4-8 vale 47.2 -> resta 'frontier', il prezzo lo declassava.
    #[test]
    fn l_agentic_index_e_il_solo_seme_del_tier() {
        // gpt-5.4-mini: un MINI caro non e' heavy, l'indice dice la verita'.
        assert_eq!(derive_tier_prior(Some(30.2), &soglie()), Some("high"));
        // claude-opus-4-8: 47.2 -> frontier (il prezzo lo faceva scendere a heavy).
        assert_eq!(derive_tier_prior(Some(47.2), &soglie()), Some("frontier"));
        // gpt-5.6-sol: il migliore del parco.
        assert_eq!(derive_tier_prior(Some(54.0), &soglie()), Some("frontier"));
        // mistral-large-2512: vale poco -> light, qualunque cosa costi.
        assert_eq!(derive_tier_prior(Some(5.5), &soglie()), Some("light"));
        // Le soglie intermedie.
        assert_eq!(derive_tier_prior(Some(35.0), &soglie()), Some("heavy"));
        assert_eq!(derive_tier_prior(Some(10.0), &soglie()), Some("medium"));
    }

    /// Senza indice il prior TACE: meglio NULL che una bugia. E' la differenza
    /// fra "non lo so" e "e' medium", che il DEFAULT 'medium' (rimosso dalla
    /// mig 0599) rendeva indistinguibili. Il ripiego sul PREZZO e' stato rimosso
    /// (mig 0608): dava tier opposti allo stesso modello (mistral-medium-2505
    /// light vs -2604 heavy) e un tier NULL non e' pericoloso — col gate acceso
    /// un modello non qualificato e' gia' fuori dal pool agentico, e la batteria
    /// gli dara' una banda `measured` al primo giro.
    #[test]
    fn senza_indice_il_prior_tace() {
        assert_eq!(derive_tier_prior(None, &soglie()), None);
    }

    /// Un giro in cui i tentativi non superati sono BOCCIATURE vere: il modello ha
    /// risposto e non ha fatto il lavoro. E' l'unico caso che nega una banda.
    fn run(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
        run_conclusive(key, passes, promote_min)
    }

    fn run_conclusive(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
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

    /// Un giro in cui i tentativi non superati sono INCONCLUSIVI: rate limit,
    /// timeout, provider giu', profilo non costruibile. Il modello non e' stato
    /// misurato, quindi non ha negato niente.
    ///
    /// Questo helper esiste perche' `run()` fissava `inconclusive: 0` e il ramo
    /// del silenzio non era esercitato da nessun test: il doc prometteva "si
    /// retrocede solo con fallimenti CONCLUSIVI" e il codice non li leggeva.
    fn run_inconclusive(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
        ProfileRun {
            profile_key: key.to_string(),
            grants: vec![],
            is_blocking: false,
            passes,
            conclusive_fails: 0,
            inconclusive: 4 - passes,
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
        assert_eq!(derive_tier_measured(&runs, None, None, 2), Some("high".to_string()),
            "3/4 su agentic_chain promuove a high; agentic_recovery con 1/4 non certifica heavy");
    }

    /// Un profilo che non certifica un tier (tool_smoke) non influenza la banda.
    #[test]
    fn un_profilo_senza_banda_non_influenza_il_tier() {
        let runs = vec![
            (run("tool_smoke", 4, 3), None),
            (run("chat_smoke", 4, 3), Some("light".to_string())),
        ];
        assert_eq!(derive_tier_measured(&runs, None, None, 2), Some("light".to_string()));
    }

    /// L'ISTERESI: la soglia per CONSERVARE una banda gia' acquisita e' piu'
    /// bassa di quella per conquistarla. Senza, un modello oscillerebbe di fascia
    /// a ogni riqualifica e destabilizzerebbe il routing.
    #[test]
    fn l_isteresi_conserva_la_banda_acquisita_ma_non_ne_regala_di_nuove() {
        // 2 pass su 4: sotto promote_min (3) ma sopra hold_min (2).
        let runs = vec![(run("agentic_recovery", 2, 3), Some("heavy".to_string()))];
        // Chi e' GIA' heavy la mantiene (2 >= hold_min).
        assert_eq!(derive_tier_measured(&runs, Some("heavy"), Some("heavy"), 2), Some("heavy".to_string()),
            "banda acquisita: si conserva con hold_min, la conservazione non ha              bisogno di evidenza nuova");
        // Chi e' medium NON la conquista (2 < promote_min): serve piu' evidenza
        // per salire che per restare.
        assert_eq!(derive_tier_measured(&runs, Some("medium"), Some("medium"), 2), None,
            "banda NUOVA: serve promote_min. Il gap fra le due soglie E' l'isteresi");
    }

    // ── Il silenzio non declassa ────────────────────────────────────────────
    //
    // Il doc di `derive_tier_measured` prometteva da sempre "si retrocede solo
    // sotto hold_min E con fallimenti CONCLUSIVI", ma il codice non leggeva mai
    // `conclusive_fails`: nessun test poteva accorgersene perche' l'helper `run()`
    // fissava `inconclusive: 0`. Questi test esercitano il ramo che mancava.

    /// LA REGRESSIONE MISURATA (2026-07-17): con `agentic_chain` e
    /// `agentic_recovery` non implementati, i loro profili chiudono INCONCLUSIVI.
    /// Un modello che passa solo `agentic_real` avrebbe preso `measured=medium`, e
    /// siccome `measured` batte `synced` sarebbe stato declassato da frontier: e'
    /// il caso di x-ai/grok-4.5 (indice 45.7, il migliore del parco), che nessun
    /// profilo ha mai contestato. 14 modelli su 29 sarebbero scesi cosi'.
    #[test]
    fn una_banda_mai_provata_non_declassa_chi_ce_l_ha_gia() {
        let runs = vec![
            (run_conclusive("chat_smoke", 1, 1), Some("light".to_string())),
            (run_conclusive("agentic_real", 3, 3), Some("medium".to_string())),
            // I due kind non implementati: il profilo non e' costruibile -> nessun
            // pass, nessuna bocciatura. SILENZIO.
            (run_inconclusive("agentic_chain", 0, 3), Some("high".to_string())),
            (run_inconclusive("agentic_recovery", 0, 3), Some("heavy".to_string())),
            (run_inconclusive("agentic_longctx", 0, 3), Some("frontier".to_string())),
        ];
        assert_eq!(
            derive_tier_measured(&runs, None, Some("frontier"), 2),
            None,
            "nessuna banda alta e' stata NEGATA: non si scrive un measured che \
             declassa. 'non l'ho provato' non e' 'l'ha fallito'"
        );
    }

    /// Il contrario: se il modello ha DAVVERO fallito la banda che aveva, si
    /// scende. Ma di UN gradino, fino alla piu' alta non contestata — non fino
    /// alla piu' alta certificata.
    #[test]
    fn una_negazione_conclusiva_declassa_di_un_gradino_solo() {
        let runs = vec![
            (run_conclusive("agentic_real", 3, 3), Some("medium".to_string())),
            // heavy NEGATO con evidenza: il modello ha risposto e ha sbagliato.
            (run_conclusive("agentic_recovery", 0, 3), Some("heavy".to_string())),
            // high: mai provato (silenzio).
            (run_inconclusive("agentic_chain", 0, 3), Some("high".to_string())),
        ];
        assert_eq!(
            derive_tier_measured(&runs, Some("heavy"), Some("heavy"), 2),
            Some("high".to_string()),
            "heavy e' stato negato -> si scende, ma a high, che nessuno ha \
             contestato. Scendere a medium punirebbe il modello per i due profili \
             che non abbiamo saputo eseguire"
        );
    }

    /// LA SCALA E' UNA SCALA: `agentic_longctx` certifica `frontier` (il vertice)
    /// ed e' l'unico profilo alto implementato. Senza il guard, un modello che
    /// fallisce la catena ma ritrova l'ago si prende il tier piu' alto di tutti,
    /// scavalcando i gradini che non ha salito.
    #[test]
    fn frontier_non_scavalca_una_banda_inferiore_negata() {
        let runs = vec![
            (run_conclusive("agentic_real", 3, 3), Some("medium".to_string())),
            (run_conclusive("agentic_chain", 0, 3), Some("high".to_string())), // NEGATA
            (run_conclusive("agentic_longctx", 4, 3), Some("frontier".to_string())),
        ];
        assert_eq!(
            derive_tier_measured(&runs, None, None, 2),
            Some("medium".to_string()),
            "ha trovato l'ago ma non sa concatenare due tool: frontier scavalcherebbe \
             high, che ha fallito davanti a noi"
        );
    }

    /// L'isteresi protegge una banda GUADAGNATA, non un prior dell'indice esterno:
    /// il `tier_source` distingue le due cose. Senza, un synced=heavy si teneva
    /// heavy con la soglia bassa — l'indice si autocertificava con la nostra
    /// clemenza — e un giro tutto inconclusivo avrebbe riciclato il prior in
    /// `measured` con zero probe superati.
    #[test]
    fn un_prior_synced_non_gode_dell_isteresi_ne_diventa_measured_a_vuoto() {
        let runs = vec![(run_conclusive("agentic_recovery", 2, 3), Some("heavy".to_string()))];
        // Il catalogo dice heavy, ma NON l'ha guadagnato la batteria (synced):
        // 2/4 non basta a conquistarlo (serve promote_min=3).
        assert_eq!(
            derive_tier_measured(&runs, None, Some("heavy"), 2),
            None,
            "il prior dell'indice non ha diritto alla soglia di conservazione: \
             quella spetta a chi la banda l'ha dimostrata"
        );
    }
    /// IL CERCHIO SI CHIUDE: la batteria SCRIVE il tier misurato, e la curatela
    /// dell'admin vince sempre.
    ///
    /// E' l'ultimo anello: senza, `derive_tier_measured` calcolava una banda che
    /// nessuno scriveva, e il tier restava per sempre al `synced` (il seme
    /// dell'indice) anche dopo che la batteria aveva DIMOSTRATO la capacita'.
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
            "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source) VALUES              ('p', 'stimato', 'medium', 'synced'),              ('p', 'curato',  'light',  'manual')",
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
            "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source)              VALUES ('p', 'm', 'high', 'synced')",
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
            (Some("high".into()), Some("synced".into())),
            "nessuna banda certificata: il tier resta il prior, non viene azzerato"
        );
    }
    /// La SELECT deve CHIEDERE tutto cio' che il mapper LEGGE: `certifies_tier`
    /// (mig 0599) era letto ma non selezionato, e `Row::get` panicava dentro il
    /// task tokio del worker. Il servizio restava `health: ok` mentre la batteria
    /// era morta a ogni giro: nessun modello ha mai raggiunto `tier_source
    /// = 'measured'`, e il "tier dai fatti" cadeva in eterno sul prior del prezzo.
    /// Gli altri test non lo videro perche' NON toccano il DB: qui lo schema e'
    /// quello vero delle migrazioni, non una finzione ricostruita a mano.
    #[sqlx::test]
    async fn la_select_dei_profili_chiede_ogni_colonna_che_il_mapper_legge(pool: PgPool) {
        // Schema come mig 0591 (tabella) + 0599 (certifies_tier).
        sqlx::query(
            "CREATE TABLE ai_model_probe_profile (
               profile_key    TEXT PRIMARY KEY,
               suite_version  INT NOT NULL,
               ord            INT NOT NULL,
               kind           TEXT NOT NULL,
               is_blocking    BOOLEAN NOT NULL,
               applies_when   JSONB,
               grants         JSONB NOT NULL DEFAULT '[]'::jsonb,
               payload        JSONB NOT NULL DEFAULT '{}'::jsonb,
               pass_predicate JSONB NOT NULL DEFAULT '{}'::jsonb,
               enabled        BOOLEAN NOT NULL DEFAULT TRUE,
               certifies_tier TEXT)",
        )
        .execute(&pool)
        .await
        .expect("schema profili");
        sqlx::query(
            "INSERT INTO ai_model_probe_profile
               (profile_key, suite_version, ord, kind, is_blocking, grants, certifies_tier)
             VALUES ('agentic_recovery', 2, 1, 'tool_realistic', true, '[\"chat\"]'::jsonb, 'heavy'),
                    ('tool_smoke',       2, 2, 'tool_minimal',   true, '[]'::jsonb,        NULL)",
        )
        .execute(&pool)
        .await
        .expect("seed profili");

        let profiles = load_profiles(&pool).await;

        assert_eq!(profiles.len(), 2, "entrambi i profili enabled vanno caricati");
        assert_eq!(
            profiles[0].certifies_tier.as_deref(),
            Some("heavy"),
            "la banda certificata deve ARRIVARE dal DB: senza, derive_tier_measured \
             non ha nulla da certificare e il tier non lascia mai il prior"
        );
        assert_eq!(
            profiles[1].certifies_tier, None,
            "NULL resta NULL: un profilo che qualifica senza certificare una banda"
        );
    }

    /// Il claim NON spende un posto del giro per un modello che verra' buttato.
    ///
    /// Il check `is_provider_in_cooldown` in `qualify_claimed` dice "non sprecare
    /// il giro", ma scatta DOPO il claim: il posto e' gia' consumato. Misurato il
    /// 2026-07-16 sul sistema vivo: due giri di fila hanno reclamato 8 modelli,
    /// tutti di openai/anthropic in cooldown per credito esaurito, e li hanno
    /// scartati in 10ms — 76 modelli su 116 stanno su quei due provider, quindi
    /// servivano ~9 ore di giri a vuoto prima di toccare un modello misurabile.
    /// La batteria girava, sembrava sana, e non misurava nulla.
    #[sqlx::test]
    async fn il_claim_salta_i_provider_in_cooldown_invece_di_sprecarci_il_giro(pool: PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        // `qualification_state` e `qualification_expires_at` le crea gia'
        // l'helper: qui solo le colonne del CLAIM che gli mancano.
        sqlx::query(
            "ALTER TABLE ai_price_catalog \
               ADD COLUMN qualification_suite_version INT, \
               ADD COLUMN qualification_started_at TIMESTAMPTZ, \
               ADD COLUMN qualification_backoff_until TIMESTAMPTZ",
        )
        .execute(&pool)
        .await
        .expect("colonne qualification");
        // La fonte PERSISTENTE del cooldown lungo (ADR 0020).
        sqlx::query(
            "CREATE TABLE nexus_provider_health ( \
               provider TEXT PRIMARY KEY, \
               billing_cooldown_until TIMESTAMPTZ)",
        )
        .execute(&pool)
        .await
        .expect("health");
        sqlx::query(
            "INSERT INTO nexus_provider_health (provider, billing_cooldown_until) VALUES \
             ('openai', NOW() + INTERVAL '6 hours'), \
             ('mistral', NOW() - INTERVAL '1 hour')",
        )
        .execute(&pool)
        .await
        .expect("cooldown");
        // openai e' il piu' "urgente" per l'ORDER BY (unqualified, scadenza
        // NULL): senza il filtro vincerebbe entrambi i posti del giro.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, qualification_state) VALUES \
             ('openai',  'gpt-in-cooldown-1', true, true, 'unqualified'), \
             ('openai',  'gpt-in-cooldown-2', true, true, 'unqualified'), \
             ('mistral', 'sano',              true, true, 'unqualified'), \
             ('google',  'sano-2',            true, true, 'unqualified')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let claimed = claim_candidates(&pool, 2, 2).await;

        let modelli: Vec<&str> = claimed.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(claimed.len(), 2, "il giro deve riempire i suoi posti: {modelli:?}");
        assert!(
            !modelli.iter().any(|m| m.contains("cooldown")),
            "REGRESSIONE: il claim ha speso un posto per un modello di un provider \
             in cooldown, che verra' buttato in 10ms. Reclamati: {modelli:?}"
        );
        // Il provider il cui cooldown e' SCADUTO torna candidabile: il filtro
        // guarda l'orologio, non la presenza della riga.
        assert!(modelli.contains(&"sano"), "mistral (cooldown scaduto) e' candidabile: {modelli:?}");
    }

    /// FRONTIER: il needle sta a META' della history, mai nel system prompt.
    #[test]
    fn la_history_pianta_il_needle_a_meta_del_pagliaio() {
        let h = build_needle_history(10_000);
        assert!(h.len() >= 10_000, "la history deve raggiungere la dimensione chiesta");
        assert_eq!(h.matches(LONG_CTX_NEEDLE).count(), 1, "UN solo ago nel pagliaio");
        let pos = h.find(LONG_CTX_NEEDLE).expect("needle");
        let frazione = pos as f64 / h.len() as f64;
        assert!(
            (0.3..0.7).contains(&frazione),
            "il needle deve stare a META' (era a {frazione:.2}): in coda lo              ritroverebbe il bias di recency senza usare la finestra, e il test              misurerebbe la posizione invece della capacita'"
        );
    }

    /// Il checker del needle e' DETERMINISTICO: o il codice c'e', o non c'e'.
    /// Nessun giudizio sulla prosa (regola M).
    #[test]
    fn il_needle_si_verifica_sul_testo_esatto() {
        let pred = json!({"requires_needle": true, "min_content_chars": 1});
        // Ha ritrovato il fatto: passa (anche con contorno).
        let turn = json!({ "content": format!("Il codice e' {LONG_CTX_NEEDLE}."),
                           "stop_reason": "end_turn" });
        assert!(evaluate_attempt(&turn, &pred, 100).pass);
        // Risposta plausibile ma SENZA il fatto: fallisce. E' il caso che conta —
        // un modello che "sembra" aver capito ma non ha letto la history.
        let turn = json!({ "content": "Non trovo alcun codice nel testo fornito.",
                           "stop_reason": "end_turn" });
        let out = evaluate_attempt(&turn, &pred, 100);
        assert!(!out.pass);
        assert_eq!(out.reason, "needle_not_found");
        // Un codice INVENTATO (allucinato) non passa: deve essere QUELLO.
        let turn = json!({ "content": "CODICE-PRATICA: AB12CD34", "stop_reason": "end_turn" });
        assert!(!evaluate_attempt(&turn, &pred, 100).pass, "un codice inventato non e' un recupero");
    }

    /// Il predicato non e' invasivo: senza `requires_needle` gli altri profili
    /// non cambiano comportamento.
    #[test]
    fn senza_requires_needle_gli_altri_profili_non_cambiano() {
        let pred = json!({"min_content_chars": 1});
        let turn = json!({ "content": "ok", "stop_reason": "end_turn" });
        assert!(evaluate_attempt(&turn, &pred, 100).pass);
    }
}
