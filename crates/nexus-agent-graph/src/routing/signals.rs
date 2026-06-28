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
///
/// PUNTO UNICO (regola L) della lista: oltre a `has_productive_action_in_history`
/// la usa `decisions::loop_signatures::exploration_counter_update` (passata come
/// parametro per restare pura).
pub const EXPLORATION_ONLY_TOOLS: &[&str] = &[
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
    "nexus_transcribe_audio",
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

/// Riepilogo conciso dei tool eseguiti nella history, per nome con conteggio:
/// es. "5 azioni (write_file x3, run_command x2)". `None` se nessun tool_use.
/// Punto unico (regola L) del riepilogo-lavoro: l'executor lo allega al messaggio
/// quando il turno si interrompe (es. provider in cooldown), cosi' l'utente vede
/// COSA e' stato fatto e non solo l'errore. Ordine = prima apparizione.
pub fn summarize_actions_in_history(messages: &[Message]) -> Option<String> {
    let names = ai_tool_use_names(messages);
    if names.is_empty() {
        return None;
    }
    let total = names.len();
    let mut order: Vec<&str> = Vec::new();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for n in &names {
        if !counts.contains_key(n) {
            order.push(n);
        }
        *counts.entry(n).or_insert(0) += 1;
    }
    let parts: Vec<String> = order
        .iter()
        .map(|n| {
            let c = counts[n];
            if c > 1 {
                format!("{n} x{c}")
            } else {
                (*n).to_string()
            }
        })
        .collect();
    Some(format!("{total} azioni ({})", parts.join(", ")))
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
//  Detector strutturali su lista messaggi (anti-loop dell'executor)
//
//  Porting 1:1 di `helpers.py`. Tutti PURI su `&[Message]`: scansionano i
//  blocchi tool_use/tool_result. Usano segnali STRUTTURALI (exit_code/is_error)
//  con fallback lessicale [`TOOL_ERROR_HINTS`], come il Python. Punto unico
//  (regola L) della domanda "questo tool_use e' riuscito?" -> [`tool_result_outcome_after`].
// ──────────────────────────────────────────────────────────────────────────

/// Pattern testuali che indicano errore in un tool_result (`_TOOL_ERROR_HINTS`
/// Python, 1:1). Match case-insensitive. SOLO fallback lessicale: i segnali
/// strutturali (exit_code/is_error) hanno priorita'.
pub const TOOL_ERROR_HINTS: &[&str] = &[
    "error:",
    "errore:",
    "[error",
    "exit code: 1",
    "exit code 1",
    "command failed",
    "comando fallito",
    "traceback",
    "exception:",
    "fatal:",
    "syntax error",
    "not found",
    "non trovato",
    "cannot find module",
    "module not found",
    "permission denied",
    "connection refused",
    "timed out",
    "timeout",
    "404 not found",
    "500 internal",
    "econnrefused",
    "enoent",
    "enotfound",
    "eperm",
    "no such file",
    "is_error",
    "[errno",
];

/// Nome del tool di allocazione porte (`_PORT_REQUEST_TOOL` Python: "request_port").
const PORT_REQUEST_TOOL: &str = "request_port";

/// Tool che, se presenti nella history recente, indicano risorse gia' attive
/// note al run (`_resource_tools` Python).
const RESOURCE_TOOLS: &[&str] = &[PORT_REQUEST_TOOL, "list_active_services", "service_restart"];

/// True se uno degli hint lessicali compare nel testo (case-insensitive).
fn text_has_error_hint(text: &str) -> bool {
    let lower = text.to_lowercase();
    TOOL_ERROR_HINTS.iter().any(|h| lower.contains(h))
}

/// Estrae i tool_use `(name, input)` di un singolo [`Message::Ai`], guardando
/// ENTRAMBE le forme: `tool_calls` (OpenAI-compat) e `ContentBlock::ToolUse`
/// (Anthropic, == anthropic_content Python). Ritorna vuoto per gli altri ruoli.
fn message_tool_uses(m: &Message) -> Vec<(&str, &Value)> {
    let mut out: Vec<(&str, &Value)> = Vec::new();
    if let Message::Ai {
        content,
        tool_calls,
    } = m
    {
        for tc in tool_calls {
            out.push((tc.name.as_str(), &tc.input));
        }
        if let MessageContent::Blocks(blocks) = content {
            for b in blocks {
                if let ContentBlock::ToolUse { name, input, .. } = b {
                    out.push((name.as_str(), input));
                }
            }
        }
    }
    out
}

/// Esito strutturato di un campo `content` di tool_result (stringa o struttura).
/// Replica il fallback lessicale Python sul testo del risultato.
fn content_value_has_error(content: &Value) -> bool {
    match content {
        Value::String(s) => text_has_error_hint(s),
        Value::Array(arr) => arr.iter().any(|cc| {
            if let Value::Object(map) = cc {
                let txt = map
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| map.get("content").and_then(Value::as_str))
                    .unwrap_or("");
                text_has_error_hint(txt)
            } else {
                false
            }
        }),
        other => text_has_error_hint(&other.to_string()),
    }
}

/// Valuta l'esito di UN messaggio se e' un tool_result: `Some(true)`=errore,
/// `Some(false)`=successo, `None`=non e' un tool_result valutabile.
///
/// Gestisce ENTRAMBE le forme: [`Message::Tool`] (== `ToolMessage` langchain) e
/// i blocchi [`ContentBlock::ToolResult`] in un qualsiasi messaggio (==
/// `HumanMessage`+anthropic_content Python). Gerarchia dei segnali (contratto A):
///   1. `exit_code` STRUTTURATO (tool-comando): 0=successo, !=0=errore;
///   2. `is_error` STRUTTURATO del blocco/messaggio tool;
///   3. fallback lessicale [`TOOL_ERROR_HINTS`] sul testo.
/// Il `status` del `ToolMessage` Python (`status == "error"`) non esiste nel
/// modello Rust: e' coperto dal blocco `is_error`/lessicale equivalente.
fn message_tool_result_outcome(m: &Message) -> Option<bool> {
    match m {
        // ToolMessage langchain: il content puo' essere testo o blocchi.
        Message::Tool { content, .. } => match content {
            MessageContent::Text(s) => Some(text_has_error_hint(s)),
            MessageContent::Blocks(blocks) => {
                // Cerca un blocco con segnale strutturale; poi lessicale.
                for b in blocks {
                    if let ContentBlock::ToolResult {
                        is_error,
                        exit_code,
                        content,
                        ..
                    } = b
                    {
                        if let Some(ec) = exit_code {
                            return Some(*ec != 0);
                        }
                        if *is_error {
                            return Some(true);
                        }
                        if content_value_has_error(content) {
                            return Some(true);
                        }
                    }
                }
                // Nessun blocco tool_result strutturato: fallback su testo piatto.
                Some(blocks.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text } if text_has_error_hint(text))
                }))
            }
        },
        // anthropic_content tool_result in un HumanMessage (tool_dispatch_node
        // emette il tool_result come HumanMessage; gli AIMessage portano i
        // tool_use, mai i tool_result -> non valutati, come in Python).
        Message::Human { content } => {
            let MessageContent::Blocks(blocks) = content else {
                return None;
            };
            let mut found_result = false;
            for b in blocks {
                if let ContentBlock::ToolResult {
                    is_error,
                    exit_code,
                    content,
                    ..
                } = b
                {
                    found_result = true;
                    // 1) exit_code strutturato.
                    if let Some(ec) = exit_code {
                        return Some(*ec != 0);
                    }
                    // 2) is_error strutturato.
                    if *is_error {
                        return Some(true);
                    }
                    // 3) fallback lessicale sul testo.
                    if content_value_has_error(content) {
                        return Some(true);
                    }
                }
            }
            if found_result {
                Some(false)
            } else {
                None
            }
        }
        // AIMessage: porta tool_use, mai tool_result -> non e' un risultato.
        Message::Ai { .. } => None,
    }
}

/// Coda degli ultimi `lookback` messaggi (come `messages[-lookback:]` Python).
fn tail_messages(messages: &[Message], lookback: usize) -> &[Message] {
    let start = messages.len().saturating_sub(lookback);
    &messages[start..]
}

/// True se nella history ci sono gia' stati tool call effettivi (un `Message::Ai`
/// con almeno un tool_use). Vedi `_has_tool_calls_in_history`.
pub fn has_tool_calls_in_history(messages: &[Message]) -> bool {
    messages.iter().any(|m| !message_tool_uses(m).is_empty())
}

/// Esito del primo tool_result nei `max_ahead` messaggi dopo `recent[idx]`.
/// `Some(true)`=errore, `Some(false)`=successo, `None`=nessun risultato trovato.
/// Vedi `_tool_result_outcome_after` (max_ahead=3 default). Punto unico (regola L)
/// della domanda "il tool_use a recent[idx] e' riuscito?".
pub fn tool_result_outcome_after(recent: &[Message], idx: usize, max_ahead: usize) -> Option<bool> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        if let Some(outcome) = message_tool_result_outcome(nm) {
            return Some(outcome);
        }
    }
    None
}

/// Conta le chiamate `request_port` negli ultimi `lookback` messaggi (default 16).
/// Segnale STRUTTURALE del loop di riallocazione. NESSUN filtro su input/label.
/// Vedi `_count_recent_request_port`.
pub fn count_recent_request_port(messages: &[Message], lookback: usize) -> i64 {
    let recent = tail_messages(messages, lookback);
    let mut count = 0i64;
    for m in recent {
        for (name, _) in message_tool_uses(m) {
            if name == PORT_REQUEST_TOOL {
                count += 1;
            }
        }
    }
    count
}

/// True se nella history recente (default lookback 24) risulta gia' una risorsa
/// attiva nota al run (un tool_use request_port / list_active_services /
/// service_restart). Vedi `_has_active_resources_in_history`.
pub fn has_active_resources_in_history(messages: &[Message], lookback: usize) -> bool {
    let recent = tail_messages(messages, lookback);
    recent
        .iter()
        .flat_map(message_tool_uses)
        .any(|(name, _)| RESOURCE_TOOLS.contains(&name))
}

/// True se uno degli ultimi `lookback` tool message (default 4) indica errore.
/// Vedi `_detect_recent_tool_error`: scansiona in ordine INVERSO i soli
/// [`Message::Tool`] (== `ToolMessage`), si ferma dopo `lookback` di essi, e
/// segnala errore su `is_error` strutturato o hint lessicale.
pub fn detect_recent_tool_error(messages: &[Message], lookback: usize) -> bool {
    let mut checked = 0usize;
    for m in messages.iter().rev() {
        if checked >= lookback {
            break;
        }
        let Message::Tool { .. } = m else {
            continue;
        };
        checked += 1;
        if message_tool_result_outcome(m) == Some(true) {
            return true;
        }
    }
    false
}

/// Comandi shell tracciati da `_detect_repeated_failed_command` (1:1).
const FAILED_COMMAND_TOOLS: &[&str] = &["run_command", "run_service", "run_in_terminal"];

/// Rileva la ripetizione dello STESSO comando shell con ERRORE. Ritorna
/// `(Some(command), count)` della signature `command|working_dir` piu' frequente
/// che ha prodotto errore, `(None, 0)` se nessuna. Vedi
/// `_detect_repeated_failed_command` (lookback=12). Solo i comandi il cui
/// tool_result successivo (entro 3 step) e' errore vengono contati.
pub fn detect_repeated_failed_command(
    messages: &[Message],
    lookback: usize,
) -> (Option<String>, i64) {
    if messages.is_empty() {
        return (None, 0);
    }
    let recent = tail_messages(messages, lookback);
    // signature `command|working_dir` -> count; preferisce l'ultima in parita'.
    let mut failed: Vec<(String, i64)> = Vec::new();
    let mut last_signature: Option<String> = None;
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if !FAILED_COMMAND_TOOLS.contains(&name) {
                continue;
            }
            let cmd = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if cmd.is_empty() {
                continue;
            }
            let wd = input
                .get("working_dir")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let signature = format!("{cmd}|{wd}");
            // _detect_repeated_failed_command guarda i 3 messaggi successivi e
            // valuta il PRIMO ToolMessage trovato (max_ahead=3, ma si ferma al
            // primo result a prescindere dall'esito: break).
            let next_is_error = first_tool_result_is_error(recent, idx, 3);
            if next_is_error == Some(true) {
                bump(&mut failed, &signature);
                last_signature = Some(signature);
            }
        }
    }
    pick_top(&failed, last_signature.as_deref()).map_or((None, 0), |(sig, count)| {
        let cmd = sig.split_once('|').map(|(c, _)| c).unwrap_or(&sig).to_string();
        (Some(cmd), count)
    })
}

/// Tool PRODUTTIVI tracciati da `_detect_repeated_action` -> chiavi argomento
/// che ne definiscono l'identita' (`_REPEATED_ACTION_TOOLS` Python, 1:1).
fn repeated_action_keys(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "write_file" | "edit_file" => Some(&["path", "file_path"]),
        "run_command" | "run_service" | "run_in_terminal" => Some(&["command"]),
        _ => None,
    }
}

/// Rileva la ripetizione IDENTICA di un'azione produttiva (scrittura/comando),
/// a prescindere dall'esito. Ritorna `(Some(label), count)` della signature piu'
/// frequente (`name: valore` troncato a 120 char), `(None, 0)` se nessuna. Vedi
/// `_detect_repeated_action` (lookback=24). FALSO-DOPPIONE: le signature la cui
/// PRIMA occorrenza e' RIUSCITA (`tool_result_outcome_after == Some(false)`)
/// sono ESCLUSE dal conteggio (ridondanza innocua, non stallo).
pub fn detect_repeated_action(messages: &[Message], lookback: usize) -> (Option<String>, i64) {
    if messages.is_empty() {
        return (None, 0);
    }
    let recent = tail_messages(messages, lookback);
    let mut counts: Vec<(String, i64)> = Vec::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut succeeded: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_sig: Option<String> = None;
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            let Some(keys) = repeated_action_keys(name) else {
                continue;
            };
            // value = primo argomento non vuoto fra le chiavi candidate.
            let mut value = String::new();
            for k in keys {
                if let Some(v) = input.get(*k).and_then(Value::as_str) {
                    let v = v.trim();
                    if !v.is_empty() {
                        value = v.to_string();
                        break;
                    }
                }
            }
            if value.is_empty() {
                continue;
            }
            let sig = format!("{name}|{value}");
            bump(&mut counts, &sig);
            let label_value: String = value.chars().take(120).collect();
            set_label(&mut labels, &sig, format!("{name}: {label_value}"));
            last_sig = Some(sig.clone());
            // Esito strutturale: un successo marca la signature come riuscita.
            if tool_result_outcome_after(recent, idx, 3) == Some(false) {
                succeeded.insert(sig);
            }
        }
    }
    // Rimuove le signature riuscite (mai stallo da abort).
    counts.retain(|(sig, _)| !succeeded.contains(sig));
    pick_top(&counts, last_sig.as_deref()).map(|(sig, count)| {
        let label = labels
            .iter()
            .find(|(s, _)| *s == sig)
            .map(|(_, l)| l.clone())
            .unwrap_or(sig);
        (Some(label), count)
    }).unwrap_or((None, 0))
}

// ── Helper di conteggio condivisi dai due detector di ripetizione ───────────

/// Valuta il PRIMO tool_result trovato entro `max_ahead` messaggi dopo `idx`
/// e ritorna il suo esito (`break` al primo, come `_detect_repeated_failed_command`).
fn first_tool_result_is_error(recent: &[Message], idx: usize, max_ahead: usize) -> Option<bool> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        if let Message::Tool { .. } = nm {
            return message_tool_result_outcome(nm);
        }
    }
    None
}

/// Incrementa il contatore della signature in una lista-associativa ordinata
/// per inserimento (replica `dict[sig] = dict.get(sig,0)+1`).
fn bump(list: &mut Vec<(String, i64)>, sig: &str) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 += 1;
    } else {
        list.push((sig.to_string(), 1));
    }
}

/// Imposta/aggiorna la label leggibile di una signature.
fn set_label(list: &mut Vec<(String, String)>, sig: &str, label: String) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 = label;
    } else {
        list.push((sig.to_string(), label));
    }
}

/// Ritorna la signature con chiave massima `(count, sig == last)`, replicando
/// `max(items, key=lambda kv: (kv[1], kv[0] == last))` di Python. `max` tiene il
/// PRIMO massimo a parita' PIENA di chiave (sostituisce solo se STRETTAMENTE
/// maggiore), quindi usiamo `>` e scorriamo in ordine d'inserimento. Il flag
/// `sig == last` (l'ultima signature processata) prevale a parita' di count.
fn pick_top(list: &[(String, i64)], last: Option<&str>) -> Option<(String, i64)> {
    let mut best: Option<&(String, i64)> = None;
    for item in list {
        let item_key = (item.1, Some(item.0.as_str()) == last);
        match best {
            None => best = Some(item),
            Some(b) => {
                let best_key = (b.1, Some(b.0.as_str()) == last);
                if item_key > best_key {
                    best = Some(item);
                }
            }
        }
    }
    best.map(|(s, c)| (s.clone(), *c))
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

    #[test]
    fn summarize_azioni_conteggio_e_ordine() {
        // Conteggio per nome, ordine di prima apparizione, formato "N azioni (...)".
        let msgs = vec![
            ai_with_tool("write_file"),
            ai_with_tool("run_command"),
            ai_with_tool("write_file"),
        ];
        assert_eq!(
            summarize_actions_in_history(&msgs).as_deref(),
            Some("3 azioni (write_file x2, run_command)")
        );
        // Nessun tool_use -> None (turno senza azioni).
        assert_eq!(summarize_actions_in_history(&[]), None);
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

    // Helper: AIMessage con tool_use (forma anthropic_content) + input.
    fn ai_tool_input(name: &str, input: Value) -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input,
            }]),
            tool_calls: vec![],
        }
    }

    // Helper: HumanMessage con anthropic_content tool_result strutturato.
    fn human_tool_result(exit_code: Option<i64>, is_error: bool, text: &str) -> Message {
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String(text.into()),
                is_error,
                exit_code,
            }]),
        }
    }

    // Helper: ToolMessage langchain con content testuale.
    fn tool_msg(text: &str) -> Message {
        Message::Tool {
            tool_call_id: "c1".into(),
            content: MessageContent::text(text),
        }
    }

    #[test]
    fn has_tool_calls_history() {
        assert!(has_tool_calls_in_history(&[ai_with_tool("read_file")]));
        assert!(!has_tool_calls_in_history(&[Message::Ai {
            content: MessageContent::text("solo testo"),
            tool_calls: vec![],
        }]));
    }

    #[test]
    fn outcome_after_exit_code_primario() {
        // tool_use seguito da tool_result con exit_code=0 -> successo.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "ls"})),
            human_tool_result(Some(0), false, "ok"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs, 0, 3), Some(false));
        // exit_code != 0 -> errore, anche senza is_error.
        let msgs2 = vec![
            ai_tool_input("run_command", json!({"command": "ls"})),
            human_tool_result(Some(2), false, "tutto bene a parole"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs2, 0, 3), Some(true));
        // Nessun risultato dopo -> None.
        let msgs3 = vec![ai_tool_input("run_command", json!({"command": "ls"}))];
        assert_eq!(tool_result_outcome_after(&msgs3, 0, 3), None);
    }

    #[test]
    fn outcome_after_lessicale_fallback() {
        // Niente exit_code/is_error: fallback su _TOOL_ERROR_HINTS.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "x"})),
            human_tool_result(None, false, "bash: command not found"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs, 0, 3), Some(true));
        let msgs_ok = vec![
            ai_tool_input("run_command", json!({"command": "x"})),
            human_tool_result(None, false, "Compilato con successo"),
        ];
        assert_eq!(tool_result_outcome_after(&msgs_ok, 0, 3), Some(false));
    }

    #[test]
    fn conta_request_port_senza_filtro_label() {
        let msgs = vec![
            ai_tool_input("request_port", json!({"label": "web"})),
            ai_tool_input("request_port", json!({"label": "api"})),
            ai_tool_input("read_file", json!({"path": "a"})),
        ];
        assert_eq!(count_recent_request_port(&msgs, 16), 2);
        assert!(has_active_resources_in_history(&msgs, 24));
        // Senza request_port/servizi -> nessuna risorsa attiva.
        let solo_read = vec![ai_tool_input("read_file", json!({"path": "a"}))];
        assert!(!has_active_resources_in_history(&solo_read, 24));
    }

    #[test]
    fn recent_tool_error_solo_tool_message() {
        // Ultimo ToolMessage con hint -> errore.
        assert!(detect_recent_tool_error(
            &[tool_msg("Error: build failed")],
            4
        ));
        // ToolMessage pulito -> nessun errore.
        assert!(!detect_recent_tool_error(&[tool_msg("done ok")], 4));
    }

    #[test]
    fn repeated_failed_command_stessa_signature() {
        // Stesso comando fallito 2 volte -> rilevato. _detect_repeated_failed_command
        // valuta SOLO i ToolMessage successivi (1:1 col Python, che guarda
        // isinstance(nm, ToolMessage)), quindi qui usiamo tool_msg.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "npm i", "working_dir": "/p"})),
            tool_msg("error: build failed"),
            ai_tool_input("run_command", json!({"command": "npm i", "working_dir": "/p"})),
            tool_msg("error: build failed"),
        ];
        let (cmd, count) = detect_repeated_failed_command(&msgs, 12);
        assert_eq!(cmd.as_deref(), Some("npm i"));
        assert_eq!(count, 2);
        // Comando RIUSCITO (ToolMessage pulito) -> non contato.
        let ok = vec![
            ai_tool_input("run_command", json!({"command": "npm i"})),
            tool_msg("done ok"),
        ];
        assert_eq!(detect_repeated_failed_command(&ok, 12), (None, 0));
    }

    #[test]
    fn repeated_action_esclude_signature_riuscita() {
        // edit_file applicato con successo poi ri-emesso e fallito: la prima
        // occorrenza riuscita ESCLUDE la signature dal conteggio (falso-doppione).
        let msgs = vec![
            ai_tool_input("edit_file", json!({"path": "a.rs"})),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input("edit_file", json!({"path": "a.rs"})),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        assert_eq!(detect_repeated_action(&msgs, 24), (None, 0));
        // Stessa scrittura ripetuta SENZA mai riuscire -> stallo rilevato.
        let stallo = vec![
            ai_tool_input("write_file", json!({"path": "b.rs"})),
            human_tool_result(None, true, "permission denied"),
            ai_tool_input("write_file", json!({"path": "b.rs"})),
            human_tool_result(None, true, "permission denied"),
        ];
        let (label, count) = detect_repeated_action(&stallo, 24);
        assert_eq!(label.as_deref(), Some("write_file: b.rs"));
        assert_eq!(count, 2);
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

/// Golden di parita' 1:1 vs Python per i detector strutturali. Carica
/// `/tmp/golden_executor_detectors.json` (vedi `gen_golden_executor_detectors.py`).
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    /// Forma INTERMEDIA di un messaggio (replica i raw spec dello script Python).
    #[derive(Debug, Deserialize)]
    #[serde(tag = "kind")]
    enum RawMsg {
        #[serde(rename = "ai_tool")]
        AiTool {
            name: String,
            #[serde(default)]
            input: Value,
        },
        #[serde(rename = "ai_text")]
        AiText {
            #[serde(default)]
            text: String,
        },
        #[serde(rename = "tool")]
        Tool {
            #[serde(default)]
            text: String,
        },
        #[serde(rename = "human_result")]
        HumanResult {
            #[serde(default)]
            exit_code: Option<i64>,
            #[serde(default)]
            is_error: bool,
            #[serde(default)]
            text: String,
        },
    }

    impl RawMsg {
        fn to_message(&self) -> Message {
            match self {
                RawMsg::AiTool { name, input } => Message::Ai {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "golden".into(),
                        name: name.clone(),
                        input: if input.is_null() { json!({}) } else { input.clone() },
                    }]),
                    tool_calls: vec![],
                },
                RawMsg::AiText { text } => Message::Ai {
                    content: MessageContent::text(text.clone()),
                    tool_calls: vec![],
                },
                RawMsg::Tool { text } => Message::Tool {
                    tool_call_id: "golden".into(),
                    content: MessageContent::text(text.clone()),
                },
                RawMsg::HumanResult {
                    exit_code,
                    is_error,
                    text,
                } => Message::Human {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "golden".into(),
                        content: Value::String(text.clone()),
                        is_error: *is_error,
                        exit_code: *exit_code,
                    }]),
                },
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        messages: Vec<RawMsg>,
        output: Value,
    }

    /// Mappa `Option<bool>` Python (True/False/None) al `Value` JSON corrispondente.
    fn opt_bool(v: Option<bool>) -> Value {
        match v {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_detectors.json generato da gen_golden_executor_detectors.py"]
    fn golden_executor_detectors() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_executor_detectors.json",
            "gen_golden_executor_detectors.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 20, "attesi >= 20 casi, trovati {}", cases.len());

        let cfg = RoutingConfig::default();
        let mut checked = 0usize;
        for c in &cases {
            let msgs: Vec<Message> = c.messages.iter().map(RawMsg::to_message).collect();
            let got: Value = match c.group.as_str() {
                "has_filesystem_mutation" => {
                    Value::Bool(has_filesystem_mutation_in_history(&msgs, &cfg))
                }
                "has_tool_calls_in_history" => Value::Bool(has_tool_calls_in_history(&msgs)),
                "tool_result_outcome_after" => {
                    opt_bool(tool_result_outcome_after(&msgs, 0, 3))
                }
                "detect_repeated_failed_command" => {
                    let (cmd, count) = detect_repeated_failed_command(&msgs, 12);
                    json!({ "command": cmd, "count": count })
                }
                "detect_repeated_action" => {
                    let (label, count) = detect_repeated_action(&msgs, 24);
                    json!({ "label": label, "count": count })
                }
                "count_recent_request_port" => {
                    Value::from(count_recent_request_port(&msgs, 16))
                }
                "has_active_resources_in_history" => {
                    Value::Bool(has_active_resources_in_history(&msgs, 24))
                }
                "detect_recent_tool_error" => Value::Bool(detect_recent_tool_error(&msgs, 4)),
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        println!("golden executor_detectors: {checked} casi verificati, tutti verdi");
    }
}
