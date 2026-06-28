//! `progress_controller`: punto unico (regola L) del controllo di avanzamento
//! del ciclo agentico. Porting 1:1 di `brain/agents/progress_controller.py`.
//!
//! Centralizza UNA sola domanda: "di fronte a uno stallo, qual e' la prossima
//! mossa?". La risposta segue SEMPRE la stessa gerarchia, identica per ogni asse
//! di stallo:
//!   1. GUIDE          -> forza-azione guidata (rimuovi read-only + tool_choice
//!                        required) + nudge assertivo.
//!   2. FORCE_DIAGNOSE -> stadio intermedio (solo asse repeated_action) prima di
//!                        escalation/abort.
//!   3. ESCALATE       -> promuovi il turno a un modello piu' capace.
//!   4. ABORT          -> guida+escalation esaurite -> chiusura con verifica E2E.
//!
//! La funzione [`decide`] e' PURA (nessun IO, nessuna lettura DB): i segnali
//! arrivano gia' risolti dal chiamante, identica a `should_force_tool_choice`.
//! Cosi' resta deterministica e testabile in isolamento.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Assi di stallo riconosciuti. Stringhe stabili (vedi serde rename): usate come
/// chiavi negli insiemi di stato ("assi gia' guidati") e nei log/meta_step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    #[serde(rename = "exploration")]
    Exploration,
    #[serde(rename = "signature")]
    Signature,
    #[serde(rename = "g1_descriptive")]
    G1Descriptive,
    #[serde(rename = "repeated_action")]
    RepeatedAction,
    #[serde(rename = "resource_reallocation")]
    ResourceReallocation,
}

impl Axis {
    /// Stringa stabile dell'asse (chiave per gli insiemi `already_*`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Axis::Exploration => "exploration",
            Axis::Signature => "signature",
            Axis::G1Descriptive => "g1_descriptive",
            Axis::RepeatedAction => "repeated_action",
            Axis::ResourceReallocation => "resource_reallocation",
        }
    }
}

/// Azioni possibili, in ordine di severita' crescente.
///
/// `force_diagnose`: stadio intermedio per l'asse repeated_action (dopo GUIDE,
/// prima di ESCALATE/ABORT). Obbliga l'agente a leggere l'errore, dichiarare la
/// causa radice e cambiare azione (vedi mig 0386).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "proceed")]
    Proceed,
    #[serde(rename = "guide")]
    Guide,
    #[serde(rename = "force_diagnose")]
    ForceDiagnose,
    #[serde(rename = "escalate")]
    Escalate,
    #[serde(rename = "abort")]
    Abort,
}

/// stop_reason UNICO emesso da un abort coordinato. `route_after_executor` lo
/// instrada alla verifica E2E (final_gate), non al learner morto. Tenuto distinto
/// dai legacy "loop_detected"/"g1_cap_reached".
pub const ABORT_STOP_REASON: &str = "loop_abort";

/// Segnali grezzi del turno corrente, raccolti dall'executor.
///
/// Tutti con default neutri: il chiamante popola solo quelli che conosce nel
/// punto in cui chiama (pre-LLM vs post-LLM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressSignals {
    /// Esplorazione: chiamate consecutive di sola lettura.
    pub exploration_count: i64,
    /// Soglia esplorazione.
    pub exploration_threshold: i64,
    /// Signature-loop: nome del tool ripetuto identico (None = nessun loop).
    pub signature_loop_tool: Option<String>,
    /// G1 descrittivo: il modello descrive senza agire (reroute_count >= max).
    pub g1_over_cap: bool,
    /// Azione produttiva ripetuta identica oltre soglia: (label, conteggio).
    pub repeated_action: Option<(String, i64)>,
    /// Riallocazione risorse: numero request_port ravvicinate (a prescindere dal
    /// label: il variare del label E' il sintomo del loop).
    pub reallocation_count: i64,
    /// Soglia riallocazione.
    pub reallocation_threshold: i64,
    /// `true` se il run ha gia' allocazioni/servizi attivi noti.
    pub has_active_resources: bool,
    /// Budget di escalation gia' consumato.
    pub escalations: i64,
    /// Budget massimo di escalation.
    pub max_escalations: i64,
    /// `true` se c'e' un candidato di escalation disponibile.
    pub has_escalation_candidate: bool,
    /// Assi per cui la forza-azione (GUIDE) e' GIA' stata applicata.
    pub already_guided: HashSet<String>,
    /// Assi per cui la DIAGNOSI FORZATA (force_diagnose) e' GIA' stata applicata.
    pub already_diagnosed: HashSet<String>,
    /// `true` abilita lo stadio intermedio force_diagnose per repeated_action.
    pub force_diagnose_enabled: bool,
}

impl Default for ProgressSignals {
    fn default() -> Self {
        // Default neutri identici ai default Python (ProgressSignals dataclass).
        Self {
            exploration_count: 0,
            exploration_threshold: 6,
            signature_loop_tool: None,
            g1_over_cap: false,
            repeated_action: None,
            reallocation_count: 0,
            reallocation_threshold: 3,
            has_active_resources: false,
            escalations: 0,
            max_escalations: 3,
            has_escalation_candidate: false,
            already_guided: HashSet::new(),
            already_diagnosed: HashSet::new(),
            force_diagnose_enabled: false,
        }
    }
}

/// Esito della decisione. Il chiamante lo APPLICA (inietta nudge, rimuove tool,
/// escala, fa il return di abort); la LOGICA vive qui.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressDecision {
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis: Option<Axis>,
    /// GUIDE: rimuovi i tool di sola lettura e obbliga tool_choice required.
    pub force_action: bool,
    /// Testo del nudge assertivo da iniettare (None se non serve).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge_text: Option<String>,
    /// Solo per ABORT: lo stop_reason coordinato (instradato a final_gate).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Spiegazione breve per log/meta_step (perche' questa mossa).
    pub reason: String,
}

/// Direttiva porte CONDIZIONALE (grounding). Vedi `_port_directive` Python.
fn port_directive(has_active_resources: bool) -> &'static str {
    if has_active_resources {
        "per le porte NON allocarne di nuove: i servizi del progetto sono gia' \
attivi (vedi blocco RISORSE PROGETTO), riusa o riavvia quelli esistenti \
invece di chiederne di nuove"
    } else {
        "request_port SOLO per un servizio NUOVO (non ancora in ascolto)"
    }
}

/// Nudge ASSERTIVO per l'esplorazione. Vedi `_exploration_nudge` Python.
fn exploration_nudge(count: i64, has_active_resources: bool) -> String {
    format!(
        "STOP esplorazione: hai gia' letto/cercato {count} volte di fila senza \
produrre nulla. I tool di sola lettura sono ora DISABILITATI per questo \
turno: l'unico modo di avanzare e' agire sul progetto (modifica file o \
comando di esecuzione/verifica; {}) \
oppure, se la richiesta era una domanda, rispondere subito a parole con il \
risultato.",
        port_directive(has_active_resources)
    )
}

/// Nudge GROUNDED per il loop di riallocazione porte. Vedi
/// `_resource_reallocation_nudge` Python.
fn resource_reallocation_nudge(count: i64) -> String {
    format!(
        "STOP: hai gia' richiesto porte {count} volte di fila. I servizi del \
progetto sono gia' attivi: NON riallocare e NON variare il label per \
ottenere una porta nuova (request_port e' idempotente per scopo e ti \
ridarebbe comunque la porta esistente). Usa il blocco RISORSE PROGETTO nel \
contesto: riusa la porta del servizio del tuo scopo se e' attivo, riavvialo \
se e' allocato ma spento. Chiama request_port SOLO per un servizio NUOVO \
che non e' ancora in ascolto."
    )
}

/// Nudge per il loop su tool identico ripetuto. Vedi `_signature_nudge` Python.
fn signature_nudge(tool: &str) -> String {
    format!(
        "STOP: hai ripetuto la stessa tool call ('{tool}', stesso input) senza \
progresso. NON ripeterla. Se ti mancano informazioni fai una richiesta \
diversa e piu' specifica; altrimenti procedi con l'azione concreta \
successiva o riassumi lo stato a parole."
    )
}

/// Nudge per la risposta descrittiva su richiesta d'azione (G1). Vedi `_g1_nudge`.
fn g1_nudge() -> String {
    "STOP: hai descritto i passi senza eseguirli. I tool di sola lettura sono \
disabilitati per questo turno: avanza eseguendo il prossimo step concreto \
con una tool call che modifica il progetto o lancia un comando."
        .to_string()
}

/// Vocabolario build/test per il nudge build-aware. Vedi `_BUILD_TEST_LABEL_KEYWORDS`.
const BUILD_TEST_LABEL_KEYWORDS: &[&str] = &[
    "build",
    "tsc",
    "compile",
    "cargo check",
    "cargo build",
    "cargo test",
    "npm run",
    "npm test",
    "pnpm",
    "yarn",
    "lint",
    "eslint",
    "make",
    "pytest",
    "run_tests",
    "test",
    "gradle",
    "mvn",
    "go build",
    "go test",
];

/// True se il label del comando ripetuto e' un build/compilazione/test. Vedi
/// `_is_build_or_test_label` Python.
fn is_build_or_test_label(label: &str) -> bool {
    let l = label.to_lowercase();
    BUILD_TEST_LABEL_KEYWORDS.iter().any(|k| l.contains(k))
}

/// Nudge per la ripetizione identica di una azione produttiva (build/test-aware).
/// Vedi `_repeated_action_nudge` Python.
fn repeated_action_nudge(label: &str, count: i64) -> String {
    if is_build_or_test_label(label) {
        format!(
            "STOP: hai gia' eseguito '{label}' {count} volte. Ri-eseguire un \
build/test NON riduce gli errori: li riduce solo correggere i file \
che l'output segnala (ogni errore ha file:riga; in fondo il totale, \
es. 'Found N errors'). Lavora sulla causa, non sulla ripetizione del \
comando. Se l'output era troncato e non vedi tutti gli errori, \
correggi quelli visibili e segnala che ne mancano, invece di \
ri-eseguire per scoprirli."
        )
    } else {
        format!(
            "STOP: hai gia' eseguito la stessa azione ({label}) {count} volte. \
Ripeterla identica NON cambia il risultato. NON ripeterla: leggi \
l'esito dell'esecuzione precedente, e poi (a) se l'azione e' riuscita, \
PROCEDI al passo successivo o concludi verificando il risultato reale; \
(b) se e' fallita, cambia approccio (causa radice diversa), non rieseguire \
lo stesso comando/edit."
        )
    }
}

/// Nudge di DIAGNOSI FORZATA (stadio tra GUIDE e ABORT). Vedi
/// `_force_diagnose_nudge` Python (build/test-aware).
fn force_diagnose_nudge(label: &str, count: i64) -> String {
    if is_build_or_test_label(label) {
        format!(
            "STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente \
non ha cambiato nulla. Ri-eseguire il build NON e' un'azione diversa: e' la \
stessa di prima. Un errore di build/test e' CORREGGIBILE: leggi gli errori \
dell'ultima esecuzione (file:riga riportati qui sotto), individua la CAUSA \
RADICE (tipo mancante / import errato / simbolo non definito, non il sintomo) \
leggendo l'output del comando qui sopra, e CORREGGI ORA i file segnalati con una \
tool call (edit_file/write_file); SOLO DOPO rie-esegui. NON chiudere il turno e \
NON dichiararti bloccato: un errore di build/test si risolve correggendo il \
codice, non e' una dipendenza mancante."
        )
    } else {
        format!(
            "STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente non \
ha cambiato nulla. Identifica la CAUSA RADICE del fallimento dall'esito \
esatto dell'ultima esecuzione (la causa reale, non il sintomo) e attaccala \
con un'azione diversa, non la stessa di prima. Se non esiste un'azione \
diversa praticabile, dichiara che sei bloccato e perche' (es. dipendenza / \
credenziale / permesso / servizio mancante): il turno chiudera' con la \
diagnosi e il prossimo passo, non con una ripetizione."
        )
    }
}

/// Punto unico: data la fotografia del progresso, decide la prossima mossa.
///
/// Gerarchia, applicata all'asse di stallo a priorita' piu' alta:
///   - se l'asse NON e' ancora stato guidato (forza-azione) -> GUIDE
///   - solo per repeated_action + flag: -> FORCE_DIAGNOSE
///   - altrimenti, se c'e' un candidato di escalation nel budget -> ESCALATE
///   - altrimenti -> ABORT (verso la verifica E2E)
///
/// Priorita' tra assi: esplorazione, signature-loop, resource_reallocation,
/// repeated_action, g1-descrittivo. Nessuno stallo -> proceed.
pub fn decide(signals: &ProgressSignals) -> ProgressDecision {
    // Determina l'asse di stallo prioritario (None = nessuno stallo bloccante).
    let axis: Option<Axis> = if signals.exploration_count
        >= 2 * signals.exploration_threshold.max(1)
    {
        Some(Axis::Exploration)
    } else if signals
        .signature_loop_tool
        .as_deref()
        .is_some_and(|t| !t.is_empty())
    {
        // Python: `elif signals.signature_loop_tool:` — truthy = non vuoto/non None.
        Some(Axis::Signature)
    } else if signals.reallocation_count >= signals.reallocation_threshold.max(1) {
        Some(Axis::ResourceReallocation)
    } else if signals.repeated_action.is_some() {
        Some(Axis::RepeatedAction)
    } else if signals.g1_over_cap {
        Some(Axis::G1Descriptive)
    } else {
        None
    };

    let Some(axis) = axis else {
        return ProgressDecision {
            action: Action::Proceed,
            axis: None,
            force_action: false,
            nudge_text: None,
            stop_reason: None,
            reason: "nessuno stallo bloccante".to_string(),
        };
    };

    let already = signals.already_guided.contains(axis.as_str());
    let already_diagnosed = signals.already_diagnosed.contains(axis.as_str());
    let can_escalate =
        signals.has_escalation_candidate && signals.escalations < signals.max_escalations;

    // Livello 1 — GUIDE (forza-azione): solo se non gia' tentato per questo asse.
    if !already {
        let nudge = match axis {
            Axis::Exploration => {
                exploration_nudge(signals.exploration_count, signals.has_active_resources)
            }
            Axis::Signature => {
                signature_nudge(signals.signature_loop_tool.as_deref().unwrap_or(""))
            }
            Axis::ResourceReallocation => resource_reallocation_nudge(signals.reallocation_count),
            Axis::RepeatedAction => {
                let (label, count) = signals
                    .repeated_action
                    .clone()
                    .unwrap_or_else(|| (String::new(), 0));
                repeated_action_nudge(&label, count)
            }
            Axis::G1Descriptive => g1_nudge(),
        };
        // Solo resource_reallocation resta SOFT (il nudge ordina di riusare le porte,
        // non c'e' un'azione correttiva diretta). Per repeated_action FORZIAMO una
        // nuova tool call (force-action): un'azione ripetuta che fallisce va CORRETTA,
        // non ripetuta ne' abbandonata -> rimuove i read-only e impone tool_choice.
        let force = !matches!(axis, Axis::ResourceReallocation);
        let reason = match axis {
            Axis::RepeatedAction => {
                format!("stallo {}: forza-azione correttiva (no ripetizione)", axis.as_str())
            }
            Axis::ResourceReallocation => {
                format!("stallo {}: nudge riusa-porte (no nuova allocazione)", axis.as_str())
            }
            _ => format!(
                "stallo {}: forza-azione (rimuovo read-only + tool_choice required)",
                axis.as_str()
            ),
        };
        return ProgressDecision {
            action: Action::Guide,
            axis: Some(axis),
            force_action: force,
            nudge_text: Some(nudge),
            stop_reason: None,
            reason,
        };
    }

    // Livello 1.5 — FORCE_DIAGNOSE: solo per repeated_action, dopo che la GUIDE
    // soft non ha cambiato nulla e PRIMA di escalation/abort. Abilitato da flag.
    if matches!(axis, Axis::RepeatedAction)
        && signals.force_diagnose_enabled
        && !already_diagnosed
    {
        let (label, count) = signals
            .repeated_action
            .clone()
            .unwrap_or_else(|| (String::new(), 0));
        return ProgressDecision {
            action: Action::ForceDiagnose,
            axis: Some(axis),
            // Forza una tool call correttiva: la diagnosi deve sfociare in un edit,
            // non in testo o resa. La scappatoia "dichiarati bloccato" resta solo nel
            // ramo non-build del nudge, per cause realmente bloccanti.
            force_action: true,
            nudge_text: Some(force_diagnose_nudge(&label, count)),
            stop_reason: None,
            reason: "stallo repeated_action: correzione forzata prima di escalation/abort"
                .to_string(),
        };
    }

    // Livello 2 — ESCALATE: gia' guidato ma ancora bloccato, c'e' budget.
    if can_escalate {
        return ProgressDecision {
            action: Action::Escalate,
            axis: Some(axis),
            force_action: false,
            nudge_text: None,
            stop_reason: None,
            reason: format!(
                "stallo {} persiste dopo forza-azione: escalation modello ({}/{})",
                axis.as_str(),
                signals.escalations + 1,
                signals.max_escalations
            ),
        };
    }

    // Livello 3 — ABORT verso verifica: guida+escalation esaurite.
    ProgressDecision {
        action: Action::Abort,
        axis: Some(axis),
        force_action: false,
        nudge_text: None,
        stop_reason: Some(ABORT_STOP_REASON.to_string()),
        reason: format!(
            "stallo {}: guida ed escalation esaurite -> chiusura con verifica E2E (final_gate)",
            axis.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nessuno_stallo_proceed() {
        let d = decide(&ProgressSignals::default());
        assert_eq!(d.action, Action::Proceed);
        assert_eq!(d.axis, None);
    }

    #[test]
    fn esplorazione_guida_con_risorse_attive() {
        let signals = ProgressSignals {
            exploration_count: 12,
            exploration_threshold: 6,
            has_active_resources: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert_eq!(d.axis, Some(Axis::Exploration));
        assert!(d.force_action);
        assert!(d.nudge_text.as_deref().unwrap().contains("NON allocarne di nuove"));
    }

    #[test]
    fn repeated_action_forza_azione_correttiva() {
        // Un'azione ripetuta che fallisce (es. build) va CORRETTA, non ripetuta ne'
        // abbandonata: la GUIDE forza una tool call (force_action=true) cosi' il
        // modello DEVE agire. Prima era force=false (solo nudge testuale) -> un
        // modello debole si arrendeva e scattava l'ABORT (incidente pnpm build).
        let signals = ProgressSignals {
            repeated_action: Some(("run_command: npm run build".to_string(), 3)),
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(d.force_action, "repeated_action ora forza l'azione correttiva");
    }

    #[test]
    fn abort_dopo_guida_senza_escalation() {
        let mut guided = HashSet::new();
        guided.insert("signature".to_string());
        let signals = ProgressSignals {
            signature_loop_tool: Some("read_file".to_string()),
            already_guided: guided,
            has_escalation_candidate: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Abort);
        assert_eq!(d.stop_reason.as_deref(), Some(ABORT_STOP_REASON));
    }
}
