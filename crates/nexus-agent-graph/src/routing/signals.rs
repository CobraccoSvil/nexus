//! Dipendenze PURE del routing, portate 1:1 dal brain Python.
//!
//! Sono le funzioni deterministiche (nessun IO, nessuna lettura DB) che le
//! `route_after_*` consultano per decidere. La config DB-driven arriva sempre
//! come parametro ([`super::config::RoutingConfig`], regola G). Punto unico
//! (regola L): se un giorno il path Rust sara' imboccato, i nodi delegano qui.
//!
//! Riferimenti Python (`brain/agents/nodes/helpers.py` salvo nota):
//!   - `_detect_unfulfilled_intent`        -> [`detect_unfulfilled_intent`]
//!   - `detect_pending_steps_report`       -> [`detect_pending_steps_report`]
//!   - `has_productive_action_in_history`  -> [`has_productive_action_in_history`]
//!   - `has_filesystem_mutation_in_history`-> [`has_filesystem_mutation_in_history`]
//!   - `_unfulfilled_signal`               -> [`unfulfilled_signal`] (routing.py)
//!   - `_is_software_task`  (final_gate.py)-> [`is_software_task`]
//!   - `_final_gate_eligible` (routing.py) -> [`final_gate_eligible`]
//!   - `todo_isolation_active` (orchestrator_config.py) -> [`todo_isolation_active`]

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::state::{AgentState, ContentBlock, Message, MessageContent};

use super::config::RoutingConfig;

// ──────────────────────────────────────────────────────────────────────────
//  Estrazione tool_use dalla history (fatto strutturale "questo run ha agito")
// ──────────────────────────────────────────────────────────────────────────

/// Itera i nomi dei tool_use emessi da ogni `Message::Ai` della history.
///
/// In Python `has_productive_action_in_history` / `has_filesystem_mutation_in_history`
/// leggono i tool_use da `additional_kwargs["anthropic_content"]` (blocchi
/// `{"type":"tool_use","name":...}`). Nel modello Rust un `Message::Ai` porta
/// i tool_use in DUE forme equivalenti a seconda di chi ha prodotto il
/// messaggio: il campo `tool_calls` (forma OpenAI-compat, da `lc_serde`) e/o i
/// blocchi `ContentBlock::ToolUse` nel `content` (forma Anthropic, equivalente
/// all'`anthropic_content` Python). Guardiamo ENTRAMBI per restare fedeli alla
/// semantica Python in ogni rappresentazione del messaggio.
fn ai_tool_use_names(messages: &[Message]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for m in messages {
        if let Message::Ai {
            content,
            tool_calls,
        } = m
        {
            // Forma OpenAI-compat: tool_calls.
            for tc in tool_calls {
                names.push(tc.name.as_str());
            }
            // Forma Anthropic: blocchi tool_use nel content (== anthropic_content).
            if let MessageContent::Blocks(blocks) = content {
                for b in blocks {
                    if let ContentBlock::ToolUse { name, .. } = b {
                        names.push(name.as_str());
                    }
                }
            }
        }
    }
    names
}

/// Tool di SOLA esplorazione (`_EXPLORATION_ONLY_TOOLS` Python): leggono/ispezionano
/// senza produrre side-effect. Un tool_use con nome NON in questo set conta come
/// azione produttiva. Lista tenuta allineata 1:1 a helpers.py.
const EXPLORATION_ONLY_TOOLS: &[&str] = &[
    "nexus_list_archive_entries",
    "nexus_read_archive_entry",
    "nexus_inspect_attachment",
    "nexus_extract_figma_structure",
    "nexus_list_attachments",
    "nexus_read_attachment",
    "nexus_extract_docx_text",
    "nexus_extract_xlsx_data",
    "nexus_extract_pdf_text",
    "nexus_describe_image_attachment",
    "read_file",
    "list_files",
    "grep",
    "read_file_lines",
    "search_in_files",
    "nexus_mcp_tool_search",
    "nexus_get_worklog",
];

/// True se il run ha gia' eseguito almeno UN'azione PRODUTTIVA (tool_use con
/// nome NON in `EXPLORATION_ONLY_TOOLS`). Vedi `has_productive_action_in_history`.
///
/// Punto unico (regola L) del fatto strutturale "questo run ha gia' agito".
pub fn has_productive_action_in_history(messages: &[Message]) -> bool {
    ai_tool_use_names(messages)
        .into_iter()
        .any(|name| !EXPLORATION_ONLY_TOOLS.contains(&name))
}

/// True se il run ha eseguito almeno un tool che MUTA filesystem/progetto.
/// Vedi `has_filesystem_mutation_in_history`. La lista mutators arriva dalla
/// config (setting `agent.tools.result_cache_mutators`, mig 0394).
pub fn has_filesystem_mutation_in_history(messages: &[Message], cfg: &RoutingConfig) -> bool {
    ai_tool_use_names(messages)
        .into_iter()
        .any(|name| cfg.fs_mutator_tools.iter().any(|m| m == name))
}

// ──────────────────────────────────────────────────────────────────────────
//  Segnale lessicale "intenzione imminente non compiuta"
//  (_detect_unfulfilled_intent)
// ──────────────────────────────────────────────────────────────────────────

/// `_INTENT_NARRATION_PATTERNS` Python (1:1). Frasi precise IT/EN che annunciano
/// un'azione imminente non eseguita. Match come substring sulla CODA del testo.
const INTENT_NARRATION_PATTERNS: &[&str] = &[
    // Italiano — intenzione futura imminente.
    "inizio verificando",
    "inizio controllando",
    "inizio analizzando",
    "inizio leggendo",
    "inizio esaminando",
    "inizio con ",
    "inizio a ",
    "inizio dal",
    "inizio dalla",
    "comincio con",
    "comincio a ",
    "comincio verificando",
    "comincio dal",
    "iniziamo verificando",
    "iniziamo con",
    "iniziamo dal",
    "cominciamo con",
    "partiamo da",
    "procedo a ",
    "procedo con",
    "procedo alla",
    "procedo nel",
    "procedo ora",
    "procedo subito",
    "procediamo con",
    "vado a ",
    "ora vado",
    "adesso vado",
    "ora verifico",
    "ora controllo",
    "ora leggo",
    "ora analizzo",
    "ora eseguo",
    "ora apro",
    "ora esamino",
    "adesso verifico",
    "adesso controllo",
    "adesso leggo",
    "verifico la presenza",
    "verifico se",
    "verifico il",
    "verifico la config",
    "verifico ora",
    "controllo la presenza",
    "controllo se",
    "controllo il",
    "controllo la config",
    "esamino il",
    "esamino la",
    "leggo il",
    "leggo la config",
    "fammi verificare",
    "fammi controllare",
    "fammi leggere",
    "fammi dare un",
    "fammi guardare",
    "il prossimo passo",
    "prossimo step",
    "passo successivo",
    "passo a ",
    "proseguo con",
    "proseguo a ",
    // Italiano — gerundio "sto + gerundio".
    "sto procedendo",
    "procedendo con",
    "procedendo a ",
    "procedendo alla",
    "sto creando",
    "sto implementando",
    "sto scrivendo",
    "sto aggiungendo",
    "sto generando",
    "sto preparando",
    "sto sviluppando",
    "stiamo procedendo",
    "stiamo creando",
    "stiamo implementando",
    // Italiano — futuro semplice.
    "creerò ",
    "creero ",
    "implementerò ",
    "implementero ",
    "scriverò ",
    "scrivero ",
    "aggiungerò ",
    "aggiungero ",
    "genererò ",
    "generero ",
    "preparerò ",
    "preparero ",
    "continuerò ",
    "continuero ",
    "proseguirò ",
    "proseguiro ",
    "il prossimo file",
    "i prossimi file",
    "i prossimi test",
    // Italiano — perifrasi "continuo con" / "passo al".
    "continuo con",
    "continuo a ",
    "passo al",
    "passo alla",
    "passo ai",
    "ora creo",
    "ora implemento",
    "ora scrivo",
    "ora aggiungo",
    "adesso creo",
    "adesso implemento",
    "adesso scrivo",
    // Inglese — intenzione futura imminente.
    "let me check",
    "let me verify",
    "let me start",
    "let me read",
    "let me look",
    "let me inspect",
    "let me examine",
    "let me first",
    "let me begin",
    "i'll check",
    "i'll verify",
    "i'll start",
    "i'll read",
    "i'll look",
    "i'll first",
    "i'll begin",
    "i'll inspect",
    "i'll examine",
    "i will check",
    "i will verify",
    "i will start",
    "i will read",
    "i'm going to",
    "i am going to",
    "let's check",
    "let's verify",
    "let's start",
    "let's look",
    "next, i",
    "now i'll",
    "now i will",
    "first, i'll",
    "first i'll",
    "first, let me",
    // Inglese — present progressive + future complementari.
    "i'm proceeding",
    "i am proceeding",
    "i'll proceed",
    "i will proceed",
    "i'm creating",
    "i'm implementing",
    "i'm writing",
    "i'm adding",
    "moving on to",
    "continuing with",
    "next i will create",
    "i'll create",
    "i'll implement",
    "i'll write",
    "i'll add",
    "i will create",
    "i will implement",
    "i will write",
    "i will add",
    "the next step is",
    "the next file",
    // Italiano — POLLING/ATTESA.
    "attendo ",
    "attendo qualche",
    "attendo ancora",
    "attendo che",
    "attendo il",
    "attendo un",
    "aspetto ",
    "aspetto che",
    "aspetto qualche",
    "aspetto ancora",
    "ricontrollo",
    "ricontrollare",
    "verifico di nuovo",
    "controllo di nuovo",
    "verifico nuovamente",
    "controllo nuovamente",
    "riprovo tra",
    "riprovo a ",
    "riprovo subito",
    "riprovo ora",
    "provo di nuovo",
    "provo ancora",
    // Inglese — polling/attesa.
    "i'll check again",
    "let me check again",
    "i'll wait",
    "let me wait",
    "waiting for",
    "i'll retry",
    "let me retry",
    "checking again",
    "i'll verify again",
    "i'll re-check",
    "let me re-check",
    "i'll try again",
];

// Rilevamento MORFOLOGICO (`_FUTURE_1P_RE` / `_START_GERUND_RE`).
//
// `_FUTURE_1P_RE = r"\b(?!però\b)\w{2,}rò\b"`: la crate `regex` Rust NON supporta
// il lookahead negativo. Lo emuliamo in due passi: matchiamo `\b\w{2,}rò\b` e
// scartiamo l'unico caso escluso ("però"). Comportamento identico al Python:
// "però" finisce in "rò" (p-e-r-ò) e verrebbe altrimenti catturato.
static FUTURE_1P_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\w{2,}rò\b").expect("regex future 1p valida"));

static START_GERUND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(inizio|comincio|sto|stiamo|iniziamo|cominciamo|ora|adesso|poi|quindi|prima|dopo)\s+\w*ndo\b",
    )
    .expect("regex start gerund valida")
});

/// Ultimi `n` caratteri di una stringa (per CODEPOINT, come lo slice Python
/// `text[-n:]`). Necessario perche' i pattern contengono accenti multibyte.
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

/// True se l'OUTPUT annuncia un'azione imminente ma non l'ha eseguita.
/// Vedi `_detect_unfulfilled_intent`. Valuta la CODA (ultimi 400 char lower-case).
pub fn detect_unfulfilled_intent(text: Option<&str>) -> bool {
    let Some(text) = text else {
        return false;
    };
    if text.trim().is_empty() {
        return false;
    }
    // text.strip().lower()[-400:] — su codepoint, come Python.
    let tail = tail_chars(&text.trim().to_lowercase(), 400);
    if INTENT_NARRATION_PATTERNS.iter().any(|p| tail.contains(p)) {
        return true;
    }
    // _FUTURE_1P_RE con esclusione di "però" (emula il lookahead negativo).
    if FUTURE_1P_RE
        .find_iter(&tail)
        .any(|m| m.as_str() != "però")
    {
        return true;
    }
    START_GERUND_RE.is_match(&tail)
}

// ──────────────────────────────────────────────────────────────────────────
//  Segnale STRUTTURALE "report con passi pendenti" (detect_pending_steps_report)
// ──────────────────────────────────────────────────────────────────────────

/// `_PENDING_STEPS_LABELS` Python (1:1). Etichette-trigger multilingua di un
/// elenco di passi pendenti. Match come substring lower-case.
const PENDING_STEPS_LABELS: &[&str] = &[
    // Italiano.
    "prossimi passi necessari",
    "prossimi passi",
    "prossimi step",
    "passi successivi",
    "passi rimanenti",
    "passi da svolgere",
    "passi da completare",
    "passi da fare",
    "step successivi",
    "step rimanenti",
    "step da fare",
    "cosa manca",
    "cosa resta da fare",
    "lavoro rimanente",
    "azioni rimanenti",
    "azioni da svolgere",
    "azioni da completare",
    "azioni necessarie",
    "da fare",
    "todo",
    "to do",
    "to-do",
    // Inglese.
    "next steps",
    "next step",
    "remaining steps",
    "remaining work",
    "remaining tasks",
    "pending steps",
    "pending tasks",
    "outstanding tasks",
    "outstanding work",
    "to be done",
    "still to do",
    "things to do",
    "what's left",
    "whats left",
    "what remains",
    "follow-up actions",
    "follow up actions",
    "action items",
    // Spagnolo.
    "próximos pasos",
    "proximos pasos",
    "pasos siguientes",
    "pasos restantes",
    "pendientes",
    "por hacer",
    "queda por hacer",
    // Francese.
    "prochaines étapes",
    "prochaines etapes",
    "étapes suivantes",
    "etapes suivantes",
    "étapes restantes",
    "etapes restantes",
    "à faire",
    "a faire",
    "reste à faire",
    "reste a faire",
    // Tedesco.
    "nächste schritte",
    "naechste schritte",
    "verbleibende schritte",
    "noch zu tun",
    "offene aufgaben",
];

// `_PENDING_ITEM_RE`: item numerato "1." / "1)" / bullet "- " / "* " / "•".
static PENDING_ITEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:[0-9]{1,2}[.)]|[-*+•])\s+\S").expect("regex pending item valida")
});

/// Sottostringa di `s` a partire dal byte successivo all'etichetta, lunga al
/// massimo 1500 CODEPOINT (come `text[start:start+1500]` Python). `start` e' un
/// indice di codepoint (numero di char prima dell'etichetta nel testo originale).
fn window_after(text: &str, char_start: usize, len_chars: usize) -> String {
    text.chars().skip(char_start).take(len_chars).collect()
}

/// True se `text` e' un REPORT con elenco esplicito di passi ancora da svolgere.
/// Vedi `detect_pending_steps_report`. `min_items`/`enabled` arrivano dalla config.
pub fn detect_pending_steps_report(text: Option<&str>, cfg: &RoutingConfig) -> bool {
    let Some(text) = text else {
        return false;
    };
    if text.trim().is_empty() {
        return false;
    }
    if !cfg.pending_steps_detection_enabled {
        return false;
    }
    let min_items = cfg.pending_steps_min_items.max(1) as usize;

    let lower = text.to_lowercase();
    // Trova la PRIMA etichetta-trigger e analizza l'elenco subito sotto.
    for label in PENDING_STEPS_LABELS {
        // Indice in BYTE nella stringa lower-case (per trovare l'etichetta).
        let Some(byte_idx) = lower.find(label) else {
            continue;
        };
        // start = idx + len(label) in codepoint (Python conta in caratteri).
        // Converto: numero di char prima dell'etichetta + lunghezza etichetta.
        let chars_before = lower[..byte_idx].chars().count();
        let label_chars = label.chars().count();
        let char_start = chars_before + label_chars;
        // Finestra di 1500 codepoint sul testo ORIGINALE (come Python).
        let window = window_after(text, char_start, 1500);
        let matches = PENDING_ITEM_RE.find_iter(&window).count();
        if matches >= min_items {
            return true;
        }
    }
    false
}

// ──────────────────────────────────────────────────────────────────────────
//  Segnale SEMANTICO "esito non compiuto" (_unfulfilled_signal, routing.py)
// ──────────────────────────────────────────────────────────────────────────

/// Estrae il bool `fulfilled` da `closure_verdict` se presente e di tipo bool.
fn closure_verdict_fulfilled(state: &AgentState) -> Option<bool> {
    match &state.closure_verdict {
        Some(Value::Object(map)) => match map.get("fulfilled") {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

/// Segnale SEMANTICO "esito non compiuto" con gerarchia de-lessicalizzata.
/// Vedi `_unfulfilled_signal` (routing.py). Ordine:
///   1. verdetto closure_judge (bool) -> `not fulfilled`;
///   2. segnale strutturale `detect_pending_steps_report(result)` -> True;
///   3. fallback lessicale `_detect_unfulfilled_intent(result)`.
pub fn unfulfilled_signal(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if let Some(fulfilled) = closure_verdict_fulfilled(state) {
        return !fulfilled;
    }
    let result = state.result.as_deref();
    // (2) Segnale strutturale "report con passi pendenti".
    if detect_pending_steps_report(result, cfg) {
        return true;
    }
    // (3) Fallback lessicale.
    detect_unfulfilled_intent(result)
}

// ──────────────────────────────────────────────────────────────────────────
//  Eleggibilita' final gate (_is_software_task / _final_gate_eligible)
// ──────────────────────────────────────────────────────────────────────────

/// True se il run va trattato come task software. Vedi `_is_software_task`.
/// Due segnali in OR: mutazione filesystem strutturale, oppure intent in whitelist.
/// In Python l'intent e' `state.user_intent or state.intent`: il campo `intent`
/// (non promosso a campo nativo) vive nello schema aperto `extra`.
pub fn is_software_task(state: &AgentState, cfg: &RoutingConfig) -> bool {
    // (1) STRUTTURALE primario: ha mutato il filesystem/progetto.
    if has_filesystem_mutation_in_history(&state.messages, cfg) {
        return true;
    }
    // (2) Whitelist intent (user_intent, fallback su extra["intent"]).
    let intent = state
        .user_intent
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| state.extra.get("intent").and_then(Value::as_str))
        .unwrap_or("")
        .to_lowercase();
    if intent.is_empty() {
        return false;
    }
    cfg.final_gate_software_intents.contains(&intent)
}

/// True se per questo stato e' eleggibile la verifica E2E pre-chiusura.
/// Vedi `_final_gate_eligible` (routing.py): esclude plan_phase, richiede gate
/// abilitato + task software + ciclo final_gate sotto il cap.
pub fn final_gate_eligible(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if state.plan_phase_active.unwrap_or(false) {
        return false;
    }
    if !cfg.final_gate_enabled || !is_software_task(state, cfg) {
        return false;
    }
    let cycle = state.final_gate_cycle.unwrap_or(0);
    cycle < cfg.final_gate_max_cycles
}

// ──────────────────────────────────────────────────────────────────────────
//  Isolamento todo (todo_isolation_active, orchestrator_config.py)
// ──────────────────────────────────────────────────────────────────────────

/// Modalita' di automazione che attivano l'esecuzione autonoma continua.
/// `_AUTONOMOUS_MODES` Python (confronto case-insensitive su stringa cruda).
const AUTONOMOUS_MODES: &[&str] = &["automatic", "automatico", "continuous", "continuo"];

/// True se il run deve eseguire i todo come sub-run ISOLATE sequenziali.
/// Vedi `todo_isolation_active`. Richiede TUTTE e tre: plan_phase_active True,
/// modalita' autonoma, setting abilitato.
///
/// NB: in Python il confronto della modalita' usa la stringa cruda di
/// `automation_mode or behavior_mode`; qui `automation_mode` e' un enum, quindi
/// ne ricaviamo la forma stringa snake_case (serde) prima del fallback su
/// `behavior_mode`. Le label dell'enum (`automatic`/`continuous`) sono incluse
/// in `AUTONOMOUS_MODES`, quindi il comportamento e' identico.
pub fn todo_isolation_active(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if state.plan_phase_active != Some(true) {
        return false;
    }
    let mode = automation_or_behavior_mode(state);
    if !AUTONOMOUS_MODES.contains(&mode.as_str()) {
        return false;
    }
    cfg.todo_isolation_enabled
}

/// Stringa di modalita' per il gate isolamento/G1: `automation_mode` (enum ->
/// snake_case) con fallback a `behavior_mode`, lower-case e trimmata. Replica
/// `str(state.get("automation_mode") or state.get("behavior_mode") or "")`.
fn automation_or_behavior_mode(state: &AgentState) -> String {
    if let Some(mode) = state.automation_mode {
        // serde dell'enum produce la label snake_case ("automatic", ...).
        if let Value::String(s) = serde_json::to_value(mode).unwrap_or(Value::Null) {
            return s.trim().to_lowercase();
        }
    }
    state
        .behavior_mode
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AutomationMode, ToolUse};
    use serde_json::json;

    fn ai_with_tool(name: &str) -> Message {
        Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: json!({}),
            }],
        }
    }

    fn ai_with_block_tool(name: &str) -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: json!({}),
            }]),
            tool_calls: vec![],
        }
    }

    #[test]
    fn productive_action_da_tool_calls() {
        assert!(has_productive_action_in_history(&[ai_with_tool(
            "write_file"
        )]));
        // Solo esplorazione -> non produttivo.
        assert!(!has_productive_action_in_history(&[ai_with_tool(
            "read_file"
        )]));
    }

    #[test]
    fn productive_action_da_blocchi() {
        // Forma Anthropic (== anthropic_content Python).
        assert!(has_productive_action_in_history(&[ai_with_block_tool(
            "edit_file"
        )]));
        assert!(!has_productive_action_in_history(&[ai_with_block_tool(
            "grep"
        )]));
    }

    #[test]
    fn fs_mutation_da_config() {
        let cfg = RoutingConfig::default();
        assert!(has_filesystem_mutation_in_history(
            &[ai_with_tool("rename_file")],
            &cfg
        ));
        // read_file non e' un mutator.
        assert!(!has_filesystem_mutation_in_history(
            &[ai_with_tool("read_file")],
            &cfg
        ));
    }

    #[test]
    fn unfulfilled_intent_pero_escluso() {
        // "però" finisce in "rò" ma il lookahead negativo Python lo esclude.
        assert!(!detect_unfulfilled_intent(Some(
            "Tutto a posto, però fammi sapere."
        )));
        // Un futuro 1a persona reale matcha.
        assert!(detect_unfulfilled_intent(Some(
            "Ottimo. Adesso creerò il file."
        )));
    }

    #[test]
    fn unfulfilled_intent_pattern_lista() {
        assert!(detect_unfulfilled_intent(Some(
            "Ho visto il problema. Ora verifico il frontend."
        )));
        assert!(!detect_unfulfilled_intent(Some("Fatto, ho concluso.")));
        assert!(!detect_unfulfilled_intent(None));
    }

    #[test]
    fn pending_steps_report_min_items() {
        let cfg = RoutingConfig::default();
        let report = "Stato attuale: ok.\nProssimi passi necessari:\n1. Verificare X\n2. Eseguire Y";
        assert!(detect_pending_steps_report(Some(report), &cfg));
        // Un solo item < min_items(2).
        let uno = "Prossimi passi:\n1. Solo questo";
        assert!(!detect_pending_steps_report(Some(uno), &cfg));
    }

    #[test]
    fn software_task_da_intent() {
        let cfg = RoutingConfig::default();
        let mut state = AgentState {
            user_intent: Some("debug".into()),
            ..Default::default()
        };
        assert!(is_software_task(&state, &cfg));
        state.user_intent = Some("architecture".into());
        assert!(!is_software_task(&state, &cfg));
    }

    #[test]
    fn software_task_da_intent_extra() {
        let cfg = RoutingConfig::default();
        let mut state = AgentState::default();
        state.extra.insert("intent".into(), json!("frontend"));
        assert!(is_software_task(&state, &cfg));
    }

    #[test]
    fn final_gate_eligible_esclude_plan_phase() {
        let cfg = RoutingConfig::default();
        let state = AgentState {
            user_intent: Some("code".into()),
            plan_phase_active: Some(true),
            ..Default::default()
        };
        assert!(!final_gate_eligible(&state, &cfg));
    }

    #[test]
    fn todo_isolation_richiede_tutte() {
        let cfg_off = RoutingConfig::default();
        let mut state = AgentState {
            plan_phase_active: Some(true),
            automation_mode: Some(AutomationMode::Automatic),
            ..Default::default()
        };
        // Setting OFF -> false.
        assert!(!todo_isolation_active(&state, &cfg_off));
        let cfg_on = RoutingConfig {
            todo_isolation_enabled: true,
            ..RoutingConfig::default()
        };
        assert!(todo_isolation_active(&state, &cfg_on));
        // Modalita' non autonoma -> false.
        state.automation_mode = Some(AutomationMode::Confirm);
        assert!(!todo_isolation_active(&state, &cfg_on));
    }
}
