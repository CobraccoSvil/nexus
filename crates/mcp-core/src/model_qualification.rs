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

/// Chiavi settings (mig 0591/0593, regola G), lock stantio e REGOLA di
/// eleggibilita': il punto unico e' il crate `nexus-model-eligibility` (regola L).
/// Vivono la' perche' l'explain diagnostico (`xtask battery-explain`) deve
/// interrogare la STESSA regola che questo worker esegue, invece di ricopiarla
/// (regola O): una copia divergerebbe in silenzio.
use nexus_model_eligibility::{
    KEY_BACKOFF_HOURS, KEY_MAX_PER_ROUND, KEY_ROUND_ENABLED, KEY_TTL_DAYS, STALE_PROBING_MINUTES,
};

use crate::orchestrator::Orchestrator;

/// Cap del backoff esponenziale (7 giorni): oltre non ha senso attendere di piu'.
const BACKOFF_CAP_HOURS: i64 = 168;

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
    /// I FATTI dietro il verdetto, quando ne esistono che nessuna colonna dedicata
    /// registra (le `measures` dei profili multi-step: `recovered`, `chained_links`,
    /// `repeated_failed`, `bad_tool_syntax`). Finisce nella colonna `derived` di
    /// `ai_model_probe_evidence` (mig 0591), che restava scritta da NESSUNO: un
    /// verdetto `no_recovery`/`no_chain` senza questi fatti non e' contestabile
    /// (regola O), ed e' precisamente cio' che ha reso invisibile la vera causa dei
    /// 93 fail del 2026-07-17 (il modello si arrende, non un bug di misura).
    /// `None` per i profili single-turn, che hanno gia' i loro segnali in colonna.
    pub derived: Option<Value>,
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
            // Lo riempie `evaluate_attempt` dal turno (unico posto che vede le
            // `measures`): qui il segnale grezzo non le conosce.
            derived: None,
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
    if let Some(motivo) = multi_step_fail_reason(turn, predicate) {
        return Some(motivo);
    }
    // Lo stato finale del profilo `latent_state`: il verificatore vive nel suo
    // modulo, qui c'e' solo la delega (regola L).
    if let Some(motivo) = crate::probe_latent_state::motivo_fallimento(turn, predicate) {
        return Some(motivo);
    }
    match predicate.get("max_latency_ms").and_then(Value::as_i64) {
        Some(cap) if latency_ms > cap => Some(format!("latency:{latency_ms}>{cap}")),
        _ => None,
    }
}

/// I predicati dei profili multi-step, confrontati coi FATTI che il loop ha
/// misurato (`probe_chain_measure::AttemptMeasures`, che il giro appende al turno
/// sotto `measures`).
///
/// Le tre chiavi erano nel DB da sempre e nessuno le leggeva: `predicate_fail_reason`
/// tace su cio' che non conosce e ritorna "superato". Finche' i kind non
/// esistevano il profilo non partiva e il buco non faceva danno; ora che il loop
/// c'e', queste righe sono l'unica cosa che impedisce a `high` e `heavy` di essere
/// gratis per chiunque.
fn multi_step_fail_reason(turn: &Value, predicate: &Value) -> Option<String> {
    let fatto_i64 = |k: &str| turn.pointer(&format!("/measures/{k}")).and_then(Value::as_i64);
    let fatto_bool = |k: &str| {
        turn.pointer(&format!("/measures/{k}"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };

    if let Some(min) = predicate.get(K_MIN_CHAINED).and_then(Value::as_i64) {
        // `chained_links` sono gli anelli CONSUMATI (un token nostro riportato in
        // una chiamata successiva), non le chiamate emesse: tre `list_files` di fila
        // fanno 0.
        let anelli = fatto_i64("chained_links").unwrap_or(0);
        if anelli < min {
            return Some(format!("no_chain:{anelli}<{min}"));
        }
    }
    if predicate.get(K_REQUIRES_RECOVERY).and_then(Value::as_bool) == Some(true)
        && !fatto_bool("recovered")
    {
        // Non ha portato il token che viveva solo nel messaggio d'errore: qualunque
        // cosa abbia fatto, non l'ha letto.
        return Some("no_recovery".to_string());
    }
    if predicate.get(K_FORBIDS_REPEAT).and_then(Value::as_bool) == Some(true)
        && fatto_bool(F_REPEATED_FAILED)
    {
        return Some(F_REPEATED_FAILED.to_string());
    }
    None
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

/// Il bersaglio della catena nella formula dello score (piano "scala relativa",
/// mig 0616): `s_chain = media di min(chained_links/LINKS_TARGET, 1)`. E' una
/// costante della FORMULA (catena piena), non un tuning di routing: cambiarla
/// cambia il significato degli score gia' persistiti, quindi il posto giusto per
/// una revisione e' un bump di suite, non un setting.
///
/// 5 -> 7 (mig 0618). A 5 la componente era SATURA e non separava piu' nessuno:
/// 120 tentativi su 156 (il 77%) toccavano esattamente 5 anelli e prendevano il
/// punteggio pieno. Quel 5 non era la bravura dei modelli, era il soffitto di
/// `max_turns: 6` — con 6 turni non se ne possono concatenare di piu'.
///
/// 7 -> 6 (mig 0621), e stavolta il bersaglio SCENDE. A 7 la componente si e'
/// risaturata (79% dei tentativi a 7 anelli, ministral-8b compreso): il soffitto
/// si era spostato, non rimosso, perche' seguire riferimenti concatenati non e'
/// piu' una capacita' rara. La 0621 non allunga la catena, la rende insidiosa —
/// il criterio di selezione sta nel primo messaggio e a un anello la pista si
/// interrompe. Rientrare COSTA UN TURNO, quindi con gli stessi 8 turni la
/// traiettoria perfetta vale 6 anelli e non piu' 7.
///
/// Il bersaglio e' esattamente il tetto della traiettoria intesa, provato dal
/// golden agent (`la_traiettoria_intesa_arriva_in_fondo`, probe_agentic_loop).
/// Lasciarlo a 7 avrebbe costruito il difetto opposto e altrettanto disonesto:
/// un pieno che NESSUNO puo' prendere, cioe' di nuovo la misura del nostro tetto.
const LINKS_TARGET: f64 = 6.0;

/// Somme CONTINUE sui tentativi CONCLUSIVI di un profilo (mig 0616): alimentano
/// [`derive_measured_score`] senza rileggere l'evidence. Restano a zero sui
/// profili single-turn (che non producono `measures`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SommeConclusive {
    /// Somma di `min(chained_links/LINKS_TARGET, 1)` per tentativo conclusivo:
    /// CONTINUA (separa links-3 da links-5), gia' clampata per-tentativo.
    pub chain_frac: f64,
    /// Tentativi conclusivi col FATTO `recovered` (ha letto l'errore).
    pub recovered: u32,
    /// Tentativi conclusivi col fatto `repeated_failed` (malus).
    pub repeated: u32,
    /// Tentativi conclusivi col fatto `bad_tool_syntax` (malus).
    pub bad_syntax: u32,
}

/// Esito aggregato di UN profilo eseguito (`repeat` tentativi).
#[derive(Debug, Clone)]
pub(crate) struct ProfileRun {
    pub profile_key: String,
    /// Il `kind` del profilo: e' la chiave con cui lo score aggancia ogni run
    /// alla sua componente (chain/recovery/real/latent/longctx).
    pub kind: String,
    pub grants: Vec<String>,
    pub is_blocking: bool,
    pub passes: u32,
    pub conclusive_fails: u32,
    pub inconclusive: u32,
    /// Pass minimi per promuovere (dal `pass_predicate`, default = repeat).
    pub promote_min: u32,
    pub first_fail_reason: Option<String>,
    pub somme: SommeConclusive,
}

impl ProfileRun {
    fn passed(&self) -> bool {
        self.passes >= self.promote_min
    }

    /// I tentativi CONCLUSIVI del run: il denominatore di ogni componente dello
    /// score (gli inconclusivi stanno fuori da numeratore E denominatore).
    fn conclusivi(&self) -> u32 {
        self.passes + self.conclusive_fails
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
    /// Lo SCORE MISURATO 0-100 (mig 0616, [`derive_measured_score`]). `None` =
    /// giro non Qualified, pesi non configurati, o una componente applicabile
    /// senza tentativi conclusivi (il silenzio non punteggia): score e bande
    /// restano invariati.
    pub measured_score: Option<f64>,
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

/// TIER `synced` DALLA CLASSIFICAZIONE ESTERNA, sulla scala RELATIVA (mig 0615).
///
/// E' il SEME onesto in attesa della misura, non una verita': appena la
/// batteria certifica una banda, `measured` lo sostituisce. L'agentic_index
/// MISURA la capacita' agentica (Agents 34% + Coding 24% dell'Intelligence
/// Index, su harness con tool): e' il nostro uso esatto — a differenza del NOME
/// (il token `mini` copre intelligence 6.8-50.2) e del PREZZO (posizionamento
/// commerciale). Le soglie ASSOLUTE sono uscite con la mig 0615: erano fossili
/// dell'indice di un giorno preciso, e a ogni rilascio forte andavano riviste a
/// mano. Ora la banda e' `tier_from_leader` (punto unico, regola L): il piu'
/// forte del parco e' l'ancora e tutti si misurano su di lui.
///
/// `None` = indice assente o stantio (il chiamante lo azzera oltre
/// `max_age_hours`: la fonte e' undocumented e puo' sparire — un indice vecchio
/// non deve passare per fresco). Il tier resta NULL e il sistema DICE che non lo
/// sa, invece di scrivere 'medium' e far finta (regola G: niente fallback
/// magico). Un tier NULL non e' pericoloso: col gate acceso un modello non
/// qualificato e' gia' fuori dal pool agentico, e la batteria gli dara' una
/// banda `measured` al primo giro utile.
pub(crate) fn derive_tier_prior(
    agentic_index: Option<f64>,
    leader: f64,
    bands: &crate::orchestrator::model_service::RelativeBands,
) -> Option<&'static str> {
    agentic_index
        .map(|idx| crate::orchestrator::model_service::tier_from_leader(idx, leader, bands))
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
                measured_score: None,
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
                measured_score: None,
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
                measured_score: None,
    }
}

// ── Lo SCORE MISURATO (mig 0616): la batteria in un numero 0-100 ────────────

/// I pesi della formula, dal DB (settings `catalog.measured_score.*`, regola G).
/// `w_recovery` = 30 e' deliberato: un gemello del leader senza recovery resta
/// FUORI da frontier senza dipendere dal malus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MeasuredScoreWeights {
    pub chain: f64,
    pub recovery: f64,
    pub real: f64,
    pub latent: f64,
    pub longctx: f64,
}

/// Come una componente estrae la sua frazione [0,1] dal run.
#[derive(Clone, Copy)]
enum ModoComponente {
    /// `media di min(chained_links/LINKS_TARGET, 1)`: CONTINUA, separa
    /// links-3 da links-5 (la somma clampata la accumula `tally`).
    CatenaContinua,
    /// Il rate del FATTO `recovered` sui conclusivi: la capacita' di leggere
    /// l'errore, distinta dalla disciplina (che il pass/fail gia' giudica).
    RateRecupero,
    /// Il pass rate conclusivo del profilo.
    RatePass,
}

/// Le 5 componenti della formula: (kind del profilo, peso, modo). L'ordine non
/// conta (somma), la CHIAVE e' il kind — la stessa parola del dispatch (regola N).
fn componenti_score(w: &MeasuredScoreWeights) -> [(&'static str, f64, ModoComponente); 5] {
    [
        (KIND_TOOL_CHAIN, w.chain, ModoComponente::CatenaContinua),
        (KIND_TOOL_RECOVERY, w.recovery, ModoComponente::RateRecupero),
        (KIND_TOOL_REALISTIC, w.real, ModoComponente::RatePass),
        (crate::probe_latent_state::KIND_LATENT_STATE, w.latent, ModoComponente::RatePass),
        (KIND_LONG_CONTEXT, w.longctx, ModoComponente::RatePass),
    ]
}

/// Aggregato dei run di un `kind`: (tentativi conclusivi, pass, somme).
fn aggrega_kind(runs: &[ProfileRun], kind: &str) -> (u32, u32, SommeConclusive) {
    let mut tot = (0u32, 0u32, SommeConclusive::default());
    for r in runs.iter().filter(|r| r.kind == kind) {
        tot.0 += r.conclusivi();
        tot.1 += r.passes;
        tot.2.chain_frac += r.somme.chain_frac;
        tot.2.recovered += r.somme.recovered;
        tot.2.repeated += r.somme.repeated;
        tot.2.bad_syntax += r.somme.bad_syntax;
    }
    tot
}

/// Il malus della formula: `-5*repeated_rate -5*bad_syntax_rate`, cap -5
/// totale. I rate sono sui tentativi CONCLUSIVI dei profili multi-step (gli
/// unici che misurano quei fatti).
fn malus_conclusivo(runs: &[ProfileRun]) -> f64 {
    let (mut conclusivi, mut repeated, mut bad) = (0u32, 0u32, 0u32);
    for kind in [KIND_TOOL_CHAIN, KIND_TOOL_RECOVERY] {
        let (n, _, somme) = aggrega_kind(runs, kind);
        conclusivi += n;
        repeated += somme.repeated;
        bad += somme.bad_syntax;
    }
    if conclusivi == 0 {
        return 0.0;
    }
    let n = f64::from(conclusivi);
    (5.0 * f64::from(repeated) / n + 5.0 * f64::from(bad) / n).min(5.0)
}

/// PUNTO UNICO PURO (regola L) dello SCORE MISURATO 0-100: media pesata delle
/// 5 componenti sui tentativi CONCLUSIVI (gli inconclusivi fuori da numeratore
/// E denominatore), meno il malus.
///
/// `kinds_applicabili` = i kind dei profili che questo giro POTEVA correre
/// (enabled + `applies_when` soddisfatto). Una componente NON applicabile per
/// struttura (o assente dalla suite) vale 0 punti SENZA rinormalizzare:
/// rinormalizzare premierebbe chi non puo' correre le prove alte.
///
/// `None` = una componente APPLICABILE e' rimasta senza tentativi conclusivi:
/// il silenzio non punteggia (regola H — score e bande restano invariati), mai
/// uno score parziale spacciato per intero.
pub(crate) fn derive_measured_score(
    runs: &[ProfileRun],
    kinds_applicabili: &[&str],
    w: &MeasuredScoreWeights,
) -> Option<f64> {
    let mut score = 0.0;
    for (kind, peso, modo) in componenti_score(w) {
        if !kinds_applicabili.contains(&kind) {
            continue; // 0 punti, dichiarati: niente rinormalizzazione
        }
        let (conclusivi, passes, somme) = aggrega_kind(runs, kind);
        if conclusivi == 0 {
            return None; // il silenzio non punteggia
        }
        let n = f64::from(conclusivi);
        let frazione = match modo {
            ModoComponente::CatenaContinua => somme.chain_frac / n,
            ModoComponente::RateRecupero => f64::from(somme.recovered) / n,
            ModoComponente::RatePass => f64::from(passes) / n,
        };
        score += peso * frazione;
    }
    Some((score - malus_conclusivo(runs)).clamp(0.0, 100.0))
}

/// La banda MEASURED di uno score sulla scala relativa, con l'isteresi di
/// demozione (mig 0616): si SALE superando la soglia della banda nuova, si
/// SCENDE solo sotto `soglia della banda attuale - demote_margin` (punti score).
/// `attuale` = la banda gia' GUADAGNATA dalla batteria (`tier_source =
/// 'measured'`), l'unica con diritto all'isteresi — mai un prior.
pub(crate) fn banda_measured(
    score: f64,
    attuale: Option<&str>,
    ancora: f64,
    bands: &crate::orchestrator::model_service::RelativeBands,
    demote_margin: f64,
) -> &'static str {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let candidata = crate::orchestrator::model_service::tier_from_leader(score, ancora, bands);
    let Some(acquisita) = attuale.and_then(banda_canonica) else {
        return candidata;
    };
    if tier_rank(candidata) >= tier_rank(acquisita) {
        return candidata;
    }
    let Some(pct) = bands.pct_of(acquisita) else {
        return candidata;
    };
    if score >= ancora * pct - demote_margin {
        acquisita // dentro il margine: la banda guadagnata si conserva
    } else {
        candidata
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
    // La FONTE dei profili (tabella + filtro enabled) e' condivisa con l'explain:
    // la premessa "quale suite e' corrente" e' esattamente cio' che lo script
    // diagnostico del 2026-07-17 aveva sbagliato leggendola dal catalogo.
    let rows = sqlx::query(concat!(
        "SELECT profile_key, suite_version, kind, is_blocking, certifies_tier, ",
        "applies_when, grants, payload, pass_predicate ",
        nexus_model_eligibility::profile_source!(),
        " ORDER BY ord"
    ))
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

/// Le chiavi che un `pass_predicate` puo' contenere. VOCABOLARIO CHIUSO (regola N):
/// tutto cio' che sta qui e' verificato da qualcuno, e cio' che non sta qui e' un
/// errore — non un vincolo da ignorare.
///
/// La differenza non e' formale. `predicate_fail_reason` legge le chiavi che
/// conosce e tace sulle altre, con default permissivi: un predicato
/// `{"min_chained_calls": 3}` su un profilo che non sa contare gli anelli non
/// verifica NIENTE e ritorna `None`, cioe' "superato". Il profilo gira, promuove
/// tutti e sembra funzionare: `high` diventerebbe gratis per chiunque, in silenzio.
/// E' il difetto peggiore possibile qui, perche' un test che non misura e'
/// indistinguibile da un test che passa.
/// Le tre chiavi dei profili multi-step. Sono nominate in tre punti — il
/// vocabolario, la lista di cio' che richiede un kind multi-turno, e il
/// verificatore — e devono essere LA STESSA parola: un refuso in uno dei tre
/// renderebbe il predicato muto, cioe' un pass regalato (regola N).
/// I due kind multi-step. Nominati dal dispatch, dal guard del predicato e dalla
/// scelta del mondo: se una delle tre copie divergesse, un profilo girerebbe col
/// mondo sbagliato o col predicato muto (regola N).
const KIND_TOOL_CHAIN: &str = "tool_chain";
const KIND_TOOL_RECOVERY: &str = "tool_recovery";
/// Gli altri due kind che alimentano lo score (mig 0616). Nominati qui e non
/// inline perche' la componente dello score e il dispatch della richiesta
/// devono essere LA STESSA parola (regola N).
const KIND_TOOL_REALISTIC: &str = "tool_realistic";
const KIND_LONG_CONTEXT: &str = "long_context";

const K_MIN_CHAINED: &str = "min_chained_calls";
const K_REQUIRES_RECOVERY: &str = "requires_recovery";
const K_FORBIDS_REPEAT: &str = "forbids_repeat_of_failed";
/// Il FATTO misurato dal loop (non la chiave del predicato): il modello ha
/// rimandato identica una chiamata gia' fallita.
const F_REPEATED_FAILED: &str = "repeated_failed";
/// Gli altri fatti delle `measures` multi-step: nominati UNA volta, perche' il
/// produttore (`turno_dai_fatti`) e i lettori (`tally`, predicati) devono usare
/// la stessa parola (regola N).
const F_CHAINED_LINKS: &str = "chained_links";
const F_RECOVERED: &str = "recovered";
const F_BAD_TOOL_SYNTAX: &str = "bad_tool_syntax";

const CHIAVI_PREDICATO: [&str; 10] = [
    // verificate da `predicate_fail_reason`
    "min_tool_calls",
    "min_content_chars",
    "requires_needle",
    "max_latency_ms",
    // lette da `profile_params` (isteresi delle bande)
    "hold_min_passes",
    "promote_min_passes",
    // verificate dal loop agentico (tool_chain / tool_recovery)
    K_MIN_CHAINED,
    K_REQUIRES_RECOVERY,
    K_FORBIDS_REPEAT,
    // verificata da `probe_latent_state` (latent_state)
    crate::probe_latent_state::K_REQUIRES_FINAL_STATE,
];

/// La prima chiave del predicato che nessuno sa verificare. `None` = il predicato
/// e' interamente coperto.
fn chiave_predicato_ignota(predicate: &Value) -> Option<String> {
    predicate
        .as_object()?
        .keys()
        .find(|k| !CHIAVI_PREDICATO.contains(&k.as_str()))
        .cloned()
}

/// I predicati che solo il loop multi-turno sa verificare. Su un profilo
/// single-turn sarebbero muti (nessun anello da contare, nessun errore iniettato):
/// il profilo promuoverebbe senza misurare cio' che dichiara di misurare.
const PREDICATI_MULTI_TURNO: [&str; 3] = [K_MIN_CHAINED, K_REQUIRES_RECOVERY, K_FORBIDS_REPEAT];

/// `Err` se il profilo chiede una verifica che il suo kind non puo' fare. Il
/// controllo e' incrociato apposta: le due meta' del contratto (kind e predicato)
/// vivono in righe diverse del DB e possono divergere per un refuso dell'admin.
fn predicato_coerente_col_kind(profile: &ProbeProfile) -> Result<(), String> {
    if let Some(k) = chiave_predicato_ignota(&profile.pass_predicate) {
        return Err(format!("predicato sconosciuto: {k}"));
    }
    let multi_turno = matches!(profile.kind.as_str(), KIND_TOOL_CHAIN | KIND_TOOL_RECOVERY);
    if !multi_turno {
        if let Some(k) = profile
            .pass_predicate
            .as_object()
            .and_then(|o| o.keys().find(|k| PREDICATI_MULTI_TURNO.contains(&k.as_str())))
        {
            return Err(format!(
                "predicato '{k}' richiede un kind multi-turno, ma il profilo e' '{}'",
                profile.kind
            ));
        }
    }
    // Stessa ragione per lo stato latente: su un altro kind non ci sarebbe nessuna
    // misura da leggere, il predicato sarebbe muto e `frontier` diventerebbe gratis.
    let k_stato = crate::probe_latent_state::K_REQUIRES_FINAL_STATE;
    if profile.kind != crate::probe_latent_state::KIND_LATENT_STATE
        && profile.pass_predicate.get(k_stato).is_some()
    {
        return Err(format!(
            "predicato '{k_stato}' richiede il kind '{}', ma il profilo e' '{}'",
            crate::probe_latent_state::KIND_LATENT_STATE,
            profile.kind
        ));
    }
    Ok(())
}

/// Un seme fresco per il tentativo.
///
/// Fresco: due giri dello stesso modello vedono handle diversi, quindi il test non
/// e' memorizzabile (il needle fisso `NX7K2P9QW4` insegna il contrario — costante in
/// chiaro nel repo, nel DB e nei log, mandata a 8 provider: GPT-4-base recita il
/// GUID di BIG-bench).
///
/// Registrato: finisce in `ai_model_probe_evidence.seed`, e da li' il giro si rigioca
/// bit a bit. Un verdetto che non sai rigiocare non e' contestabile, quindi e'
/// un'opinione (regola O: un numero senza la sua premessa).
///
/// Il seme varia per ISTANZA, mai i PARAMETRI della banda (profondita' della catena,
/// tipo di guasto): stesso parametro = stessa difficolta', seme diverso = istanza mai
/// vista. E' la risposta della letteratura alla tensione freschezza/stabilita' (survey
/// contaminazione arXiv 2406.04244: i benchmark dinamici "mancano della garanzia di
/// risultati consistenti fra valutazioni successive").
fn seme_fresco() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        // Il nanosecondo da solo non basta: due tentativi ravvicinati possono
        // cadere nello stesso tick su Windows (granularita' ~100ns).
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// I kind in cui OGNI token del compito nasce dal seme. Solo li' il seme e' la
/// PREMESSA del verdetto e va registrato in `ai_model_probe_evidence.seed`: sugli
/// altri profili non c'e' niente da rigiocare, e un verdetto che non sai rigiocare
/// non e' contestabile, quindi e' un'opinione (regola O).
fn kind_deriva_dal_seme(kind: &str) -> bool {
    mondo_del_kind(kind).is_some() || kind == crate::probe_latent_state::KIND_LATENT_STATE
}

/// Il mondo che serve a un kind, se e' uno dei due multi-step.
fn mondo_del_kind(kind: &str) -> Option<crate::probe_world::WorldKind> {
    match kind {
        KIND_TOOL_CHAIN => Some(crate::probe_world::WorldKind::Catena),
        KIND_TOOL_RECOVERY => Some(crate::probe_world::WorldKind::Recupero),
        _ => None,
    }
}

/// L'istruzione del giro: nomina il SOLO bersaglio noto (l'handle di partenza), il
/// CUSTODE che seleziona la voce da seguire, e nient'altro.
///
/// Non nomina i tool — quale usare lo decide il modello — e non dice "concatena tre
/// chiamate": chiederlo esplicitamente misurerebbe l'obbedienza al nostro prompt
/// invece della capacita' di capire che, per arrivare in fondo, bisogna seguire i
/// riferimenti.
///
/// Il custode e' l'UNICO posto in cui il criterio compare: sparita la vecchia
/// formula "segui la voce marcata 'current'", che metteva la risposta dentro ogni
/// tool_result e rendeva vincente una ricerca di stringa. Qui il criterio va tenuto
/// a mente per tutto il giro.
///
/// Cio' che l'istruzione NON dice: che a un certo punto la pista si interrompe. Un
/// ostacolo annunciato misurerebbe l'obbedienza; questo va scoperto leggendo
/// l'errore, esattamente come il guasto del profilo di recupero.
fn istruzione_catena(handle: &str, custode: &str) -> String {
    format!(
        "Parti dalla risorsa {handle}. Ogni risorsa che leggi elenca delle voci, \
         ciascuna con un campo 'owner': prosegui attraverso la voce affidata al \
         custode {custode}, e continua finche' la pista non si esaurisce. Rispondi \
         con il riferimento finale."
    )
}

/// Il recupero non si annuncia: se il prompt dicesse "quando fallisce, leggi
/// l'errore", misureremmo l'obbedienza. Dice solo il compito.
fn istruzione_recupero(handle: &str) -> String {
    format!("Leggi la risorsa {handle} e riportane il contenuto.")
}

impl ProbeCtx<'_> {
    /// Un turno del loop. Gli errori del provider tornano DENTRO il Value (il
    /// produttore non li propaga come `Err`): il loop li legge da `error_class` e
    /// chiude inconclusivo.
    async fn turno_del_loop(&self, messages_json: &str) -> Value {
        let (tools_json, _, system_text) = self.request;
        match self
            .orchestrator
            .neural
            .generate_agent_turn_with_thinking(
                self.provider,
                self.model,
                messages_json,
                tools_json,
                self.params.max_tokens,
                system_text,
                None,
            )
            .await
        {
            Ok(v) => v,
            // Errore LOCALE (bridge non configurato, json invalido): non e' colpa
            // del modello, e il loop lo trattera' come inconclusivo.
            Err(e) => json!({ "error_class": "bridge_error", "stop_reason": "error",
                              "error": e.to_string(), "tool_use_blocks": [] }),
        }
    }

    /// Il mondo del tentativo e l'istruzione che lo apre. `Err` col motivo se il
    /// mondo non e' costruibile: e' colpa nostra, e il chiamante chiude inconclusivo.
    fn prepara_mondo(
        &self,
        kind: crate::probe_world::WorldKind,
        attempt: i32,
        seme: u64,
    ) -> Result<(crate::probe_world::ScriptedWorld, String), String> {
        use crate::probe_world::{ScriptedWorld, TokenSeed, WorldKind};
        let seed = TokenSeed {
            provider: self.provider.to_string(),
            model: self.model.to_string(),
            profile_key: self.profile.profile_key.clone(),
            attempt,
            // Fresco a ogni tentativo e REGISTRATO nell'evidenza (mig 0610): il giro
            // si rigioca identico da quella riga.
            seed: seme,
        };
        // L'anello di partenza e' l'UNICO che la richiesta nomina: gli altri il
        // modello deve guadagnarseli seguendo la catena.
        let handle0 = seed.handle(0);
        let istruzione = match kind {
            WorldKind::Catena => istruzione_catena(&handle0, &seed.custode()),
            WorldKind::Recupero => istruzione_recupero(&handle0),
        };
        let (_, _, system_text) = self.request;
        // L'INVARIANTE del needle, qui come guard: nessun token A VALLE puo' essere
        // gia' visibile nella richiesta, o la catena e' scorciatoiabile.
        let mondo = ScriptedWorld::new(kind, seed, &[&istruzione, system_text])?;
        Ok((mondo, istruzione))
    }

    /// UN tentativo multi-step: prepara il mondo, gira il loop, consegna i FATTI al
    /// predicato. Ogni uscita anticipata e' un inconclusivo, mai una bocciatura:
    /// il modello risponde di cio' che fa, non dei nostri cap ne' dei provider caduti.
    async fn multi_step_attempt(
        &self,
        kind: crate::probe_world::WorldKind,
        attempt: i32,
        seme: u64,
    ) -> (AttemptOutcome, i64) {
        let started = std::time::Instant::now();
        let ms = || started.elapsed().as_millis() as i64;

        let (mut mondo, istruzione) = match self.prepara_mondo(kind, attempt, seme) {
            Ok(v) => v,
            Err(motivo) => return esito_inconclusivo(format!("mondo_non_costruibile:{motivo}"), ms()),
        };
        let max_turns = self
            .profile
            .payload
            .get("max_turns")
            .and_then(Value::as_i64)
            .unwrap_or(6) as usize;

        let esito = tokio::time::timeout(
            Duration::from_secs(self.params.timeout_s),
            crate::probe_agentic_loop::run_loop(self, kind, &mut mondo, &istruzione, max_turns),
        )
        .await;
        let latency_ms = ms();

        let Ok(out) = esito else {
            return esito_inconclusivo(
                format!("probe_timeout:{}s", self.params.timeout_s),
                latency_ms,
            );
        };
        if let Some(motivo) = out.inconclusive {
            return esito_inconclusivo(motivo, latency_ms);
        }
        (
            verdetto_dai_fatti(&out.measures, &self.profile.pass_predicate, latency_ms),
            latency_ms,
        )
    }

    /// UN tentativo di `latent_state`: costruisce l'istanza fresca dal seme, la manda,
    /// consegna i FATTI al predicato. Come per il multi-step, ogni uscita anticipata
    /// e' un inconclusivo e mai una bocciatura: il modello risponde di cio' che fa,
    /// non dei nostri cap ne' dei provider caduti.
    async fn latent_state_attempt(&self, attempt: i32, seme: u64) -> (AttemptOutcome, i64) {
        let started = std::time::Instant::now();
        let parametri = crate::probe_latent_state::ParametriGiro {
            provider: self.provider,
            model: self.model,
            profile_key: &self.profile.profile_key,
            attempt,
            seme,
            payload: &self.profile.payload,
        };
        let esito = tokio::time::timeout(
            Duration::from_secs(self.params.timeout_s),
            crate::probe_latent_state::tentativo(self, parametri),
        )
        .await;
        let latency_ms = started.elapsed().as_millis() as i64;
        match esito {
            Ok(Ok(turno)) => (
                evaluate_attempt(&turno, &self.profile.pass_predicate, latency_ms),
                latency_ms,
            ),
            Ok(Err(motivo)) => esito_inconclusivo(motivo, latency_ms),
            Err(_elapsed) => esito_inconclusivo(
                format!("probe_timeout:{}s", self.params.timeout_s),
                latency_ms,
            ),
        }
    }
}

/// L'`AttemptOutcome` di un esito NON attribuibile al modello (transient, cap
/// nostro, timeout): pass/fail entrambi falsi, nessun segnale del modello. Punto
/// unico del literal (regola L): lo costruivano a mano sia `esito_inconclusivo`
/// che il ramo timeout di `single_attempt`, e i due literal identici erano una
/// duplicazione (un campo in piu' a entrambi li faceva superare la soglia del
/// detector) — ora esiste in un solo posto.
fn outcome_inconclusivo(reason: String) -> AttemptOutcome {
    AttemptOutcome {
        pass: false,
        inconclusive: true,
        reason,
        error_class: None,
        tool_call_count: 0,
        content_chars: 0,
        stop_reason: String::new(),
        derived: None,
    }
}

/// Un esito inconclusivo con la latenza gia' misurata. Esiste come funzione sola
/// perche' era una chiusura dentro `multi_step_attempt`, e un esito che il
/// chiamante deve poter produrre da quattro punti diversi non e' un dettaglio di
/// quella funzione.
fn esito_inconclusivo(reason: String, ms: i64) -> (AttemptOutcome, i64) {
    (outcome_inconclusivo(reason), ms)
}

/// I fatti misurati, nella forma che il predicato sa leggere. Il turno e' sintetico
/// apposta: il verdetto guarda `measures`, MAI la prosa del modello (regola M).
fn turno_dai_fatti(m: &crate::probe_chain_measure::AttemptMeasures) -> Value {
    json!({
        "content": "",
        "stop_reason": "end_turn",
        "tool_use_blocks": [],
        "measures": {
            F_CHAINED_LINKS: m.chained_links,
            F_RECOVERED: m.recovered,
            F_REPEATED_FAILED: m.repeated_failed,
            F_BAD_TOOL_SYNTAX: m.bad_tool_syntax,
        }
    })
}

/// Dai FATTI del loop al VERDETTO, per la STESSA strada che percorre la produzione
/// (regola L: un solo punto per il passaggio misura->verdetto dei profili
/// multi-step; regola O: il test del giro completo delega QUI, non ricostruisce la
/// sequenza a mano). Aggancia le `measures` alla colonna diagnostica `derived`:
/// senza, un verdetto `no_recovery`/`no_chain` non porta con se' i fatti che l'hanno
/// deciso e non e' contestabile — ed e' esattamente cio' che ha reso invisibile la
/// causa dei 93 fail del 2026-07-17 (il modello si arrende, non un bug di misura).
fn verdetto_dai_fatti(
    m: &crate::probe_chain_measure::AttemptMeasures,
    predicate: &Value,
    latency_ms: i64,
) -> AttemptOutcome {
    let turno = turno_dai_fatti(m);
    let mut out = evaluate_attempt(&turno, predicate, latency_ms);
    out.derived = turno.get("measures").cloned();
    out
}

impl crate::probe_agentic_loop::TurnSource for ProbeCtx<'_> {
    async fn turn(&self, messages_json: &str) -> Value {
        self.turno_del_loop(messages_json).await
    }
}

async fn build_profile_request(
    db: &PgPool,
    profile: &ProbeProfile,
) -> Result<(String, String, String), String> {
    // PRIMA di costruire la richiesta: se il predicato chiede una verifica che
    // nessuno sa fare, il profilo NON e' costruibile. Fallire qui produce un giro
    // inconclusivo (colpa nostra, stato del modello invariato); tacere
    // produrrebbe una promozione gratuita.
    predicato_coerente_col_kind(profile)?;
    match profile.kind.as_str() {
        "chat" => Ok((
            "[]".to_string(),
            json!([{ "role": "user",
                     "content": "Verifica operativa: rispondi con la sola parola: ok" }])
            .to_string(),
            "Sei in una verifica di raggiungibilita'. Rispondi in modo conciso.".to_string(),
        )),
        "tool_minimal" => Ok(crate::model_health_probe::build_tool_probe_request()),
        KIND_TOOL_REALISTIC | "thinking_matrix" => {
            let tools = resolve_probe_tools(profile)?;
            let system = resolve_probe_system(db, profile).await?;
            Ok((
                tools.to_string(),
                build_realistic_messages(profile),
                system,
            ))
        }
        KIND_LONG_CONTEXT => Ok(build_long_context_request(profile)),
        // `latent_state` (frontier): niente tool — il compito e' leggere, non agire.
        // I messaggi li costruisce `latent_state_attempt`, che conosce il seme e
        // quindi i codici: il registro e' un'ISTANZA fresca, non un testo fisso.
        crate::probe_latent_state::KIND_LATENT_STATE => Ok((
            "[]".to_string(),
            String::new(),
            crate::probe_latent_state::system_text(),
        )),
        // I due multi-step: qui si costruisce solo la cornice (tool + system).
        // L'istruzione la completa `multi_step_attempt`, che conosce il seed e
        // quindi l'handle di partenza; il resto della conversazione lo costruisce
        // il loop, turno per turno, con le risposte del mondo finto.
        KIND_TOOL_CHAIN | KIND_TOOL_RECOVERY => {
            let tools = resolve_probe_tools(profile)?;
            let system = resolve_probe_system(db, profile).await?;
            Ok((tools.to_string(), String::new(), system))
        }
        other => Err(format!("kind profilo non implementato: {other}")),
    }
}

/// Registra un tentativo in `ai_model_probe_evidence` e ritorna l'id.
#[allow(clippy::too_many_arguments)]
/// Registra un tentativo. Il `seme` e' la PREMESSA del verdetto: senza, un
/// fallimento contestato non si rigioca e quindi non si contesta (regola O). Vale
/// solo per i profili multi-step, che dal seme derivano ogni token; per gli altri
/// e' `None`.
async fn insert_evidence(
    db: &PgPool,
    provider: &str,
    model: &str,
    profile: &ProbeProfile,
    attempt: i32,
    latency_ms: i64,
    outcome: &AttemptOutcome,
    seme: Option<i64>,
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
          tool_call_count, content_chars, stop_reason, verdict, verdict_reason, seed, derived) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",
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
    .bind(seme)
    // I FATTI dietro il verdetto (regola O): la colonna `derived` (mig 0591) restava
    // NULL per costruzione — nessun writer la toccava — quindi un `no_recovery` non
    // portava con se' il fatto che l'aveva deciso e non era diagnosticabile.
    .bind(&outcome.derived)
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
        attempt: i32,
        seme: u64,
    ) -> (AttemptOutcome, i64) {
        // I due kind multi-step non stanno in un turno solo: hanno bisogno del
        // loop, del mondo finto e del taint tracking. E' l'unico punto in cui il
        // giro si biforca.
        if let Some(kind) = mondo_del_kind(&self.profile.kind) {
            return self.multi_step_attempt(kind, attempt, seme).await;
        }
        // `latent_state` sta in un turno solo, ma il suo compito e' un'istanza fresca
        // che nasce dal seme: non puo' venire da `build_profile_request`.
        if self.profile.kind == crate::probe_latent_state::KIND_LATENT_STATE {
            return self.latent_state_attempt(attempt, seme).await;
        }
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
            Err(_elapsed) => {
                outcome_inconclusivo(format!("probe_timeout:{}s", self.params.timeout_s))
            }
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
            return; // niente somme: il silenzio non entra nello score
        }
        if outcome.pass {
            self.passes += 1;
        } else {
            self.conclusive_fails += 1;
            if self.first_fail_reason.is_none() {
                self.first_fail_reason = Some(outcome.reason.clone());
            }
        }
        // Le somme continue, dai FATTI del tentativo (le `measures` che
        // `verdetto_dai_fatti` aggancia a `derived`): sui conclusivi soltanto,
        // e solo dove esistono (i single-turn non ne hanno).
        let Some(m) = outcome.derived.as_ref() else {
            return;
        };
        let links = m.get(F_CHAINED_LINKS).and_then(Value::as_f64).unwrap_or(0.0);
        self.somme.chain_frac += (links / LINKS_TARGET).clamp(0.0, 1.0);
        for (fatto, conto) in [
            (F_RECOVERED, &mut self.somme.recovered),
            (F_REPEATED_FAILED, &mut self.somme.repeated),
            (F_BAD_TOOL_SYNTAX, &mut self.somme.bad_syntax),
        ] {
            if m.get(fatto).and_then(Value::as_bool) == Some(true) {
                *conto += 1;
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
        kind: ctx.profile.kind.clone(),
        grants: ctx.profile.grants.clone(),
        is_blocking: ctx.profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: 0,
        promote_min: ctx.params.promote_min,
        first_fail_reason: None,
        somme: SommeConclusive::default(),
    };
    for attempt in 1..=ctx.params.repeat {
        // Un seme per TENTATIVO: i 4 tentativi campionano istanze diverse invece di
        // ripetere la stessa (FLenQA misura che una posizione fissa e' la peggiore:
        // ripetere 4 volte lo stesso caso fa passare i borderline per fortuna).
        let seme = seme_fresco();
        let (mut outcome, latency_ms) =
            ctx.single_attempt(thinking.clone(), attempt as i32, seme).await;
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
            // Il seme e' la premessa del verdetto, e vale solo dove ogni token ne
            // deriva: sugli altri profili non c'e' niente da rigiocare.
            kind_deriva_dal_seme(&ctx.profile.kind).then_some(seme as i64),
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
        kind: profile.kind.clone(),
        grants: profile.grants.clone(),
        is_blocking: profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: params.repeat,
        promote_min: params.promote_min,
        first_fail_reason: None,
        somme: SommeConclusive::default(),
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

/// LO SCORE MISURATO (mig 0616). Solo su un esito QUALIFIED, come il tier: una
/// squalifica o un giro inconclusivo non misurano nulla. Pesi dal DB (regola G):
/// chiavi assenti = nessuno score, con un WARN che dice quale migrazione manca.
async fn attach_measured_score(
    derived: &mut Derived,
    runs: &[ProfileRun],
    profiles: &[ProbeProfile],
    declared: &[String],
    db: &PgPool,
) {
    if derived.state != DerivedState::Qualified {
        return;
    }
    let Some(w) = measured_score_weights(db).await else {
        tracing::warn!(
            "measured_score: pesi 'catalog.measured_score.*' assenti o non numerici \
             (applicare la migrazione #0616): score non calcolato"
        );
        return;
    };
    let kinds: Vec<&str> = profiles
        .iter()
        .filter(|p| profile_applies(p, declared))
        .map(|p| p.kind.as_str())
        .collect();
    derived.measured_score = derive_measured_score(runs, &kinds, &w);
}

/// I pesi della formula dal DB. `None` = una chiave manca o non e' un numero:
/// fail-visibile, mai un default hardcoded (regola G).
async fn measured_score_weights(db: &PgPool) -> Option<MeasuredScoreWeights> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key LIKE 'catalog.measured_score.%'",
    )
    .fetch_all(db)
    .await
    .map_err(|e| tracing::warn!(error = %e, "measured_score_weights: lettura fallita"))
    .ok()?;
    let map: std::collections::HashMap<_, _> = rows.into_iter().collect();
    let num = |k: &str| -> Option<f64> { map.get(k)?.trim().parse().ok() };
    Some(MeasuredScoreWeights {
        chain: num("catalog.measured_score.w_chain")?,
        recovery: num("catalog.measured_score.w_recovery")?,
        real: num("catalog.measured_score.w_real")?,
        latent: num("catalog.measured_score.w_latent")?,
        longctx: num("catalog.measured_score.w_longctx")?,
    })
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
    attach_measured_score(&mut derived, &runs, profiles, declared, db).await;
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
        self.scrivi_tier_e_score(&mut tx).await?;
        tx.commit().await?;
        Ok(res)
    }

    /// Tier e SCORE misurati, dentro la transazione del verdetto. `None` su
    /// entrambi = giro che non dimostra nulla: le colonne restano quelle di
    /// ieri (regola H: il silenzio non declassa e non punteggia).
    async fn scrivi_tier_e_score(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), sqlx::Error> {
        use crate::orchestrator::model_service::{apply_measured_score, apply_tier, TierSource};
        if let Some(tier) = self.derived.measured_tier.as_deref() {
            apply_tier(&mut **tx, self.provider, self.model, tier, TierSource::Measured).await?;
        }
        if let Some(score) = self.derived.measured_score {
            apply_measured_score(&mut **tx, self.provider, self.model, score, self.profiles_suite)
                .await?;
        }
        Ok(())
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
    let max_per_round =
        setting_i64(db, KEY_MAX_PER_ROUND, nexus_model_eligibility::DEFAULT_MAX_PER_ROUND).await;
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
        suite_version: nexus_model_eligibility::current_suite_version(
            profiles.iter().map(|p| p.suite_version),
        ),
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
    // Ri-ancoraggio bande measured (mig 0616): a fine giro, idempotente.
    riancora_bande_measured(db, cfg.suite_version).await;
    done
}

/// La configurazione delle bande measured (settings `catalog.measured_band.*`,
/// mig 0616). `None` = chiavi assenti: il pass non parte (fail-visibile).
struct MeasuredBandConfig {
    deadband_pct: f64,
    demote_margin: f64,
    min_population: usize,
}

async fn measured_band_config(db: &PgPool) -> Option<MeasuredBandConfig> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key LIKE 'catalog.measured_band.%'",
    )
    .fetch_all(db)
    .await
    .map_err(|e| tracing::warn!(error = %e, "measured_band_config: lettura fallita"))
    .ok()?;
    let map: std::collections::HashMap<_, _> = rows.into_iter().collect();
    let num = |k: &str| -> Option<f64> { map.get(k)?.trim().parse().ok() };
    Some(MeasuredBandConfig {
        deadband_pct: num("catalog.measured_band.anchor_deadband_pct")?,
        demote_margin: num("catalog.measured_band.demote_margin")?,
        min_population: num("catalog.measured_band.min_population")? as usize,
    })
}

/// RI-ANCORA le bande measured sull'ultimo parco di score (mig 0616).
///
/// SEMANTICA NUOVA, dichiarata: il tier di un modello puo' muoversi SENZA che
/// quel modello sia stato ri-provato — si e' mosso il leader, e la scala e'
/// relativa al parco. Il leader si calcola SOLO fra righe alla suite corrente
/// (score di suite diverse non sono confrontabili); sotto `min_population`
/// modelli misurati le bande NON si applicano (senza questa soglia il primo
/// misurato di ogni suite sarebbe frontier per definizione) e il tier resta
/// `synced`. La precedenza delle fonti resta di [`apply_tier`]: la curatela
/// `manual` non si tocca.
async fn riancora_bande_measured(db: &PgPool, suite: i32) {
    let Some(bands) = crate::orchestrator::model_service::relative_bands(db, "catalog.measured_band").await else {
        tracing::warn!(
            "riancora_bande_measured: percentuali 'catalog.measured_band.*_pct' assenti \
             (applicare la migrazione #0617): bande measured non applicate"
        );
        return;
    };
    let Some(cfg) = measured_band_config(db).await else {
        tracing::warn!(
            "riancora_bande_measured: config 'catalog.measured_band.*' assente \
             (applicare la migrazione #0616): bande measured non applicate"
        );
        return;
    };
    let righe = leggi_score_suite(db, suite).await;
    if righe.len() < cfg.min_population {
        tracing::debug!(
            misurati = righe.len(),
            min_population = cfg.min_population,
            suite = suite,
            "riancora_bande_measured: popolazione sotto soglia, bande non applicate \
             (il tier resta synced)"
        );
        return;
    }
    let Some(ancora) = ancora_measured_aggiornata(db, &righe, &cfg).await else {
        return;
    };
    applica_bande_measured(db, &righe, ancora, &bands, cfg.demote_margin).await;
}

/// Una riga misurata a suite corrente: (provider, model, score, tier, fonte).
type RigaMisurata = (String, String, f64, Option<String>, Option<String>);

/// Le righe con uno score alla suite corrente: il PERIMETRO del ri-ancoraggio
/// (score di suite diverse non sono confrontabili).
async fn leggi_score_suite(db: &PgPool, suite: i32) -> Vec<RigaMisurata> {
    sqlx::query_as(
        "SELECT provider, model, measured_score, performance_tier, tier_source \
           FROM ai_price_catalog \
          WHERE measured_score IS NOT NULL AND measured_score_suite = $1",
    )
    .bind(suite)
    .fetch_all(db)
    .await
    .map_err(|e| tracing::warn!(error = %e, "riancora_bande_measured: lettura score fallita"))
    .unwrap_or_default()
}

/// L'ancora measured effettiva: il massimo score del perimetro, passato per la
/// deadband contro l'ancora persistita (e persistito se lo scarto la supera).
async fn ancora_measured_aggiornata(
    db: &PgPool,
    righe: &[RigaMisurata],
    cfg: &MeasuredBandConfig,
) -> Option<f64> {
    use crate::orchestrator::model_service::{persist_anchor, resolve_anchor};
    let leader = righe
        .iter()
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|r| (format!("{}/{}", r.0, r.1), r.2))?;
    let attuale = leggi_ancora_measured(db).await;
    let (ancora, persisti) = resolve_anchor(attuale, leader.1, cfg.deadband_pct);
    if persisti {
        tracing::info!(ancora = ancora, leader = %leader.0,
            "scala relativa: nuova ancora delle bande measured (deadband superata)");
        persist_anchor(db, "catalog.measured_band", ancora, &leader.0).await;
    }
    Some(ancora)
}

/// Applica la banda relativa a ogni riga del perimetro. La precedenza delle
/// fonti resta di `apply_tier`: la curatela `manual` non si tocca.
async fn applica_bande_measured(
    db: &PgPool,
    righe: &[RigaMisurata],
    ancora: f64,
    bands: &crate::orchestrator::model_service::RelativeBands,
    demote_margin: f64,
) {
    use crate::orchestrator::model_service::{apply_tier, TierSource};
    for (provider, model, score, tier, source) in righe {
        let acquisita = (source.as_deref() == Some("measured"))
            .then_some(tier.as_deref())
            .flatten();
        let banda = banda_measured(*score, acquisita, ancora, bands, demote_margin);
        if let Err(e) = apply_tier(db, provider, model, banda, TierSource::Measured).await {
            tracing::warn!(provider = %provider, model = %model, error = %e,
                "riancora_bande_measured: apply_tier fallita");
        }
    }
}

/// L'ancora measured persistita, se numerica e positiva.
async fn leggi_ancora_measured(db: &PgPool) -> Option<f64> {
    crate::settings::get_setting(db, "catalog.measured_band.anchor")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
}

/// I parametri del giro, letti una volta dal DB (regola G).
struct RoundConfig {
    suite_version: i32,
    ttl_days: i64,
    backoff_hours: i64,
}

/// I candidati del giro, reclamati col CAS di `nexus_model_eligibility::sql_claim`.
///
/// La REGOLA (chi e' eleggibile: catalog abilitato, tool use dichiarato, backoff
/// scaduto, lock libero, da rimisurare, provider senza cooldown) vive nel crate
/// come punto unico, insieme al perche' di ogni condizione. Non e' cortesia
/// architetturale: `xtask battery-explain` compone la SUA query dalle stesse
/// condizioni, quindi la diagnosi risponde sempre sulla regola che gira davvero
/// qui (regola O). Una copia — cioe' com'era prima — divergeva in silenzio.
async fn claim_candidates(
    db: &PgPool,
    max_per_round: i64,
    suite_version: i32,
) -> Vec<(String, String, Value)> {
    sqlx::query_as(&nexus_model_eligibility::sql_claim())
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
                measured_score: None,
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
            kind: "test".into(),
            grants: grants.iter().map(|s| s.to_string()).collect(),
            is_blocking: blocking,
            passes,
            conclusive_fails: fails,
            inconclusive,
            promote_min,
            first_fail_reason: first_fail.map(str::to_owned),
            somme: SommeConclusive::default(),
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
    // ── Il vocabolario CHIUSO dei predicati ─────────────────────────────────

    fn profilo(kind: &str, predicate: Value) -> ProbeProfile {
        ProbeProfile {
            profile_key: format!("test_{kind}"),
            suite_version: 2,
            kind: kind.to_string(),
            is_blocking: false,
            applies_when: None,
            grants: vec![],
            payload: json!({}),
            pass_predicate: predicate,
            certifies_tier: None,
        }
    }

    /// IL FAIL-APERTO, che e' il difetto peggiore: un predicato che nessuno
    /// verifica non e' un vincolo, e `predicate_fail_reason` con i suoi default
    /// permissivi ritorna `None` = "superato". Un profilo che non misura niente e
    /// promuove tutti e' indistinguibile da un profilo che funziona.
    #[test]
    fn un_predicato_sconosciuto_e_un_errore_non_un_vincolo_ignorato() {
        // La prova del fail-aperto: oggi il verificatore TACE su una chiave ignota
        // e dichiara il tentativo superato.
        let sig = read_turn_signals(&json!({"content": "x", "tool_use_blocks": []}));
        assert_eq!(
            predicate_fail_reason(&json!({}), &json!({"min_unicorni": 3}), &sig, 10),
            None,
            "il verificatore non conosce 'min_unicorni' e lo ignora: senza il \
             vocabolario chiuso, quel profilo promuoverebbe chiunque"
        );
        // Per questo il profilo dev'essere rifiutato PRIMA di girare.
        assert_eq!(
            predicato_coerente_col_kind(&profilo("chat", json!({"min_unicorni": 3}))),
            Err("predicato sconosciuto: min_unicorni".to_string())
        );
    }

    /// Le due meta' del contratto stanno in colonne diverse del DB e possono
    /// divergere: un predicato di catena su un profilo single-turn sarebbe muto
    /// (nessun anello da contare), quindi promuoverebbe senza misurare.
    #[test]
    fn un_predicato_di_catena_su_un_profilo_single_turn_e_un_errore() {
        let e = predicato_coerente_col_kind(&profilo("chat", json!({"min_chained_calls": 3})));
        assert!(
            e.is_err() && e.unwrap_err().contains("multi-turno"),
            "min_chained_calls su kind 'chat' non e' verificabile: va rifiutato"
        );
        // Sul kind giusto, invece, e' legittimo.
        assert!(
            predicato_coerente_col_kind(&profilo("tool_chain", json!({"min_chained_calls": 3})))
                .is_ok()
        );
    }

    /// Stessa trappola per lo stato latente: `requires_final_state` su un kind che non
    /// produce misure non verificherebbe NIENTE e regalerebbe `frontier`, che e' il
    /// vertice. Il refuso va rifiutato prima che il profilo giri.
    #[test]
    fn requires_final_state_su_un_kind_qualunque_e_un_errore() {
        let k = crate::probe_latent_state::K_REQUIRES_FINAL_STATE;
        let e = predicato_coerente_col_kind(&profilo("chat", json!({ k: true })));
        assert!(
            e.is_err() && e.unwrap_err().contains("latent_state"),
            "requires_final_state su kind 'chat' e' muto: va rifiutato"
        );
        assert!(predicato_coerente_col_kind(&profilo(
            crate::probe_latent_state::KIND_LATENT_STATE,
            json!({ k: true })
        ))
        .is_ok());
    }

    /// I predicati dei profili VERI del DB devono essere tutti coperti: e' il
    /// controllo che impedisce a un profilo aggiunto dall'admin di girare a vuoto.
    #[test]
    fn il_vocabolario_copre_i_predicati_dei_profili_reali() {
        for (kind, pred) in [
            ("chat", json!({"min_content_chars": 1})),
            ("tool_minimal", json!({"min_tool_calls": 1})),
            (
                "tool_realistic",
                json!({"max_latency_ms": 60000, "min_tool_calls": 1,
                       "hold_min_passes": 2, "promote_min_passes": 3}),
            ),
            (
                "tool_chain",
                json!({"max_latency_ms": 120000, "hold_min_passes": 2,
                       "min_chained_calls": 3, "promote_min_passes": 3}),
            ),
            (
                "tool_recovery",
                json!({"max_latency_ms": 120000, "hold_min_passes": 2,
                       "requires_recovery": true, "promote_min_passes": 3,
                       "forbids_repeat_of_failed": true}),
            ),
            (
                "long_context",
                json!({"max_latency_ms": 180000, "hold_min_passes": 2,
                       "requires_needle": true, "promote_min_passes": 3}),
            ),
            // `agentic_latent_state` (mig 0611): il predicato e' quello della riga.
            (
                crate::probe_latent_state::KIND_LATENT_STATE,
                json!({"requires_final_state": true, "promote_min_passes": 4,
                       "hold_min_passes": 3, "max_latency_ms": 120000}),
            ),
        ] {
            assert!(
                predicato_coerente_col_kind(&profilo(kind, pred)).is_ok(),
                "il profilo '{kind}' del DB deve essere accettato dal vocabolario"
            );
        }
    }

    // ── I predicati multi-step, agganciati ai fatti del loop ────────────────

    /// Un turno multi-step come il loop lo consegna: i fatti stanno sotto
    /// `measures`, ed e' li' che il predicato li legge.
    fn turno_con_misure(chained: i64, recovered: bool, repeated: bool) -> Value {
        json!({
            "content": "", "stop_reason": "end_turn", "tool_use_blocks": [],
            "measures": {
                "chained_links": chained,
                "recovered": recovered,
                "repeated_failed": repeated,
                "bad_tool_syntax": false
            }
        })
    }

    /// `min_chained_calls: 3` boccia chi ha concatenato meno di 3 anelli, e i
    /// "3 anelli" sono token nostri riportati — non chiamate emesse.
    #[test]
    fn il_predicato_della_catena_boccia_chi_non_concatena() {
        let pred = json!({ "min_chained_calls": 3 });
        let sig = read_turn_signals(&turno_con_misure(0, false, false));
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(0, false, false), &pred, &sig, 10),
            Some("no_chain:0<3".to_string()),
            "zero anelli: il profilo non certifica high"
        );
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(2, false, false), &pred, &sig, 10),
            Some("no_chain:2<3".to_string()),
            "due anelli su tre: sotto soglia"
        );
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(3, false, false), &pred, &sig, 10),
            None,
            "tre anelli: superato"
        );
    }

    /// `requires_recovery` chiede il FATTO, non le buone intenzioni: senza il token
    /// dell'errore non c'e' recupero, per quanto bella sia la prosa.
    #[test]
    fn il_predicato_del_recupero_chiede_il_token_non_le_scuse() {
        let pred = json!({ "requires_recovery": true });
        let sig = read_turn_signals(&turno_con_misure(0, false, false));
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(0, false, false), &pred, &sig, 10),
            Some("no_recovery".to_string())
        );
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(0, true, false), &pred, &sig, 10),
            None
        );
    }

    /// `forbids_repeat_of_failed` boccia chi rimanda identica una chiamata gia'
    /// fallita. Vale solo dove il guasto e' PERMANENTE: su uno transitorio ritentare
    /// e' la mossa giusta, ed e' quella che il nostro stesso prompt ordina.
    #[test]
    fn il_predicato_della_ripetizione_boccia_chi_insiste_identico() {
        let pred = json!({ "forbids_repeat_of_failed": true });
        let sig = read_turn_signals(&turno_con_misure(0, true, true));
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(0, true, true), &pred, &sig, 10),
            Some("repeated_failed".to_string()),
            "ha recuperato ma insistendo identico prima: il profilo lo dice"
        );
        assert_eq!(
            predicate_fail_reason(&turno_con_misure(0, true, false), &pred, &sig, 10),
            None
        );
    }

    /// LA REGRESSIONE CHE CONTA: un turno SENZA `measures` (single-turn) non deve
    /// superare un predicato di catena per silenzio. Prima dell'aggancio,
    /// `min_chained_calls` non veniva letto da nessuno e il profilo prometteva
    /// `high` a chiunque rispondesse.
    #[test]
    fn un_turno_senza_misure_non_supera_un_predicato_di_catena() {
        let turno = json!({ "content": "ok", "stop_reason": "end_turn", "tool_use_blocks": [] });
        let sig = read_turn_signals(&turno);
        assert_eq!(
            predicate_fail_reason(&turno, &json!({ "min_chained_calls": 3 }), &sig, 10),
            Some("no_chain:0<3".to_string()),
            "nessuna misura = nessun anello provato: mai un pass per assenza di dati"
        );
        assert_eq!(
            predicate_fail_reason(&turno, &json!({ "requires_recovery": true }), &sig, 10),
            Some("no_recovery".to_string())
        );
    }

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
    fn bande() -> crate::orchestrator::model_service::RelativeBands {
        // La scala relativa dal seed (mig 0615).
        crate::orchestrator::model_service::RelativeBands {
            frontier_pct: 0.85,
            heavy_pct: 0.65,
            high_pct: 0.45,
            medium_pct: 0.20,
        }
    }

    /// L'ANCORA del parco reale al 2026-07-19 (openai/gpt-5.6-sol, indice 54.0):
    /// e' il valore con cui la scala relativa riproduce quasi esattamente le
    /// vecchie soglie assolute 45/35/25/10 (45.9/35.1/24.3/10.8).
    const ANCORA_54: f64 = 54.0;

    /// L'agentic_index e' l'UNICO seme del tier `synced` (mig 0608): MISURA la
    /// capacita' agentica, mentre il prezzo era il posizionamento commerciale
    /// del fornitore e il nome un aggettivo di marketing. Dalla mig 0615 la
    /// banda e' RELATIVA al leader (`tier_from_leader`, punto unico).
    ///
    /// I due casi REALI che il prezzo sbagliava (misurati sul parco il 16/07):
    /// gpt-5.4-mini costa da heavy (>$2) ma vale 30.2 -> e' 'high', non 'heavy';
    /// claude-opus-4-8 vale 47.2 -> resta 'frontier', il prezzo lo declassava.
    #[test]
    fn l_agentic_index_e_il_solo_seme_del_tier() {
        // gpt-5.4-mini: un MINI caro non e' heavy, l'indice dice la verita'.
        assert_eq!(derive_tier_prior(Some(30.2), ANCORA_54, &bande()), Some("high"));
        // claude-opus-4-8: 47.2 -> frontier (il prezzo lo faceva scendere a heavy).
        assert_eq!(derive_tier_prior(Some(47.2), ANCORA_54, &bande()), Some("frontier"));
        // gpt-5.6-sol: il leader e' il 100% di se stesso.
        assert_eq!(derive_tier_prior(Some(54.0), ANCORA_54, &bande()), Some("frontier"));
        // mistral-large-2512: vale poco -> light, qualunque cosa costi.
        assert_eq!(derive_tier_prior(Some(5.5), ANCORA_54, &bande()), Some("light"));
        // I BORDI DICHIARATI della scala relativa: 35.0 sta in [35.0, 35.1) e
        // 10.0 in [10.0, 10.8), le due finestre in cui le soglie relative
        // (65%/20% di 54) sono PIU' ALTE delle vecchie assolute. Erano 'heavy'
        // e 'medium'; ora 'high' e 'light'. E' la tolleranza sui bordi del
        // cambio di scala, quantificata sul DB vivo: 5 modelli su 79.
        assert_eq!(derive_tier_prior(Some(35.0), ANCORA_54, &bande()), Some("high"));
        assert_eq!(derive_tier_prior(Some(10.0), ANCORA_54, &bande()), Some("light"));
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
        assert_eq!(derive_tier_prior(None, ANCORA_54, &bande()), None);
    }

    /// Un giro in cui i tentativi non superati sono BOCCIATURE vere: il modello ha
    /// risposto e non ha fatto il lavoro. E' l'unico caso che nega una banda.
    fn run(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
        run_conclusive(key, passes, promote_min)
    }

    fn run_conclusive(key: &str, passes: u32, promote_min: u32) -> ProfileRun {
        ProfileRun {
            profile_key: key.to_string(),
            kind: "test".into(),
            grants: vec![],
            is_blocking: false,
            passes,
            conclusive_fails: 4 - passes,
            inconclusive: 0,
            promote_min,
            first_fail_reason: None,
            somme: SommeConclusive::default(),
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
            kind: "test".into(),
            grants: vec![],
            is_blocking: false,
            passes,
            conclusive_fails: 0,
            inconclusive: 4 - passes,
            promote_min,
            first_fail_reason: None,
            somme: SommeConclusive::default(),
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
    // ── Lo SCORE MISURATO: la simulazione del piano sul giro reale ──────────

    fn pesi() -> MeasuredScoreWeights {
        // I pesi VERI della produzione (mig 0620), non quelli del seed: un test
        // che simula "il giro reale" con pesi che nessuno usa piu' misurerebbe
        // una formula immaginaria (regola O). chain 12 perche' e' diventata
        // commodity, recovery 45 perche' e' il solo che apre il ventaglio,
        // longctx 0 perche' il profilo e' spento e un peso che nessuno puo'
        // prendere comprime la scala invece di ordinarla.
        MeasuredScoreWeights { chain: 12.0, recovery: 45.0, real: 18.0, latent: 25.0, longctx: 0.0 }
    }

    /// I kind che la suite 4 reale poteva correre: long_context NON c'e'
    /// (profilo disabilitato), quindi vale 0 punti per tutti, senza
    /// rinormalizzare.
    fn kinds_suite_4() -> Vec<&'static str> {
        vec![
            "chat",
            "tool_minimal",
            KIND_TOOL_REALISTIC,
            KIND_TOOL_CHAIN,
            KIND_TOOL_RECOVERY,
            crate::probe_latent_state::KIND_LATENT_STATE,
        ]
    }

    fn fatti(links: usize, rec: bool, rep: bool, bad: bool) -> crate::probe_chain_measure::AttemptMeasures {
        crate::probe_chain_measure::AttemptMeasures {
            chained_links: links,
            recovered: rec,
            repeated_failed: rep,
            bad_tool_syntax: bad,
        }
    }

    /// Un run multi-step costruito per la STESSA strada della produzione
    /// (regola O): misure -> `verdetto_dai_fatti` (il predicato REALE del
    /// profilo, mig 0610) -> `tally`. Niente pass/fail fabbricati: li decide il
    /// verificatore vero, e le somme continue le accumula il produttore vero.
    fn run_dai_fatti(
        kind: &str,
        predicato: &Value,
        misure: &[crate::probe_chain_measure::AttemptMeasures],
    ) -> ProfileRun {
        let mut run = ProfileRun {
            profile_key: kind.to_string(),
            kind: kind.to_string(),
            grants: vec![],
            is_blocking: false,
            passes: 0,
            conclusive_fails: 0,
            inconclusive: 0,
            promote_min: 3,
            first_fail_reason: None,
            somme: SommeConclusive::default(),
        };
        for m in misure {
            run.tally(&verdetto_dai_fatti(m, predicato, 1_000));
        }
        run
    }

    /// Un run single-turn coi CONTEGGI del giro reale (i verdetti verbatim da
    /// `ai_model_probe_evidence`; il percorso turno->verdetto dei single-turn e'
    /// gia' coperto dai test dei rispettivi probe).
    fn run_conteggi(kind: &str, passes: u32, fails: u32) -> ProfileRun {
        ProfileRun {
            profile_key: kind.to_string(),
            kind: kind.to_string(),
            grants: vec![],
            is_blocking: false,
            passes,
            conclusive_fails: fails,
            inconclusive: 0,
            promote_min: 3,
            first_fail_reason: None,
            somme: SommeConclusive::default(),
        }
    }

    /// I predicati REALI dei due profili multi-step (mig 0610, suite 4).
    fn predicato_catena() -> Value {
        json!({ "max_latency_ms": 120000, "hold_min_passes": 2,
                "min_chained_calls": 3, "promote_min_passes": 3 })
    }
    fn predicato_recupero() -> Value {
        json!({ "max_latency_ms": 120000, "hold_min_passes": 3,
                "requires_recovery": true, "promote_min_passes": 4,
                "forbids_repeat_of_failed": true })
    }

    /// minimax-m2.1, l'ultimo giro reale (evidence 18/07, suite 4): catena piena
    /// 5,5,5,5; recupero SEMPRE letto (recovered 4/4) ma 2 tentativi bocciati
    /// per ripetizione; real 3/3; latent 2/4 (empty_completion).
    fn giro_minimax() -> Vec<ProfileRun> {
        vec![
            run_dai_fatti(KIND_TOOL_CHAIN, &predicato_catena(), &[
                fatti(5, false, false, false), fatti(5, false, false, false),
                fatti(5, false, false, false), fatti(5, false, false, false),
            ]),
            run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
                fatti(2, true, true, true), fatti(1, true, false, false),
                fatti(0, true, true, true), fatti(0, true, false, true),
            ]),
            run_conteggi(KIND_TOOL_REALISTIC, 3, 0),
            run_conteggi(crate::probe_latent_state::KIND_LATENT_STATE, 2, 2),
        ]
    }

    /// LA SIMULAZIONE DEL PIANO, coi ProfileRun dell'ULTIMO GIRO REALE
    /// (ai_model_probe_evidence, 17-18/07, suite 4) fatti passare dai produttori
    /// veri. UNICO input non preso dall'evidence, dichiarato: la catena di
    /// ministral-8b (vintage pre-0610, senza misure) e' simulata a 2 anelli su 5
    /// come nel piano.
    ///
    /// I valori ASSOLUTI del piano (85 / 67.5 / 65 / 65) assumevano componenti
    /// idealizzate; la formula ESATTA sui fatti reali produce i numeri sotto —
    /// ministral coincide (32.5) — e TUTTE le conclusioni del piano reggono:
    /// leader frontier, kimi/qwen/grok heavy, ministral MEDIUM (non piu' high),
    /// banda high VUOTA.
    #[test]
    fn la_simulazione_del_piano_sul_giro_reale() {
        let w = pesi();
        let kinds = kinds_suite_4();
        // kimi-k2.5: catena 5,4,3,5 (un tentativo con sintassi rotta), UN
        // recupero letto su 4, real 3/3, latent 4/4.
        let kimi = vec![
            run_dai_fatti(KIND_TOOL_CHAIN, &predicato_catena(), &[
                fatti(5, false, false, false), fatti(4, false, false, true),
                fatti(3, false, false, false), fatti(5, false, false, false),
            ]),
            run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
                fatti(0, true, false, false), fatti(0, false, false, false),
                fatti(0, false, false, false), fatti(0, false, false, false),
            ]),
            run_conteggi(KIND_TOOL_REALISTIC, 3, 0),
            run_conteggi(crate::probe_latent_state::KIND_LATENT_STATE, 4, 0),
        ];
        // qwen3-235b: catena 5,5,4,5; recupero MAI letto (e una ripetizione).
        let qwen = vec![
            run_dai_fatti(KIND_TOOL_CHAIN, &predicato_catena(), &[
                fatti(5, false, false, false), fatti(5, false, false, false),
                fatti(4, false, false, false), fatti(5, false, false, false),
            ]),
            run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
                fatti(0, false, false, false), fatti(0, false, false, false),
                fatti(0, false, false, false), fatti(0, false, true, false),
            ]),
            run_conteggi(KIND_TOOL_REALISTIC, 3, 0),
            run_conteggi(crate::probe_latent_state::KIND_LATENT_STATE, 4, 0),
        ];
        // grok-4.5: catena piena, recupero MAI letto e ripetuto IDENTICO 4/4
        // (il pattern sospetto del piano): recovery 0 + malus repeated.
        let grok = vec![
            run_dai_fatti(KIND_TOOL_CHAIN, &predicato_catena(), &[
                fatti(5, false, false, false), fatti(5, false, false, false),
                fatti(5, false, false, false), fatti(5, false, false, false),
            ]),
            run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
                fatti(0, false, true, false), fatti(0, false, true, false),
                fatti(0, false, true, true), fatti(0, false, true, true),
            ]),
            run_conteggi(KIND_TOOL_REALISTIC, 3, 0),
            run_conteggi(crate::probe_latent_state::KIND_LATENT_STATE, 4, 0),
        ];
        // ministral-8b: catena a 2 anelli (dichiarato sopra), niente recupero,
        // real 3/3, latent 2/4. Il caso di separazione del piano.
        let ministral = vec![
            run_dai_fatti(KIND_TOOL_CHAIN, &predicato_catena(), &[
                fatti(2, false, false, false), fatti(2, false, false, false),
                fatti(2, false, false, false), fatti(2, false, false, false),
            ]),
            run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
                fatti(0, false, false, false), fatti(0, false, false, false),
                fatti(0, false, false, false), fatti(0, false, false, false),
            ]),
            run_conteggi(KIND_TOOL_REALISTIC, 3, 0),
            run_conteggi(crate::probe_latent_state::KIND_LATENT_STATE, 2, 2),
        ];

        let punteggio = |runs: &[ProfileRun]| {
            derive_measured_score(runs, &kinds, &w).expect("giro completo: lo score c'e'")
        };
        let vicino = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let s_minimax = punteggio(&giro_minimax());
        let s_kimi = punteggio(&kimi);
        let s_qwen = punteggio(&qwen);
        let s_grok = punteggio(&grok);
        let s_ministral = punteggio(&ministral);
        // La formula esatta sui fatti reali, coi pesi della mig 0620
        // (chain 12, recovery 45, real 18, latent 25, longctx 0) e
        // LINKS_TARGET = 6 (mig 0621). Ogni addendo e' scritto come
        // peso * frazione-osservata, cosi' il numero si legge invece di essere
        // copiato dall'output: catena (somma links)/(4*6), recupero e latent per
        // rate, malus -5 su repeated e bad_syntax sugli 8 tentativi multi-step.
        //
        // Il denominatore scende da 28 a 24 col bersaglio: questi tentativi sono
        // della suite 4 e i loro anelli valgono un po' di piu' rapportati a una
        // catena piu' corta. E' precisamente il motivo per cui il cambio di
        // bersaglio pretende un bump di suite — i punteggi non sono confrontabili
        // fra materiali diversi, e questa riga lo rende visibile.
        assert!(vicino(s_minimax, 12.0 * 20.0 / 24.0 + 45.0 + 18.0 + 25.0 * 0.5 - 3.125), "minimax: {s_minimax}");
        assert!(vicino(s_kimi, 12.0 * 17.0 / 24.0 + 45.0 * 0.25 + 18.0 + 25.0 - 0.625), "kimi: {s_kimi}");
        assert!(vicino(s_qwen, 12.0 * 19.0 / 24.0 + 18.0 + 25.0 - 0.625), "qwen: {s_qwen}");
        assert!(vicino(s_grok, 12.0 * 20.0 / 24.0 + 18.0 + 25.0 - 3.75), "grok: {s_grok}");
        assert!(vicino(s_ministral, 12.0 * 8.0 / 24.0 + 18.0 + 25.0 * 0.5), "ministral: {s_ministral}");
        // grok sotto kimi NONOSTANTE catena piena e latent pieno: il recovery
        // (peso 30) e il malus repeated lo pagano. E' il razionale di w_recovery.
        assert!(s_grok < s_kimi, "recovery 0 + malus deve costare: {s_grok} vs {s_kimi}");

        // LE BANDE RELATIVE al leader misurato (74.375): il criterio di
        // successo che la scala assoluta fallisce — ministral si separa.
        let ancora = s_minimax;
        let banda = |s: f64| banda_measured(s, None, ancora, &bande(), 3.0);
        assert_eq!(banda(s_minimax), "frontier", "il leader e' il 100% di se stesso");
        assert_eq!(banda(s_kimi), "heavy");
        // qwen e grok scendono a HIGH con i pesi della 0620: la catena non li
        // porta piu' in alto (12 punti invece di 25) e il recupero, che nessuno
        // dei due passa, ora ne pesa 45. Prima erano indistinguibili da kimi.
        assert_eq!(banda(s_qwen), "high");
        assert_eq!(banda(s_grok), "high");
        assert_eq!(banda(s_ministral), "medium", "ministral-8b NON e' piu' high");
        // QUI IL TEST E' STATO CAPOVOLTO, ed e' la misura del progresso: coi
        // pesi vecchi la banda high restava VUOTA (il ciclo asseriva che nessuno
        // la toccasse) perche' la catena da 25 punti spingeva qwen e grok su
        // heavy insieme a kimi. Con la 0620 i cinque modelli occupano QUATTRO
        // bande su cinque: una scala che usa i suoi gradini invece di
        // accatastare tutti in cima.
        let bande_usate: std::collections::BTreeSet<_> =
            [s_minimax, s_kimi, s_qwen, s_grok, s_ministral].iter().map(|s| banda(*s)).collect();
        assert!(
            bande_usate.contains("high"),
            "la banda high deve essere popolata: e' il gradino che la de-pesatura ha restituito"
        );
        assert!(bande_usate.len() >= 4, "bande distinte usate: {bande_usate:?}");
    }

    /// Il razionale di w_recovery=45 (mig 0620, prima 30): un GEMELLO del leader
    /// che non legge mai l'errore resta fuori da frontier SENZA dipendere dal
    /// malus. Il peso e' salito perche' il recupero e' rimasto il solo criterio
    /// che apre davvero il ventaglio, mentre la catena e' diventata commodity.
    #[test]
    fn un_gemello_del_leader_senza_recovery_non_e_frontier() {
        let w = pesi();
        let kinds = kinds_suite_4();
        let mut gemello = giro_minimax();
        // Stesso giro, ma il recupero non viene MAI letto (recovered=false):
        // stessi fatti di ripetizione/sintassi, quindi lo stesso malus.
        gemello[1] = run_dai_fatti(KIND_TOOL_RECOVERY, &predicato_recupero(), &[
            fatti(2, false, true, true), fatti(1, false, false, false),
            fatti(0, false, true, true), fatti(0, false, false, true),
        ]);
        let leader = derive_measured_score(&giro_minimax(), &kinds, &w).expect("leader");
        let s = derive_measured_score(&gemello, &kinds, &w).expect("gemello");
        assert!((s - (leader - 45.0)).abs() < 1e-9, "perde ESATTAMENTE il peso recovery: {s}");
        assert_ne!(
            banda_measured(s, None, leader, &bande(), 3.0),
            "frontier",
            "senza recovery non si e' il migliore, nemmeno col malus azzerato: \
             {s} / {leader} = {:.3}",
            s / leader
        );
    }

    /// IL SILENZIO NON PUNTEGGIA: una componente APPLICABILE senza tentativi
    /// conclusivi (provider giu', profilo non costruibile) azzera lo SCORE del
    /// giro, non il punteggio della componente — score e bande restano invariati.
    #[test]
    fn una_componente_applicabile_ma_muta_annulla_lo_score() {
        let w = pesi();
        let kinds = kinds_suite_4();
        let mut giro = giro_minimax();
        // Il recupero e' girato ma SOLO inconclusivo: 4 tentativi, zero conclusivi.
        giro[1] = ProfileRun {
            profile_key: KIND_TOOL_RECOVERY.into(),
            kind: KIND_TOOL_RECOVERY.into(),
            grants: vec![],
            is_blocking: false,
            passes: 0,
            conclusive_fails: 0,
            inconclusive: 4,
            promote_min: 4,
            first_fail_reason: None,
            somme: SommeConclusive::default(),
        };
        assert_eq!(derive_measured_score(&giro, &kinds, &w), None);
        // Ma una componente NON applicabile (long_context fuori suite) NON
        // annulla: vale 0 punti e il resto punteggia (gia' provato sopra).
    }

    /// Il MARGINE DI DEMOZIONE (catalog.measured_band.demote_margin): si sale
    /// superando la soglia, si scende solo sotto soglia - margine. L'isteresi
    /// spetta SOLO alla banda guadagnata dalla batteria, mai a un prior.
    #[test]
    fn il_margine_di_demozione_protegge_la_banda_guadagnata() {
        let b = bande();
        // Ancora 100: soglia heavy = 65. A 63 (dentro il margine di 3) chi E'
        // heavy resta heavy; a 61.9 scende — e di UNA banda, a high.
        assert_eq!(banda_measured(63.0, Some("heavy"), 100.0, &b, 3.0), "heavy");
        assert_eq!(banda_measured(61.9, Some("heavy"), 100.0, &b, 3.0), "high");
        // Salire non ha margine: superata la soglia, si sale.
        assert_eq!(banda_measured(86.0, Some("heavy"), 100.0, &b, 3.0), "frontier");
        // Un prior (nessuna banda measured) non gode dell'isteresi.
        assert_eq!(banda_measured(63.0, None, 100.0, &b, 3.0), "high");
        // Una banda fuori scala (refuso) non e' un gradino da difendere.
        assert_eq!(banda_measured(63.0, Some("boh"), 100.0, &b, 3.0), "high");
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
            measured_score: None,
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
                measured_score: None,
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
    /// Colonne e settings dello score (mig 0616) sopra lo schema di test.
    async fn colonne_e_settings_score(pool: &PgPool) {
        sqlx::query(
            "ALTER TABLE ai_price_catalog \
               ADD COLUMN tier_source TEXT, \
               ADD COLUMN measured_score DOUBLE PRECISION, \
               ADD COLUMN measured_score_at TIMESTAMPTZ, \
               ADD COLUMN measured_score_suite INT",
        )
        .execute(pool)
        .await
        .expect("colonne score");
        // Lo schema REALE dei settings (updated_at compresa): il pass PERSISTE
        // l'ancora via update_setting_value (regola O: la strada vera).
        sqlx::query(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, \
             updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        )
        .execute(pool)
        .await
        .expect("settings");
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES \
             ('catalog.measured_band.frontier_pct', '0.92'), \
             ('catalog.measured_band.heavy_pct', '0.65'), \
             ('catalog.measured_band.high_pct', '0.45'), \
             ('catalog.measured_band.medium_pct', '0.20'), \
             ('catalog.measured_band.anchor', ''), \
             ('catalog.measured_band.anchor_model', ''), \
             ('catalog.measured_band.anchor_at', ''), \
             ('catalog.measured_band.anchor_deadband_pct', '0.03'), \
             ('catalog.measured_band.demote_margin', '3'), \
             ('catalog.measured_band.min_population', '3')",
        )
        .execute(pool)
        .await
        .expect("seed settings");
    }

    /// MIN_POPULATION: sotto 3 modelli misurati a suite corrente le bande
    /// measured NON si applicano (il tier resta synced). Obbligatoria: senza,
    /// il primo misurato di OGNI suite sarebbe frontier per definizione (e' il
    /// 100% di se stesso). Il test attraversa il pass VERO
    /// (`riancora_bande_measured`), non una sua imitazione.
    #[sqlx::test]
    async fn sotto_min_population_le_bande_measured_non_si_applicano(pool: PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        colonne_e_settings_score(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, performance_tier, tier_source, measured_score, measured_score_suite) \
             VALUES ('p', 'primo', 'heavy', 'synced', 80.0, 5)",
        )
        .execute(&pool)
        .await
        .expect("seed");

        riancora_bande_measured(&pool, 5).await;

        let (tier, src): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='primo'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(
            (tier.as_deref(), src.as_deref()),
            (Some("heavy"), Some("synced")),
            "UN solo misurato: se qui compare frontier/measured, min_population \
             e' saltata e il primo misurato di ogni suite si autoproclama leader"
        );
        let ancora: (String,) = sqlx::query_as(
            "SELECT value FROM settings WHERE key = 'catalog.measured_band.anchor'",
        )
        .fetch_one(&pool)
        .await
        .expect("ancora");
        assert_eq!(ancora.0, "", "sotto soglia nemmeno l'ancora si fissa");

        // Al terzo misurato la popolazione basta: il pass si applica, l'ancora
        // si fissa sul leader e le bande sono RELATIVE a lui.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, performance_tier, tier_source, measured_score, measured_score_suite) \
             VALUES ('p', 'secondo', 'medium', 'synced', 60.0, 5), \
                    ('p', 'terzo',   'medium', 'synced', 40.0, 5), \
                    ('p', 'fuori-suite', 'medium', 'synced', 99.0, 4)",
        )
        .execute(&pool)
        .await
        .expect("altri");
        riancora_bande_measured(&pool, 5).await;
        let righe: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT model, performance_tier, tier_source FROM ai_price_catalog ORDER BY model",
        )
        .fetch_all(&pool)
        .await
        .expect("righe");
        assert_eq!(
            righe,
            vec![
                // 99.0 ma di SUITE VECCHIA: fuori dal leader e fuori dal pass.
                ("fuori-suite".into(), Some("medium".into()), Some("synced".into())),
                ("primo".into(), Some("frontier".into()), Some("measured".into())),
                ("secondo".into(), Some("heavy".into()), Some("measured".into())),
                ("terzo".into(), Some("high".into()), Some("measured".into())),
            ],
            "a popolazione raggiunta il leader (80) e' frontier, 60/80=75% e' \
             heavy, 40/80=50% e' high; la suite vecchia resta fuori"
        );
        let ancora: (String,) = sqlx::query_as(
            "SELECT value FROM settings WHERE key = 'catalog.measured_band.anchor'",
        )
        .fetch_one(&pool)
        .await
        .expect("ancora");
        assert_eq!(ancora.0, "80", "l'ancora e' il leader misurato a suite corrente");
    }

    /// Lo SCORE atterra nella stessa transazione del verdetto (DerivedWrite::
    /// qualified), e un giro senza score NON tocca le colonne di ieri.
    #[sqlx::test]
    async fn il_verdetto_scrive_lo_score_e_il_silenzio_non_lo_azzera(pool: PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "ALTER TABLE ai_price_catalog \
               ADD COLUMN tier_source TEXT, \
               ADD COLUMN capability_locked BOOLEAN NOT NULL DEFAULT false, \
               ADD COLUMN capability_source TEXT NOT NULL DEFAULT 'auto', \
               ADD COLUMN qualified_at TIMESTAMPTZ, \
               ADD COLUMN qualification_suite_version INT, \
               ADD COLUMN qualification_reason TEXT, \
               ADD COLUMN qualification_evidence_id BIGINT, \
               ADD COLUMN qualification_started_at TIMESTAMPTZ, \
               ADD COLUMN qualification_attempts INT NOT NULL DEFAULT 0, \
               ADD COLUMN qualification_backoff_until TIMESTAMPTZ, \
               ADD COLUMN measured_score DOUBLE PRECISION, \
               ADD COLUMN measured_score_at TIMESTAMPTZ, \
               ADD COLUMN measured_score_suite INT",
        )
        .execute(&pool)
        .await
        .expect("colonne");
        sqlx::query("INSERT INTO ai_price_catalog (provider, model) VALUES ('p', 'm')")
            .execute(&pool)
            .await
            .expect("seed");

        let con_score = Derived {
            state: DerivedState::Qualified,
            qualified_capabilities: vec!["chat".into()],
            reason: "suite_passed".into(),
            thinking: None,
            measured_tier: None,
            measured_score: Some(74.375),
        };
        apply_derived(&pool, "p", "m", 5, &con_score, None, 30, 24).await;
        let riga: (Option<f64>, Option<i32>) = sqlx::query_as(
            "SELECT measured_score, measured_score_suite FROM ai_price_catalog WHERE model='m'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(riga, (Some(74.375), Some(5)), "score e suite atterrano col verdetto");

        // Il giro successivo NON produce score (pesi assenti, componente muta):
        // le colonne restano quelle di ieri, mai azzerate.
        let senza_score = Derived { measured_score: None, ..con_score };
        apply_derived(&pool, "p", "m", 5, &senza_score, None, 30, 24).await;
        let riga: (Option<f64>, Option<i32>) = sqlx::query_as(
            "SELECT measured_score, measured_score_suite FROM ai_price_catalog WHERE model='m'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(riga, (Some(74.375), Some(5)), "il silenzio non azzera lo score");
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

    /// REGRESSIONE del giro muto del 2026-07-17: 32 tentativi su 32 inconclusive con
    /// "mondo_non_costruibile", perche' il guard vietava l'handle di partenza che
    /// l'istruzione DEVE nominare.
    ///
    /// Questo test raggiunge il mondo per la STESSA strada della produzione (regola O):
    /// l'istruzione la costruisce `istruzione_catena`/`istruzione_recupero`, gli stessi
    /// produttori che chiama `multi_step_attempt`. I 17 test che c'erano prima
    /// passavano tutti una richiesta VUOTA (`&[]`) o un token a valle: nessuno costruiva
    /// il mondo come lo costruisce chi lo usa davvero, e per questo 39 test verdi
    /// certificavano un motore che non poteva partire.
    #[test]
    fn il_mondo_si_costruisce_con_l_istruzione_vera() {
        use crate::probe_world::{ScriptedWorld, TokenSeed, WorldKind};

        for (kind, profilo) in [
            (WorldKind::Catena, "agentic_chain"),
            (WorldKind::Recupero, "agentic_recovery"),
        ] {
            let seed = TokenSeed {
                provider: "mistral".into(),
                model: "mistral-medium-2604".into(),
                profile_key: profilo.into(),
                attempt: 1,
                seed: 42,
            };
            // Esattamente cio' che fa `multi_step_attempt`: handle0 -> istruzione.
            let handle0 = seed.handle(0);
            let istruzione = match kind {
                WorldKind::Catena => istruzione_catena(&handle0, &seed.custode()),
                WorldKind::Recupero => istruzione_recupero(&handle0),
            };
            assert!(
                istruzione.contains(&handle0),
                "{profilo}: l'istruzione deve nominare l'anello di partenza, o il \
                 modello non ha da dove cominciare"
            );

            let mondo = ScriptedWorld::new(kind, seed.clone(), &[&istruzione, "system"]);
            assert!(
                mondo.is_ok(),
                "{profilo}: il mondo deve costruirsi con l'istruzione VERA, non solo \
                 con una richiesta vuota. Motivo del rifiuto: {:?}",
                mondo.err()
            );
        }
    }

    /// L'altra meta' dell'invariante, che il fix NON deve aver spento: un token a
    /// valle nella richiesta rende la catena scorciatoiabile, e il mondo si rifiuta.
    #[test]
    fn un_handle_a_valle_nell_istruzione_resta_vietato() {
        use crate::probe_world::{ScriptedWorld, TokenSeed, WorldKind};

        let seed = TokenSeed {
            provider: "mistral".into(),
            model: "mistral-medium-2604".into(),
            profile_key: "agentic_chain".into(),
            attempt: 1,
            seed: 42,
        };
        let trapelato = seed.handle(3);
        let istruzione = format!(
            "{} (e la risposta e' {trapelato})",
            istruzione_catena(&seed.handle(0), &seed.custode())
        );
        assert!(
            ScriptedWorld::new(WorldKind::Catena, seed.clone(), &[&istruzione]).is_err(),
            "un anello a valle visibile nella richiesta deve impedire il giro"
        );
        assert!(
            ScriptedWorld::new(WorldKind::Catena, seed.clone(), &[&format!("esca {}", seed.esca(0))])
                .is_err(),
            "un'esca viaggia solo nelle risposte: nella richiesta deve impedire il giro"
        );
    }

    // ── Il GIRO COMPLETO del recupero (regola O) ─────────────────────
    //
    // Il test isolato `il_modello_che_legge_l_errore_recupera` (probe_agentic_loop)
    // si ferma a `out.measures.recovered`: prova la MECCANICA del loop, non che la
    // misura arrivi al VERDETTO. Questi due test attraversano la stessa catena della
    // produzione — `multi_step_attempt` fa esattamente `run_loop` -> `turno_dai_fatti`
    // -> `evaluate_attempt` col predicato REALE del profilo — e asseriscono la
    // conseguenza (verdict='pass'/'no_recovery'), non un fatto intermedio. Partono
    // dai produttori veri (il turno nasce da `agent_turn_value_from_gw`, i fatti da
    // `run_loop`): nessun `measures`/`derived` fabbricato a mano.

    /// Un modello finto per il mondo Recupero. `riporta_il_token` = legge l'errore e
    /// porta il `current_epoch` (recupera); altrimenti al secondo turno risponde con
    /// SOLO testo e nessuna tool-call — cio' che fa un modello che si ARRENDE, e che
    /// fa chiudere il loop (probe_agentic_loop: `blocchi.is_empty() -> break`). E' la
    /// forma reale del `no_recovery` osservato in produzione, non un ramo inventato.
    struct RecuperoScritto {
        turni_emessi: std::cell::RefCell<usize>,
        riporta_il_token: bool,
    }

    /// Il turno come lo produce la PRODUZIONE: una [`GwResponse`] del gateway fatta
    /// passare per `agent_turn_value_from_gw`, l'UNICO produttore di questo Value.
    /// Fabbricarlo a mano fisserebbe l'assunto da verificare (regola O).
    ///
    /// Punto unico dei modelli finti di questo modulo: prima ogni agente scriptato si
    /// portava dietro la propria copia della costruzione, e una copia diverge.
    fn turno_prodotto(chiamate: &[(String, String)]) -> Value {
        use crate::nexus_gateway::{GwResponse, GwToolCall, GwToolFunctionCall, GwUsage};
        let tc: Vec<GwToolCall> = chiamate
            .iter()
            .enumerate()
            .map(|(i, (n, a))| GwToolCall {
                id: format!("c{i}"),
                kind: "function".into(),
                function: GwToolFunctionCall { name: n.clone(), arguments: a.clone() },
                thought_signature: None,
            })
            .collect();
        let resp = GwResponse {
            content: if chiamate.is_empty() { "non riesco a proseguire".into() } else { String::new() },
            tool_calls: (!tc.is_empty()).then_some(tc),
            usage: GwUsage::default(),
            model_used: "m".into(),
            provider_used: "p".into(),
            latency_ms: 1,
            finish_reason: if chiamate.is_empty() { "stop".into() } else { "tool_calls".into() },
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
        };
        crate::orchestrator::neural_client::agent_turn_value_from_gw("p", "m", &resp)
    }

    impl crate::probe_agentic_loop::TurnSource for RecuperoScritto {
        async fn turn(&self, messages_json: &str) -> Value {
            let produci = turno_prodotto;
            let mut n = self.turni_emessi.borrow_mut();
            let turno = *n;
            *n += 1;
            // Il token dell'errore il modello non lo conosce in anticipo: lo LEGGE
            // dalla conversazione, dal campo `current_epoch`, come farebbe uno vero.
            // TUTTE le occorrenze del nome campo, non solo la prima: il `message`
            // dell'errore lo nomina in prosa prima del campo (stesso fix del
            // gemello in probe_agentic_loop).
            let token = messages_json.split("current_epoch").skip(1).find_map(|coda| {
                let i = coda.find("E-")?;
                let tok: String = coda[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                (tok.len() > 3).then_some(tok)
            });
            match (turno, self.riporta_il_token, &token) {
                (0, _, _) => produci(&[("read_file".into(), r#"{"path":"start"}"#.into())]),
                (_, true, Some(t)) => produci(&[("read_file".into(), format!(r#"{{"path":"{t}"}}"#))]),
                // Si arrende: nessuna tool-call, il loop si chiude.
                _ => produci(&[]),
            }
        }
    }

    async fn giro_completo_recupero(riporta_il_token: bool) -> AttemptOutcome {
        use crate::probe_world::{ScriptedWorld, TokenSeed, WorldKind};
        // Il predicato REALE del profilo `agentic_recovery` (mig 0599/0610), non un
        // predicato di comodo: e' cio' che gira in produzione.
        let predicate = json!({
            "max_latency_ms": 120000, "hold_min_passes": 3, "requires_recovery": true,
            "promote_min_passes": 4, "forbids_repeat_of_failed": true
        });
        let seed = TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: "agentic_recovery".into(),
            attempt: 1,
            seed: 7,
        };
        let mut mondo = ScriptedWorld::new(WorldKind::Recupero, seed, &[]).unwrap();
        let modello = RecuperoScritto { turni_emessi: std::cell::RefCell::new(0), riporta_il_token };
        // La STESSA catena di `multi_step_attempt`: loop -> `verdetto_dai_fatti`
        // (che fa `turno_dai_fatti` -> `evaluate_attempt` -> `derived`). Il test
        // delega al punto unico, non ricostruisce la sequenza a mano (regola O).
        let out = crate::probe_agentic_loop::run_loop(&modello, WorldKind::Recupero, &mut mondo, "istr", 6).await;
        assert!(out.inconclusive.is_none(), "il modello ha risposto: e' un verdetto vero, non un inconclusivo");
        verdetto_dai_fatti(&out.measures, &predicate, 42)
    }

    /// Chi RIPORTA il token dell'errore ottiene verdict='pass'. Il test isolato si
    /// fermava a `measures.recovered=true`; qui la misura arriva al verdetto.
    #[tokio::test]
    async fn il_giro_completo_promuove_chi_riporta_il_token_dell_errore() {
        let out = giro_completo_recupero(true).await;
        assert!(out.pass, "recupero riuscito deve dare pass, non '{}'", out.reason);
        assert_eq!(out.reason, "ok");
        // `derived` porta i FATTI dietro il verdetto (regola O: un verdetto senza i
        // suoi fatti non e' contestabile). Prima era una colonna morta (mig 0591).
        assert_eq!(
            out.derived.as_ref().and_then(|d| d.pointer("/recovered")).and_then(Value::as_bool),
            Some(true),
            "il verdetto deve portare con se' il fatto che l'ha deciso"
        );
    }

    /// Chi si ARRENDE dopo l'errore (nessuna tool-call al secondo turno, il loop si
    /// chiude) ottiene verdict='no_recovery'. E' la forma ESATTA dei 93 fail
    /// osservati in produzione al 2026-07-17: non un bug di misura, un modello che
    /// non riporta il token. Se `turno_dai_fatti` smettesse di portare `recovered`,
    /// entrambi i giri darebbero no_recovery e questo test lo vedrebbe.
    #[tokio::test]
    async fn il_giro_completo_boccia_chi_non_riporta_il_token() {
        let out = giro_completo_recupero(false).await;
        assert!(!out.pass);
        assert_eq!(out.reason, "no_recovery");
        assert_eq!(
            out.derived.as_ref().and_then(|d| d.pointer("/recovered")).and_then(Value::as_bool),
            Some(false),
            "il fail 'no_recovery' deve mostrare recovered=false, cosi' e' diagnosticabile"
        );
    }

    // ── LA CATENA: raggiungibilita' e difficolta', PROVATE (regola O) ─────
    //
    // Un mondo piu' difficile e' un'ipotesi finche' non si misura da due lati
    // opposti, e sono i due modi in cui questa batteria ha gia' sbagliato:
    //
    //   RAGGIUNGIBILITA' - esiste una traiettoria che passa? Il profilo di recupero
    //     e' stato a 0 pass su 30 modelli DUE volte perche' nessuno l'aveva mai
    //     percorso in codice. Un test che nessuno passa non e' severo, e' rotto.
    //   DIFFICOLTA' - le strategie da una riga falliscono? Senza questo lato, la
    //     catena e' saturata due volte (100% di pass) e ce ne siamo accorti solo
    //     leggendo l'istogramma degli anelli in produzione.
    //
    // I quattro agenti qui sotto sono scriptati e NON-LLM: differiscono solo per la
    // regola di scelta della prossima voce, quindi il delta fra i loro esiti misura
    // il MONDO e nient'altro. Attraversano la strada della produzione — istruzione da
    // `istruzione_catena`, mondo da `ScriptedWorld::new` col guard del needle, giro da
    // `run_loop`, verdetto da `verdetto_dai_fatti` col predicato reale — perche' un
    // agente d'oro che chiamasse il mondo a mano proverebbe solo che sappiamo
    // scrivere un copione.

    /// La regola con cui un agente sceglie la voce da seguire. E' l'UNICA cosa che
    /// cambia fra i quattro: tutto il resto e' identico per costruzione.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Strategia {
        /// LA TRAIETTORIA INTESA: segui la voce del custode nominato nel compito e,
        /// quando la pista si chiude, torna all'elenco precedente e prendi la voce
        /// che avevi scartato. Nient'altro: nessuna conoscenza privilegiata del
        /// seme, nessun token passato di nascosto.
        Oro,
        /// La scorciatoia che il ridisegno doveva uccidere: cerca l'etichetta
        /// 'current' e prendi quel ref.
        CercaCurrent,
        /// L'altra scorciatoia da una riga: prendi sempre il primo ref dell'elenco.
        PrimoRef,
        /// Legge davvero il custode, ma non si adatta: sul ramo chiuso insiste.
        /// E' il caso piu' severo per noi — isola l'ADATTAMENTO da tutto il resto.
        SenzaRitorno,
    }

    struct AgenteScritto {
        strategia: Strategia,
    }

    /// Il primo token con un dato prefisso dentro un testo.
    fn token_con_prefisso(testo: &str, prefisso: &str) -> Option<String> {
        let i = testo.find(prefisso)?;
        Some(
            testo[i..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect(),
        )
    }

    /// Il contenuto del primo messaggio utente: l'istruzione del compito, dove vive
    /// il custode. Un agente vero ce l'ha sotto gli occhi allo stesso modo.
    fn istruzione_dalla_conversazione(msgs: &[Value]) -> String {
        msgs.iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|m| m.get("content").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    }

    /// I tool_result visti finora, in ordine.
    fn risultati_tool(msgs: &[Value]) -> Vec<String> {
        msgs.iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .filter_map(|m| m.get("content").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    /// Il `ref` della voce affidata al custode (`affidata=true`) o dell'altra.
    fn ref_per_custode(elenco: &Value, custode: &str, affidata: bool) -> Option<String> {
        elenco["entries"]
            .as_array()?
            .iter()
            .find(|v| (v["owner"].as_str() == Some(custode)) == affidata)
            .and_then(|v| v["ref"].as_str().map(str::to_string))
    }

    /// L'ultimo tool_result che sia un ELENCO di voci: e' l'elenco a cui l'errore
    /// del ramo chiuso dice di tornare.
    fn ultimo_elenco(risultati: &[String]) -> Option<Value> {
        risultati
            .iter()
            .rev()
            .filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .find(|v| v["entries"].is_array())
    }

    /// La vecchia strategia vincente, conservata come misura: la voce marcata
    /// 'current'. Se il mondo la espone ancora, questo agente passa — ed e'
    /// esattamente cio' che il test di simmetria non deve permettere.
    fn ref_marcato_current(elenco: &Value) -> Option<String> {
        elenco["entries"]
            .as_array()?
            .iter()
            .find(|v| v.as_object().is_some_and(|o| o.values().any(|x| x == "current")))
            .and_then(|v| v["ref"].as_str().map(str::to_string))
    }

    impl AgenteScritto {
        /// Il bersaglio del prossimo turno, deciso SOLO da cio' che e' nella
        /// conversazione. `None` = non so proseguire, e il giro si chiude.
        fn prossimo_bersaglio(&self, messages_json: &str) -> Option<String> {
            let msgs: Vec<Value> = serde_json::from_str(messages_json).ok()?;
            let istruzione = istruzione_dalla_conversazione(&msgs);
            let custode = token_con_prefisso(&istruzione, "C-")?;
            let risultati = risultati_tool(&msgs);
            let Some(ultimo) = risultati.last() else {
                // Primo turno: l'unico bersaglio noto e' quello del compito.
                return token_con_prefisso(&istruzione, "H-");
            };
            // Il CODICE dell'errore, non la sua prosa: `E_BRANCH_CLOSED` e' un
            // identificatore macchina stabile, il `message` e' per gli umani
            // (regola M, lo stesso criterio che il gateway applica ai provider).
            if ultimo.contains("E_BRANCH_CLOSED") {
                return self.dopo_il_ramo_chiuso(&risultati, &custode);
            }
            self.dall_elenco(&serde_json::from_str::<Value>(ultimo).ok()?, &custode)
        }

        fn dall_elenco(&self, elenco: &Value, custode: &str) -> Option<String> {
            match self.strategia {
                Strategia::Oro | Strategia::SenzaRitorno => {
                    ref_per_custode(elenco, custode, true)
                }
                Strategia::PrimoRef => elenco["entries"][0]["ref"].as_str().map(str::to_string),
                Strategia::CercaCurrent => ref_marcato_current(elenco),
            }
        }

        /// Cosa si fa quando la pista si chiude. E' l'unico bivio che separa
        /// l'adattamento dalla sua assenza.
        fn dopo_il_ramo_chiuso(&self, risultati: &[String], custode: &str) -> Option<String> {
            let elenco = ultimo_elenco(risultati)?;
            match self.strategia {
                // L'errore lo dichiara: torna all'elenco, prendi la voce scartata.
                Strategia::Oro => ref_per_custode(&elenco, custode, false),
                // Non si adatta: ripresenta la stessa voce, che resta chiusa.
                _ => ref_per_custode(&elenco, custode, true),
            }
        }
    }

    impl crate::probe_agentic_loop::TurnSource for AgenteScritto {
        async fn turn(&self, messages_json: &str) -> Value {
            match self.prossimo_bersaglio(messages_json) {
                Some(b) => turno_prodotto(&[(
                    "read_file".into(),
                    json!({ "path": b }).to_string(),
                )]),
                None => turno_prodotto(&[]),
            }
        }
    }

    fn seme_catena(seme: u64) -> crate::probe_world::TokenSeed {
        crate::probe_world::TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: "agentic_chain".into(),
            attempt: 1,
            seed: seme,
        }
    }

    /// Un giro completo di `agentic_chain`, per la STESSA strada di
    /// `multi_step_attempt`: istruzione dal produttore vero, mondo col guard del
    /// needle, `run_loop`, `verdetto_dai_fatti` col predicato REALE del profilo.
    async fn giro_completo_catena(strategia: Strategia, seme: u64) -> (AttemptOutcome, usize) {
        use crate::probe_world::{ScriptedWorld, WorldKind};
        // Il predicato del profilo `agentic_chain` dopo la mig 0621: 4 anelli.
        // 4 > 3 = l'anello cieco piu' lontano, quindi passare IMPLICA essere
        // rientrati sulla pista — non c'e' altra strada per superare quel numero.
        let predicate = json!({
            "max_latency_ms": 120000, "hold_min_passes": 2,
            "min_chained_calls": 4, "promote_min_passes": 3
        });
        let seed = seme_catena(seme);
        let istruzione = istruzione_catena(&seed.handle(0), &seed.custode());
        let mondo = ScriptedWorld::new(WorldKind::Catena, seed.clone(), &[&istruzione, "system"]);
        let mut mondo = mondo.expect("il mondo si costruisce con l'istruzione vera");
        let agente = AgenteScritto { strategia };
        // `max_turns: 8`, cio' che il profilo dichiara nel payload (mig 0618).
        let out = crate::probe_agentic_loop::run_loop(
            &agente,
            WorldKind::Catena,
            &mut mondo,
            &istruzione,
            8,
        )
        .await;
        assert!(
            out.inconclusive.is_none(),
            "il giro dev'essere attribuibile all'agente, non ai nostri cap: {:?}",
            out.inconclusive
        );
        (verdetto_dai_fatti(&out.measures, &predicate, 42), out.measures.chained_links)
    }

    /// I semi su cui si prova il mondo. Ne servono piu' d'uno perche' il seme decide
    /// DOVE la pista si interrompe e in che ORDINE stanno le voci: un solo seme
    /// misurerebbe un'istanza, non il design.
    const SEMI: [u64; 6] = [7, 42, 99, 1234, 5, 20260720];

    /// PROVA DI RAGGIUNGIBILITA'. La traiettoria intesa, scritta come codice, deve
    /// PASSARE — e arrivare al tetto di 6 anelli che `LINKS_TARGET` dichiara.
    ///
    /// Se questo test rosseggia, il mondo e' irrisolvibile e va ritarato: non e' il
    /// modello a essere debole, e' il test a essere rotto. E' la lezione dei due
    /// 0/30 sul profilo di recupero, resa eseguibile.
    #[tokio::test]
    async fn la_traiettoria_intesa_arriva_in_fondo() {
        for seme in SEMI {
            let (out, anelli) = giro_completo_catena(Strategia::Oro, seme).await;
            assert!(
                out.pass,
                "seme {seme}: la traiettoria intesa DEVE passare, invece '{}' ({anelli} anelli)",
                out.reason
            );
            assert_eq!(
                anelli, 6,
                "seme {seme}: la traiettoria perfetta vale esattamente 6 anelli con 8 \
                 turni (il rientro dalla pista chiusa ne costa uno). Se questo numero \
                 scende, LINKS_TARGET sta misurando un pieno irraggiungibile; se sale, \
                 l'interruzione non sta piu' costando nulla"
            );
        }
    }

    /// PROVA DI DIFFICOLTA' (simmetria). Le tre strategie da una riga NON devono
    /// passare. Senza questo lato non avremmo provato di aver alzato la difficolta':
    /// avremmo solo cambiato le stringhe.
    ///
    /// I tre casi isolano tre cose diverse, ed e' il punto:
    ///   - 'cerca current'  -> l'etichetta non c'e' piu': la vecchia strategia e' morta
    ///   - 'primo ref'      -> l'ordine e' del seme: la posizione non e' un criterio
    ///   - 'senza ritorno'  -> legge il custode DAVVERO, e non basta: manca solo
    ///                         l'adattamento, e si ferma esattamente all'anello cieco
    #[tokio::test]
    async fn le_strategie_da_una_riga_non_passano_piu() {
        for seme in SEMI {
            for strategia in [Strategia::CercaCurrent, Strategia::PrimoRef, Strategia::SenzaRitorno] {
                let (out, anelli) = giro_completo_catena(strategia, seme).await;
                assert!(
                    !out.pass,
                    "seme {seme}: la strategia {strategia:?} NON deve passare, invece \
                     ha chiuso {anelli} anelli con verdetto '{}'",
                    out.reason
                );
                // La CONSEGUENZA col suo numero, non un booleano: il motivo dice
                // quanti anelli ha chiuso e quanti ne servivano, cosi' un rosso e'
                // gia' una diagnosi (regola O).
                assert_eq!(
                    out.reason,
                    format!("no_chain:{anelli}<4"),
                    "seme {seme}, {strategia:?}"
                );
            }
        }
    }

    /// Il DELTA che il mondo produce, letto anello per anello: e' la misura del
    /// design, non del singolo agente.
    ///
    /// 'cerca current' si ferma a ZERO (l'etichetta e' sparita, non sa dove andare
    /// dopo il primo elenco); 'senza ritorno' si ferma ESATTAMENTE sull'anello
    /// cieco, che e' il costo puro del non-adattamento; l'oro arriva a 6. Tre
    /// numeri diversi dallo stesso mondo, e la distanza fra il secondo e il terzo
    /// e' cio' che il profilo misura davvero.
    ///
    /// PERCHE' NON BASTA IL TEST SOPRA, misurato e non supposto: rimettendo
    /// l'etichetta `state: "current"` negli elenchi (la mutazione che reintroduce
    /// il difetto), `le_strategie_da_una_riga_non_passano_piu` resta VERDE — chi
    /// segue l'etichetta arriva comunque solo all'anello cieco, e 3 < 4 e' ancora
    /// un fail. Il pass/fail da solo non vede la regressione: la vede questo, che
    /// guarda gli anelli. Un test che non rosseggia quando reintroduci il bug non
    /// copre il bug, copre se stesso.
    #[tokio::test]
    async fn i_tre_agenti_si_fermano_dove_il_design_dice() {
        for seme in SEMI {
            let cieco = seme_catena(seme).anello_cieco();
            let (_, anelli_current) = giro_completo_catena(Strategia::CercaCurrent, seme).await;
            let (_, anelli_fermo) = giro_completo_catena(Strategia::SenzaRitorno, seme).await;
            let (_, anelli_oro) = giro_completo_catena(Strategia::Oro, seme).await;
            assert_eq!(
                anelli_current, 0,
                "seme {seme}: senza l'etichetta 'current' la vecchia strategia non \
                 supera nemmeno il primo elenco"
            );
            assert_eq!(
                anelli_fermo, cieco,
                "seme {seme}: chi non si adatta si ferma sull'anello cieco, ne' prima \
                 ne' dopo. Se arrivasse oltre, la pista non si sarebbe interrotta"
            );
            assert!(
                anelli_oro > anelli_fermo,
                "seme {seme}: l'adattamento deve VALERE anelli ({anelli_oro} vs {anelli_fermo})"
            );
        }
    }
}
