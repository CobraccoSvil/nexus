//! Dipendenze PURE del routing, portate 1:1 dal brain Python.
//!
//! Sono le funzioni deterministiche (nessun IO, nessuna lettura DB) che le
//! `route_after_*` consultano per decidere. La config DB-driven arriva sempre
//! come parametro ([`super::config::RoutingConfig`], regola G). Punto unico
//! (regola L): se un giorno il path Rust sara' imboccato, i nodi delegano qui.
//!
//! Riferimenti Python (`brain/agents/nodes/helpers.py` salvo nota).
//! `_detect_unfulfilled_intent` (blacklist lessicale INTENT_NARRATION) e' stato
//! RIMOSSO (ADR 0018 fase 3): il segnale strutturale
//! [`crate::decisions::helpers::structural_unfulfilled_signal`] +
//! [`detect_pending_steps_report`] + task_complete (ADR 0034) lo sostituiscono.
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

use crate::decisions::loop_signatures::build_signature;
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
            ..
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
    // Osservazione RUNTIME di servizi/porte (letture pure, per natura da
    // POLLING: stesso input, output che evolve nel tempo). Senza questa
    // classificazione il signature-loop trattava tre letture identiche di log
    // come stallo anche con edit/build in mezzo (run 2c41b145:
    // gemini-2.5-pro interrotto mentre monitorava il dev server tra una
    // correzione e l'altra). Da read-only ereditano: sconto post-progresso nel
    // signature-loop, soglia repeated_action piu' alta, conteggio nel budget
    // esplorazione (il polling infinito a vuoto resta guidato/interrotto).
    "read_service_output",
    "tail_service_logs",
    "list_active_services",
    "nexus_list_ports",
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

/// Come [`has_productive_action_in_history`] ma limitata agli ULTIMI `lookback`
/// messaggi: distingue "il run sta producendo lavoro ADESSO" da "ha agito all'inizio
/// e ora gira a vuoto". Usata dal gate G1 loop-conclamato per NON abortire un run che
/// ha appena eseguito azioni concrete (anti falso-negativo, regola H): un run reale
/// aveva installato i browser Playwright + system-deps e fatto passare il test E2E,
/// ma il vecchio gate lessicale "non compiuto" (blacklist NARRAZIONE, rimossa con
/// ADR 0018 fase 3) lo abortiva ignorando i 16 tool riusciti, sostituendo il
/// successo con un messaggio di resa. Il segnale STRUTTURALE prevale sempre.
pub fn has_recent_productive_action(messages: &[Message], lookback: usize) -> bool {
    let start = messages.len().saturating_sub(lookback);
    has_productive_action_in_history(&messages[start..])
}

/// Elenca i file modificati con SUCCESSO (edit_file/write_file con tool_result NON
/// errore) negli ultimi `lookback` messaggi, in ordine di prima apparizione, senza
/// duplicati. Usata per un recap ONESTO: un ABORT non deve dichiarare "File toccati:
/// nessuno" quando l'agente ha realmente applicato modifiche (regola H). Pura.
pub fn modified_files_from_messages(messages: &[Message], lookback: usize) -> Vec<String> {
    let recent = tail_messages(messages, lookback);
    let mut out: Vec<String> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if !matches!(name, "edit_file" | "write_file") {
                continue;
            }
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(path) = path else { continue };
            // Solo edit RIUSCITO (outcome == Some(false) = non errore).
            if tool_result_outcome_after(recent, idx, 3) == Some(false)
                && !out.iter().any(|p| p == path)
            {
                out.push(path.to_string());
            }
        }
    }
    out
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

/// Estrae il set dei FILE modificati dal run: per ogni tool_use in history il cui
/// nome e' un mutator fs (`cfg.fs_mutator_tools`, DB-driven), l'argomento `path`
/// o `file_path`. PUNTO UNICO (regola L) usato dal final_gate per il gate
/// DELTA-aware: un errore di build conta come REGRESSIONE solo se colpisce un
/// file che il task ha toccato; il debito preesistente in file non toccati non
/// blocca la chiusura. Riusa [`message_tool_uses`] (stesso estrattore (name,
/// input) del resto del modulo).
pub fn touched_files_in_history(
    messages: &[Message],
    cfg: &RoutingConfig,
) -> std::collections::BTreeSet<String> {
    let mut files = std::collections::BTreeSet::new();
    for m in messages {
        for (name, input) in message_tool_uses(m) {
            if !cfg.fs_mutator_tools.iter().any(|t| t == name) {
                continue;
            }
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .or_else(|| input.get("file_path").and_then(Value::as_str));
            if let Some(p) = path {
                let p = p.trim();
                if !p.is_empty() {
                    files.insert(p.to_string());
                }
            }
        }
    }
    files
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
/// Punto unico (regola L): l'executor lo usa per individuare l'ULTIMO tool_use
/// nella coda (esito strutturato dello StallContext).
pub fn message_tool_uses(m: &Message) -> Vec<(&str, &Value)> {
    let mut out: Vec<(&str, &Value)> = Vec::new();
    if let Message::Ai {
        content,
        tool_calls,
        ..
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
        Message::Tool { content, .. } => {
            match content {
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
                    Some(blocks.iter().any(
                        |b| matches!(b, ContentBlock::Text { text } if text_has_error_hint(text)),
                    ))
                }
            }
        }
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

/// Chiave CONTRATTO del tool_result emesso dal ponte figlio->padre di
/// `mcp-core::agent_tools::subagent_native` (`K_SUB_RUN_ID`): discrimina un
/// tool_result di sub-run da qualunque altro. Duplicata come letterale (mcp-core
/// dipende da questo crate, non viceversa: non possiamo importare la sua const),
/// stabile come gli altri contratti macchina (`EXIT CODE: N`, `\u{274C}`).
const SUBAGENT_RUN_ID_KEY: &str = "subagent_run_id";

/// `true` se un payload di tool_result e' la chiusura RIUSCITA di un sub-run:
/// porta la chiave contratto [`SUBAGENT_RUN_ID_KEY`] E `status == "completed"`
/// (segnale MACCHINA, regola M — non prosa). `paused`/`timeout`/`failed` NON
/// contano. Gestisce sia il payload gia' strutturato (`Value::Object`) sia la
/// forma tipica in cui il tool ritorna una STRINGA JSON (`Value::String`).
fn is_completed_subagent_payload(v: &Value) -> bool {
    fn obj_matches(obj: &serde_json::Map<String, Value>) -> bool {
        obj.contains_key(SUBAGENT_RUN_ID_KEY)
            && obj.get("status").and_then(Value::as_str) == Some("completed")
    }
    match v {
        Value::Object(obj) => obj_matches(obj),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .as_ref()
            .and_then(Value::as_object)
            .map(obj_matches)
            .unwrap_or(false),
        _ => false,
    }
}

/// `true` se nella history c'e' il tool_result di un `dispatch_subagent(s)`
/// COMPLETATO con successo (un sub-run arrivato a fine turno, che ha percio'
/// dichiarato il proprio esito via `task_complete`).
///
/// Serve al final_gate (`completion_confirmed`, ADR 0034): quando il PADRE
/// coordinatore delega l'intero lavoro a un sub-agente — che HA dichiarato in
/// modo strutturato — e chiude il turno senza ri-dichiarare a sua volta, la
/// CHIUSURA onesta del run ESISTE gia' (quella del figlio). Il criterio non deve
/// bocciare per "nessuna dichiarazione": ne cerca UNA, non che sia del padre. La
/// verifica tecnica (build/typecheck) resta a guardia della correttezza — un
/// figlio che ha lasciato il lavoro incompleto fa fallire gli altri criteri.
///
/// Punto unico (regola L) del fatto strutturale "un sub-run e' stato delegato e
/// chiuso con successo in questo run". Legge il segnale MACCHINA (`status`), mai
/// la prosa del summary.
pub fn has_completed_subagent_dispatch(messages: &[Message]) -> bool {
    messages.iter().any(message_has_completed_subagent_result)
}

/// Vero se il messaggio porta un tool_result di sub-run completato, in una
/// qualsiasi delle forme (ToolMessage testo/blocchi, HumanMessage con blocchi
/// tool_result), come [`message_tool_result_outcome`].
fn message_has_completed_subagent_result(m: &Message) -> bool {
    let block_matches = |b: &ContentBlock| match b {
        ContentBlock::ToolResult { content, .. } => is_completed_subagent_payload(content),
        ContentBlock::Text { text } => is_completed_subagent_payload(&Value::String(text.clone())),
        ContentBlock::ToolUse { .. } => false,
    };
    match m {
        Message::Tool { content, .. } => match content {
            MessageContent::Text(s) => is_completed_subagent_payload(&Value::String(s.clone())),
            MessageContent::Blocks(blocks) => blocks.iter().any(block_matches),
        },
        Message::Human { content } => match content {
            MessageContent::Blocks(blocks) => blocks.iter().any(block_matches),
            MessageContent::Text(_) => false,
        },
        Message::Ai { .. } => false,
    }
}

/// CODICE STRUTTURATO (regola M) che il guard anti-persistenza-redazione della
/// fonte (`mcp-core::security::redaction_guard`) antepone al tool_result quando
/// RIFIUTA un input contenente un placeholder di redazione (audit
/// `redacted_placeholder_rejected`). E' un CONTRATTO MACCHINA stabile — come il
/// marker d'errore `\u{274C}` e `EXIT CODE: N` — non prosa: la fonte lo CODIFICA,
/// [`recent_redaction_rejected`] lo LEGGE. mcp-core importa QUESTA costante
/// (punto unico, regola L): un solo letterale, definito nel crate a valle che i
/// consumatori leggono, referenziato dalla fonte a monte.
pub const REDACTION_REJECTED_CODE: &str = "[REDACTION_REJECTED]";

/// Rende il testo di UN content di tool_result (stringa o blocchi) per la sola
/// ricerca del codice sentinella [`REDACTION_REJECTED_CODE`]. Gemello di
/// [`content_value_to_text`] applicato ai `ContentBlock::ToolResult`; NON
/// classifica il significato del testo (regola M: cerca un codice macchina,
/// non pattern di prosa).
fn tool_result_text_of(m: &Message) -> Option<String> {
    match m {
        Message::Tool { content, .. } => Some(match content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { content, .. } => {
                        Some(content_value_to_text(content))
                    }
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        Message::Human { content } => {
            let MessageContent::Blocks(blocks) = content else {
                return None;
            };
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { content, .. } => {
                        Some(content_value_to_text(content))
                    }
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Message::Ai { .. } => None,
    }
}

/// `true` se negli ultimi `lookback` messaggi c'e' un tool_result che porta il
/// CODICE STRUTTURATO [`REDACTION_REJECTED_CODE`] (la fonte ha rifiutato un input
/// per placeholder di redazione). E' il SEGNALE STRUTTURATO (regola M) che
/// alimenta `StallContext.redaction_rejected`: riconosce il blocco ambientale
/// (l'email/segreto ri-oscurato che il modello continua a copiare) SENZA
/// pattern-matching sulla prosa del messaggio ne' `contains("[REDACTED:")` sul
/// placeholder umano. Punto unico (regola L): l'executor delega qui, non
/// re-implementa la scansione.
pub fn recent_redaction_rejected(messages: &[Message], lookback: usize) -> bool {
    tail_messages(messages, lookback)
        .iter()
        .filter_map(tool_result_text_of)
        .any(|t| t.contains(REDACTION_REJECTED_CODE))
}

/// Cap del testo estratto per il confronto output-progresso (evita di
/// confrontare blob enormi: la testa e' sufficiente a distinguere due esiti).
const OUTPUT_COMPARE_CAP: usize = 4000;

/// Soglia di similarita' (Jaccard su righe) oltre cui due output della STESSA
/// azione sono considerati "lo stesso esito". Sotto soglia = l'esito e'
/// CAMBIATO -> la ripetizione mostra PROGRESSO (es. build che fallisce con
/// errori diversi dopo ogni correzione), non uno stallo.
///
/// Taratura CONSERVATIVA (0.75): un esito identico con 1-2 righe volatili su
/// 10 (timestamp/durate, "Done in 741ms") resta ~0.82 -> SIMILE (stallo, come
/// storico); due errori davvero diversi condividono solo il boilerplate ->
/// tipicamente < 0.75 -> DIVERSO (progresso). Nel dubbio si classifica SIMILE:
/// la feature puo' solo salvare run che progrediscono, mai nascondere uno
/// stallo piu' di quanto facesse il comportamento storico.
pub const OUTPUT_SIMILARITY_THRESHOLD: f64 = 0.75;

/// TESTO del primo tool_result dopo `idx` (gemello di
/// [`tool_result_outcome_after`], stessa finestra e STESSE FORME accettate:
/// `Message::Tool` langchain E `Message::Human` con blocchi `ToolResult`):
/// usato per il confronto output-progresso. Cap [`OUTPUT_COMPARE_CAP`].
/// `None` se nessun tool_result nella finestra.
fn tool_result_text_after(recent: &[Message], idx: usize, max_ahead: usize) -> Option<String> {
    let end = (idx + 1 + max_ahead).min(recent.len());
    for nm in recent.iter().take(end).skip(idx + 1) {
        let content = match nm {
            Message::Tool { content, .. } => content,
            Message::Human { content } if matches!(content, MessageContent::Blocks(_)) => content,
            _ => continue,
        };
        let text: String = match content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<String> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult { content, .. } => {
                            Some(content_value_to_text(content))
                        }
                        _ => None,
                    })
                    .collect();
                // Human senza alcun blocco ToolResult: non e' un tool_result
                // (es. un nudge testuale a blocchi) -> continua a cercare.
                if parts.is_empty() {
                    continue;
                }
                parts.join("\n")
            }
        };
        return Some(text.chars().take(OUTPUT_COMPARE_CAP).collect());
    }
    None
}

/// Forma testuale di un content di tool_result (stringa diretta o JSON
/// serializzato): solo per il CONFRONTO strutturale, mai per decidere sul
/// significato del testo (regola M).
fn content_value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// CONFRONTO STRUTTURALE di due output della stessa azione: Jaccard
/// sull'insieme delle righe trimmate non vuote, soglia
/// [`OUTPUT_SIMILARITY_THRESHOLD`]. E' una misura di uguaglianza fuzzy del
/// dato grezzo (le righe volatili tipo "Done in 741ms" pesano 1/N), NON una
/// classificazione semantica del contenuto (regola M rispettata: nessun
/// pattern-matching sul significato). Due output entrambi vuoti sono simili.
pub fn outputs_similar(a: &str, b: &str) -> bool {
    let lines = |s: &str| -> std::collections::HashSet<String> {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    let la = lines(a);
    let lb = lines(b);
    if la.is_empty() && lb.is_empty() {
        return true;
    }
    let inter = la.intersection(&lb).count() as f64;
    let union = la.union(&lb).count() as f64;
    union > 0.0 && (inter / union) >= OUTPUT_SIMILARITY_THRESHOLD
}

/// `true` se le ultime DUE occorrenze della signature `target_sig` nella
/// finestra recente hanno prodotto OUTPUT DIVERSI (sotto soglia di
/// similarita'): la ripetizione sta facendo PROGRESSO — es. `npm run build`
/// rilanciata dopo ogni correzione, che fallisce con errori via via diversi —
/// e NON va chiusa come loop (incidente "run_command: npm run build si
/// ripeteva senza ulteriore progresso" su un modello che stava convergendo).
/// `false` se gli output sono uguali (stallo vero) o se non ci sono almeno
/// due occorrenze con output confrontabile.
pub fn repeated_signature_output_progress(
    messages: &[Message],
    target_sig: &str,
    lookback: usize,
) -> bool {
    let recent = tail_messages(messages, lookback);
    let mut outputs: Vec<String> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            if build_signature(name, input) == target_sig {
                if let Some(text) = tool_result_text_after(recent, idx, 3) {
                    outputs.push(text);
                }
            }
        }
    }
    if outputs.len() < 2 {
        return false;
    }
    let last = &outputs[outputs.len() - 1];
    let prev = &outputs[outputs.len() - 2];
    !outputs_similar(prev, last)
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

/// Statistiche errore tool STRUTTURATE per lo SCALE-CONTROLLER (regola M: dal
/// segnale `exit_code`/`is_error` del tool_result, MAI dal parsing della prosa —
/// stesso punto unico di [`detect_recent_tool_error`] via
/// `message_tool_result_outcome`). Scansiona gli ultimi `lookback` `Message::Tool`
/// e ritorna `(error_count, error_free_streak)`:
///   - `error_count`: quanti tool_result nella finestra sono errori (esito `true`);
///   - `error_free_streak`: quanti tool_result CONSECUTIVI in coda (dall'ultimo
///     all'indietro) sono SENZA errore, fermandosi al primo errore.
/// Un tool_result con esito ignoto (`None`) NON conta come errore ma INTERROMPE la
/// streak pulita (conservativo: non affermiamo "pulito" su un esito ambiguo). Su
/// history vuota o senza tool_result ritorna `(0, 0)`.
pub fn tool_error_stats(messages: &[Message], lookback: usize) -> (i64, i64) {
    let mut error_count = 0i64;
    let mut streak = 0i64;
    let mut streak_open = true;
    let mut checked = 0usize;
    for m in messages.iter().rev() {
        if checked >= lookback {
            break;
        }
        let Message::Tool { .. } = m else {
            continue;
        };
        checked += 1;
        match message_tool_result_outcome(m) {
            Some(true) => {
                error_count += 1;
                streak_open = false;
            }
            Some(false) => {
                if streak_open {
                    streak += 1;
                }
            }
            None => {
                // Esito ambiguo: non e' un errore, ma chiude la streak pulita.
                streak_open = false;
            }
        }
    }
    (error_count, streak)
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
        let cmd = sig
            .split_once('|')
            .map(|(c, _)| c)
            .unwrap_or(&sig)
            .to_string();
        (Some(cmd), count)
    })
}

/// Tool tracciati da `detect_repeated_action` -> chiavi argomento che ne
/// definiscono il BERSAGLIO (path/comando/pattern).
///
/// Il bersaglio NON e' l'identita' dell'azione (quella e' l'INPUT COMPLETO, via
/// [`build_signature`]): qui si estrae solo il bersaglio per la label leggibile e
/// per le esclusioni basate sul file (rilettura-dopo-edit, falso-doppione).
/// PUNTO UNICO (regola L): l'estrazione del bersaglio passa tutta da qui.
///
/// Oltre ai tool PRODUTTIVI (scrittura/comando) sono inclusi i tool di SOLA
/// LETTURA con bersaglio (read_file/list_files/grep & co.): la ripetizione
/// IDENTICA di una lettura (stesso path/pattern) e' un loop di esplorazione che
/// non converge (NON-convergenza, regola H) e va fermato dal progress_controller
/// ben prima del cap esplorazione 2x. Per questi tool la ripetizione conta a
/// prescindere dall'esito (vedi [`is_read_only_repeatable_tool`]): rileggere con
/// SUCCESSO lo stesso file e' proprio lo stallo da interrompere.
fn repeated_action_keys(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "write_file" | "edit_file" => Some(&["path", "file_path"]),
        "run_command" | "run_service" | "run_in_terminal" => Some(&["command"]),
        // Tool di sola lettura con bersaglio: bersaglio = path o pattern.
        "read_file" | "read_file_lines" | "list_files" => Some(&["path", "file_path", "dir"]),
        "grep" | "search_in_files" => Some(&["pattern", "query", "path"]),
        _ => None,
    }
}

/// True se `name` e' un tool di SOLA LETTURA per cui la ripetizione identica conta
/// come stallo a PRESCINDERE dall'esito (a differenza dei tool produttivi, dove la
/// PRIMA occorrenza riuscita esclude la signature come "ridondanza innocua"). Per i
/// read-only la rilettura riuscita ripetuta E' lo stallo (l'agente non avanza):
/// quindi NON va esclusa dal conteggio. Punto unico (regola L) della distinzione.
fn is_read_only_repeatable_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "read_file_lines" | "list_files" | "grep" | "search_in_files"
    )
}

/// Esito ricco di [`detect_repeated_action_detailed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedActionHit {
    /// Label leggibile per nudge/recap: `name: bersaglio` (bersaglio troncato a
    /// 120 char). NON contiene il discriminante di contenuto (resta umano).
    pub label: String,
    /// Conteggio della signature vincente nella finestra recente.
    pub count: i64,
    /// Nome del tool ripetuto (`edit_file`, `write_file`, `run_command`, ...).
    pub tool_name: String,
    /// `true` se l'ULTIMA occorrenza della signature vincente e' FALLITA
    /// (tool_result con errore). Discrimina "edit_file fallito da correggere"
    /// dalle altre ripetizioni: alimenta il nudge specifico del controller.
    pub failed: bool,
}

/// Rileva la ripetizione IDENTICA di un'azione produttiva (scrittura/comando),
/// a prescindere dall'esito. Versione RICCA: ritorna [`RepeatedActionHit`] con
/// label, conteggio, nome tool ed esito dell'ultima occorrenza.
///
/// IDENTITA' dell'azione (regola L, punto unico UNIVERSALE): [`build_signature`]
/// `(name, input)` = name + hash dell'INPUT COMPLETO. Lo stesso punto del detector
/// dell'engine ([`crate::decisions::loop_signatures`]): vale per OGNI tool e OGNI
/// argomento, senza whitelist da mantenere. Cosi' due chiamate dello stesso tool
/// che differiscono in QUALSIASI argomento (l'`old_string` di un edit, il range di
/// un read_file, il path di un grep) sono azioni DISTINTE (count 1 ciascuna): solo
/// la chiamata DAVVERO identica fa count>=2. Il bersaglio ([`repeated_action_keys`])
/// serve solo per label/esclusioni, non per l'identita'.
///
/// FALSO-DOPPIONE: le signature la cui PRIMA occorrenza e' RIUSCITA
/// (`tool_result_outcome_after == Some(false)`) sono ESCLUSE dal conteggio
/// (ridondanza innocua, non stallo). Lookback canonico 24.
pub fn detect_repeated_action_detailed(
    messages: &[Message],
    lookback: usize,
) -> Option<RepeatedActionHit> {
    if messages.is_empty() {
        return None;
    }
    let recent = tail_messages(messages, lookback);
    let mut counts: Vec<(String, i64)> = Vec::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    // sig -> nome tool (per il ramo edit-fallito del controller).
    let mut tool_names: Vec<(String, String)> = Vec::new();
    // sig -> esito dell'ULTIMA occorrenza (true = fallita).
    let mut last_failed: Vec<(String, bool)> = Vec::new();
    let mut succeeded: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Target (file) con un edit_file/write_file RIUSCITO visto finora nella finestra:
    // una rilettura read-only di uno di questi DOPO la modifica e' VERIFICA del
    // risultato, NON uno stallo (regola H). Senza questa esclusione, il pattern sano
    // "leggi -> modifica -> rileggi per verificare" faceva scattare repeated_action a
    // soglia 2 e ABORTIVA un task GIA' risolto, con recap falso "File toccati: nessuno"
    // (incidente vite.config.ts: edit applicato, poi rilettura -> falso loop -> abort).
    let mut modified_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
    // RILETTURA-DOPO-PROGRESSO (regola H, generalizza l'esclusione rilettura-dopo-edit):
    // una ripetizione di tool READ-ONLY NON e' uno stallo se tra l'occorrenza PRECEDENTE
    // della STESSA signature e quella corrente c'e' stata almeno UN'AZIONE PRODUTTIVA (un
    // tool NON read-only: write/edit/run_command/run_service/nexus_db_query/...). E' il
    // pattern del DEBUGGING attivo: rileggi un file per VERIFICARE dopo aver agito, non a
    // vuoto. Senza questa esclusione, due riletture sparse intervallate da ~12 azioni
    // produttive scattavano repeated_action a soglia 2 e ABORTIVANO un agente che stava
    // CONVERGENDO (incidente deepseek-v4-pro: HTTP 500 backend, utente gia' creato, due
    // read_file di index.js a step 18 e 24 -> falso loop -> abort). Solo le read-only
    // ripetute SENZA alcuna azione produttiva in mezzo (rilettura davvero a vuoto) restano
    // stallo. `last_productive_idx` = indice del messaggio dell'ULTIMA azione produttiva
    // vista finora; `read_first_idx` = per ogni signature read-only, l'indice della sua
    // PRIMA occorrenza non ancora "scontata" dal progresso.
    let mut last_productive_idx: Option<usize> = None;
    let mut read_first_idx: Vec<(String, usize)> = Vec::new();
    let mut last_sig: Option<String> = None;
    // sig -> testo dell'ULTIMO tool_result visto (per il confronto
    // output-progresso delle azioni produttive fallite ripetute).
    let mut last_outputs: Vec<(String, String)> = Vec::new();
    for (idx, m) in recent.iter().enumerate() {
        for (name, input) in message_tool_uses(m) {
            let Some(keys) = repeated_action_keys(name) else {
                continue;
            };
            // bersaglio = primo argomento non vuoto fra le chiavi candidate.
            let mut target = String::new();
            for k in keys {
                if let Some(v) = input.get(*k).and_then(Value::as_str) {
                    let v = v.trim();
                    if !v.is_empty() {
                        target = v.to_string();
                        break;
                    }
                }
            }
            if target.is_empty() {
                continue;
            }
            // Esito strutturale dell'occorrenza corrente (primo tool_result dopo).
            let outcome = tool_result_outcome_after(recent, idx, 3);
            // Rilettura-di-verifica: un tool di SOLA LETTURA su un file gia' modificato
            // (edit/write riuscito PRIMA, cronologicamente, nella finestra) non e' una
            // ripetizione-stallo ma la verifica della modifica -> NON conta.
            if is_read_only_repeatable_tool(name) && modified_targets.contains(&target) {
                continue;
            }
            // IDENTITA' UNIVERSALE (regola L): la firma e' l'UNICA definizione di
            // "stessa azione", data da build_signature(name, input) = name + hash
            // dell'INPUT COMPLETO (ordine chiavi irrilevante). E' lo STESSO punto
            // usato dal detector dell'engine (loop_signatures): nessun tool puo'
            // sfuggire, perche' OGNI argomento entra per costruzione (il range di
            // read_file, l'old_string di edit_file, il pattern+path di grep, ...).
            // Niente whitelist di chiavi da mantenere a mano -> niente piu' falsi
            // loop quando si aggiunge un tool/argomento. Il `target` qui sopra resta
            // solo per la label leggibile e per le esclusioni sul bersaglio
            // (rilettura-dopo-edit, falso-doppione).
            let sig = build_signature(name, input);
            // ESCLUSIONE rilettura-dopo-progresso (solo tool READ-ONLY). Per i tool
            // PRODUTTIVI il comportamento resta invariato: aggiornano l'indice di
            // progresso e contano sempre. Per i read-only: se la signature e' gia'
            // comparsa e DOPO la sua prima occorrenza c'e' stata un'azione produttiva
            // (last_productive_idx > prima_occorrenza), questa rilettura e' VERIFICA,
            // non stallo -> non incrementa il conteggio; aggiorna la "prima occorrenza"
            // a quella corrente cosi' una eventuale terza rilettura va misurata di
            // nuovo rispetto al progresso piu' recente.
            if is_read_only_repeatable_tool(name) {
                if let Some((_, first_idx)) = read_first_idx.iter_mut().find(|(s, _)| *s == sig) {
                    if last_productive_idx.is_some_and(|p| p > *first_idx) {
                        *first_idx = idx;
                        continue;
                    }
                } else {
                    read_first_idx.push((sig.clone(), idx));
                }
            } else {
                // Azione produttiva ESEGUITA: segna il progresso che "scusa" le
                // successive riletture read-only delle signature gia' viste.
                last_productive_idx = Some(idx);
            }
            // OUTPUT-PROGRESSO (regola M/H): per un'azione PRODUTTIVA FALLITA
            // ripetuta, se l'esito TESTUALE dell'occorrenza corrente differisce
            // da quello della precedente (confronto STRUTTURALE, outputs_similar:
            // mai semantica del testo), l'azione sta PROGREDENDO — es. `npm run
            // build` rilanciata dopo ogni correzione che fallisce con errori via
            // via diversi — e il conteggio RIPARTE. Solo la ripetizione con lo
            // STESSO esito e' uno stallo (incidente "run_command: npm run build
            // si ripeteva senza ulteriore progresso" su un run che convergeva).
            if !is_read_only_repeatable_tool(name) && outcome == Some(true) {
                let cur_out = tool_result_text_after(recent, idx, 3).unwrap_or_default();
                if let Some((_, prev_out)) = last_outputs.iter_mut().find(|(s, _)| *s == sig) {
                    if !outputs_similar(prev_out, &cur_out) {
                        if let Some((_, c)) = counts.iter_mut().find(|(s, _)| *s == sig) {
                            *c = 0;
                        }
                    }
                    *prev_out = cur_out;
                } else {
                    last_outputs.push((sig.clone(), cur_out));
                }
            }
            bump(&mut counts, &sig);
            let label_value: String = target.chars().take(120).collect();
            set_label(&mut labels, &sig, format!("{name}: {label_value}"));
            set_label(&mut tool_names, &sig, name.to_string());
            last_sig = Some(sig.clone());
            // Un edit/write RIUSCITO (outcome == Some(false)) segna il target come
            // modificato: le successive riletture read-only dello stesso file sono
            // verifica e vengono escluse dal conteggio (sopra).
            if outcome == Some(false) && matches!(name, "edit_file" | "write_file") {
                modified_targets.insert(target.clone());
            }
            // FALSO-DOPPIONE (solo tool PRODUTTIVI): la prima occorrenza RIUSCITA
            // esclude la signature (ridondanza innocua). Per i tool di SOLA LETTURA
            // la rilettura RIUSCITA ripetuta E' lo stallo (l'agente non avanza),
            // quindi NON va esclusa: conta come ripetizione (regola H).
            if outcome == Some(false) && !is_read_only_repeatable_tool(name) {
                succeeded.insert(sig.clone());
            }
            // Memorizza l'esito dell'ULTIMA occorrenza vista (None -> non fallita).
            set_failed(&mut last_failed, &sig, outcome == Some(true));
        }
    }
    // Rimuove le signature riuscite (mai stallo da abort).
    counts.retain(|(sig, _)| !succeeded.contains(sig));
    let (sig, count) = pick_top(&counts, last_sig.as_deref())?;
    let label = labels
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, l)| l.clone())
        .unwrap_or_else(|| sig.clone());
    let tool_name = tool_names
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, n)| n.clone())
        .unwrap_or_default();
    let failed = last_failed
        .iter()
        .find(|(s, _)| *s == sig)
        .map(|(_, f)| *f)
        .unwrap_or(false);
    Some(RepeatedActionHit {
        label,
        count,
        tool_name,
        failed,
    })
}

/// Variante COMPATTA storica: `(Some(label), count)` o `(None, 0)`. Delega al
/// punto unico [`detect_repeated_action_detailed`] (regola L). Conservata per i
/// call site/test che non hanno bisogno di nome tool ed esito.
pub fn detect_repeated_action(messages: &[Message], lookback: usize) -> (Option<String>, i64) {
    match detect_repeated_action_detailed(messages, lookback) {
        Some(hit) => (Some(hit.label), hit.count),
        None => (None, 0),
    }
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

/// Imposta/aggiorna l'esito (fallita?) dell'ULTIMA occorrenza di una signature.
/// Sovrascrive sempre: alla fine resta l'esito della chiamata piu' recente.
fn set_failed(list: &mut Vec<(String, bool)>, sig: &str, failed: bool) {
    if let Some(entry) = list.iter_mut().find(|(s, _)| s == sig) {
        entry.1 = failed;
    } else {
        list.push((sig.to_string(), failed));
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
    detect_pending_steps_report_with(
        text,
        cfg.pending_steps_detection_enabled,
        cfg.pending_steps_min_items,
    )
}

/// PUNTO UNICO parametrico (regola L) del rilevamento "report con passi
/// pendenti": la variante con `RoutingConfig` delega qui; l'executor (che ha
/// una config propria, `ExecutorConfig`) chiama direttamente questa firma con
/// le STESSE chiavi DB `agent.closure.pending_steps_*` (ADR 0018 fase 3: e' il
/// sostituto strutturale del vecchio fallback lessicale rimosso).
pub fn detect_pending_steps_report_with(text: Option<&str>, enabled: bool, min_items: i64) -> bool {
    let Some(text) = text else {
        return false;
    };
    if text.trim().is_empty() {
        return false;
    }
    if !enabled {
        return false;
    }
    let min_items = min_items.max(1) as usize;

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

/// Segnale SEMANTICO "esito non compiuto", SOLO strutturale (ADR 0018 fase 3:
/// il fallback lessicale `detect_unfulfilled_intent` e' stato RIMOSSO — le
/// leve 0/1/2 + task_complete ADR 0034 coprono i casi che intercettava).
/// Ordine:
///   1. verdetto closure_judge (bool) -> `not fulfilled`;
///   2. segnale strutturale `detect_pending_steps_report(result)`.
pub fn unfulfilled_signal(state: &AgentState, cfg: &RoutingConfig) -> bool {
    if let Some(fulfilled) = closure_verdict_fulfilled(state) {
        return !fulfilled;
    }
    detect_pending_steps_report(state.result.as_deref(), cfg)
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
    // (1-bis) DELEGA a subagente = lavoro software per PROCURA (incidente run
    // 1daf83b3): il padre non tocca file in prima persona ma il figlio puo'
    // averlo fatto, e il suo summary puo' ALLUCINARE il completamento. Senza
    // questo segnale il final_gate veniva SALTATO in silenzio (pass-through su
    // is_software_task=false) e un run chiudeva 'completed' sulla parola del
    // subagente, senza alcuna verifica oggettiva. Prefix-match STRUTTURALE sul
    // nome del tool di delega (dispatch_subagent / dispatch_subagents), mai
    // sul testo (regola M).
    if ai_tool_use_names(&state.messages)
        .into_iter()
        .any(|name| name.starts_with("dispatch_subagent"))
    {
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
                thought_signature: None,
            }],
            reasoning: None,
            thinking_signature: None,
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
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
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
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
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
    fn completed_subagent_dispatch_da_history() {
        // Payload REALE del ponte figlio->padre (finalize_success): il tool_result
        // e' una STRINGA JSON con `subagent_run_id` + `status`.
        let ok = r#"{"subagent_run_id":"11111111-1111-1111-1111-111111111111","kind":"rust_implementer","status":"completed","summary":"riscritto AppointmentsTab","iterations":56,"cost_usd":0.1}"#;
        // Forma HumanMessage con blocco tool_result (content = stringa JSON).
        assert!(has_completed_subagent_dispatch(&[human_tool_result(
            None, false, ok
        )]));
        // Forma ToolMessage langchain con content testuale.
        assert!(has_completed_subagent_dispatch(&[tool_msg(ok)]));
        // Solo `status == "completed"` conta: paused/failed/timeout/running NO.
        for st in ["paused", "failed", "timeout", "running"] {
            let other = format!(r#"{{"subagent_run_id":"abc","status":"{st}","summary":"x"}}"#);
            assert!(
                !has_completed_subagent_dispatch(&[tool_msg(&other)]),
                "status {st} non deve contare come completamento"
            );
        }
        // Un tool_result NON-subagente (nessun subagent_run_id) non conta.
        assert!(!has_completed_subagent_dispatch(&[tool_msg(
            r#"{"status":"completed","output":"build ok"}"#
        )]));
        // Payload gia' strutturato (Value::Object) invece di stringa JSON.
        let structured = Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: json!({"subagent_run_id": "abc", "status": "completed"}),
                is_error: false,
                exit_code: None,
            }]),
        };
        assert!(has_completed_subagent_dispatch(&[structured]));
        // History senza sub-run -> false.
        assert!(!has_completed_subagent_dispatch(&[tool_msg(
            "nessun subrun"
        )]));
    }

    #[test]
    fn outputs_similar_confronto_strutturale() {
        // Stesso errore con la sola riga di durata diversa -> SIMILI.
        let a = "vite build\nerror TS2345 in bookingService.ts:42\nfailed\nDone in 741ms\nl1\nl2\nl3\nl4\nl5\nl6";
        let b = "vite build\nerror TS2345 in bookingService.ts:42\nfailed\nDone in 488ms\nl1\nl2\nl3\nl4\nl5\nl6";
        assert!(outputs_similar(a, b));
        // Errori DIVERSI -> non simili (progresso).
        let c = "vite build\nerror TS2551 in LoginPage.tsx:10\nfailed\nDone in 500ms";
        assert!(!outputs_similar(a, c));
        // Entrambi vuoti -> simili.
        assert!(outputs_similar("", ""));
    }

    #[test]
    fn build_ripetuta_con_errori_diversi_non_e_stallo() {
        // REGRESSIONE "run_command: npm run build si ripeteva senza ulteriore
        // progresso": la STESSA firma (input identico) fallita 3 volte ma con
        // OUTPUT DIVERSI (un errore corretto per volta) e' PROGRESSO -> il
        // conteggio riparte a ogni esito nuovo e il detector NON scatta.
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        let err = |t: &str| human_tool_result(Some(1), true, t);
        let msgs = vec![
            build(),
            err("error TS1 in a.ts:1\nriga2\nriga3\nriga4"),
            build(),
            err("error TS2 in b.ts:9\naltra2\naltra3\naltra4"),
            build(),
            err("error TS3 in c.ts:5\nancora2\nancora3\nancora4"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24);
        assert!(
            hit.as_ref().map(|h| h.count).unwrap_or(0) < 2,
            "build con errori diversi non deve contare come ripetizione: {hit:?}"
        );
    }

    #[test]
    fn build_ripetuta_con_stesso_errore_e_stallo() {
        // Contro-prova: 3 build fallite con lo STESSO output -> stallo vero.
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        let stesso = "error TS1 in a.ts:1\nriga2\nriga3\nriga4";
        let msgs = vec![
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert!(hit.count >= 3);
        assert!(hit.failed);
    }

    #[test]
    fn signature_output_progress_true_su_esiti_diversi() {
        let sig = build_signature("run_command", &json!({"command": "npm run build"}));
        let build = || ai_tool_input("run_command", json!({"command": "npm run build"}));
        // Esiti diversi -> progresso.
        let msgs = vec![
            build(),
            human_tool_result(Some(1), true, "error TS1 in a.ts\nx\ny\nz"),
            build(),
            human_tool_result(Some(1), true, "error TS2 in b.ts\nk\nw\nq"),
        ];
        assert!(repeated_signature_output_progress(&msgs, &sig, 24));
        // Esiti uguali -> nessun progresso.
        let stesso = "error TS1 in a.ts\nx\ny\nz";
        let msgs2 = vec![
            build(),
            human_tool_result(Some(1), true, stesso),
            build(),
            human_tool_result(Some(1), true, stesso),
        ];
        assert!(!repeated_signature_output_progress(&msgs2, &sig, 24));
        // Una sola occorrenza -> nessun progresso dichiarabile.
        let msgs3 = vec![build(), human_tool_result(Some(1), true, stesso)];
        assert!(!repeated_signature_output_progress(&msgs3, &sig, 24));
    }

    #[test]
    fn has_tool_calls_history() {
        assert!(has_tool_calls_in_history(&[ai_with_tool("read_file")]));
        assert!(!has_tool_calls_in_history(&[Message::Ai {
            content: MessageContent::text("solo testo"),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
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
    fn redaction_rejected_da_codice_strutturato() {
        // SEGNALE STRUTTURATO (regola M): un tool_result che porta il codice
        // sentinella [REDACTION_REJECTED] -> true; il testo umano del placeholder
        // ([REDACTED:...]) da solo NON basta (non e' il codice macchina).
        let rifiutato = vec![
            ai_tool_input("run_command", json!({"command": "x"})),
            human_tool_result(
                None,
                true,
                "\u{274C} [REDACTION_REJECTED] [BLOCCATO — placeholder di redazione nell'input]",
            ),
        ];
        assert!(recent_redaction_rejected(&rifiutato, 16));
        // Un tool_result che MENZIONA il placeholder umano ma NON porta il codice
        // strutturato non conta (evita falsi positivi da prosa/log).
        let solo_prosa = vec![
            ai_tool_input("read_file", json!({"path": ".env"})),
            human_tool_result(Some(0), false, "ADMIN_EMAIL=[REDACTED:email_pii]"),
        ];
        assert!(!recent_redaction_rejected(&solo_prosa, 16));
        // Nessun tool_result -> false.
        assert!(!recent_redaction_rejected(&[], 16));
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
    fn tool_error_stats_conta_errori_e_streak() {
        // Nessun tool_result -> (0, 0).
        assert_eq!(tool_error_stats(&[], 40), (0, 0));
        // Tre ok consecutivi in coda -> error_count 0, streak 3.
        let all_ok = vec![
            tool_msg("done ok"),
            tool_msg("done ok"),
            tool_msg("done ok"),
        ];
        assert_eq!(tool_error_stats(&all_ok, 40), (0, 3));
        // Un errore in coda -> error_count 1, streak 0 (l'ultimo e' errore).
        let last_err = vec![tool_msg("done ok"), tool_msg("Error: build failed")];
        assert_eq!(tool_error_stats(&last_err, 40), (1, 0));
        // Errore in mezzo, poi due ok in coda -> error_count 1, streak 2 (la streak
        // parte dall'ultimo all'indietro e si ferma al primo errore).
        let mixed = vec![
            tool_msg("Error: fallito"),
            tool_msg("done ok"),
            tool_msg("done ok"),
        ];
        assert_eq!(tool_error_stats(&mixed, 40), (1, 2));
    }

    #[test]
    fn repeated_failed_command_stessa_signature() {
        // Stesso comando fallito 2 volte -> rilevato. _detect_repeated_failed_command
        // valuta SOLO i ToolMessage successivi (1:1 col Python, che guarda
        // isinstance(nm, ToolMessage)), quindi qui usiamo tool_msg.
        let msgs = vec![
            ai_tool_input(
                "run_command",
                json!({"command": "npm i", "working_dir": "/p"}),
            ),
            tool_msg("error: build failed"),
            ai_tool_input(
                "run_command",
                json!({"command": "npm i", "working_dir": "/p"}),
            ),
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
    fn repeated_action_edit_old_string_diverso_non_stallo() {
        // Due edit_file sullo STESSO path ma con old_string DIVERSI: il secondo
        // e' la CORREZIONE del primo, non una ripetizione a vuoto. Con la
        // signature sensibile al contenuto sono azioni DISTINTE -> count 1
        // ciascuna -> nessuno stallo (soglia 2 non raggiunta da nessuna).
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn beta() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24);
        // La signature vincente ha count 1: sotto la soglia 2, l'executor non
        // considera stallo. Verifichiamo che nessuna signature arrivi a 2.
        assert_eq!(hit.as_ref().map(|h| h.count), Some(1));
    }

    #[test]
    fn repeated_action_edit_old_string_identico_stallo() {
        // Due edit_file IDENTICI (stesso path + stesso old_string) entrambi
        // falliti: e' una ripetizione a vuoto reale -> count 2 -> stallo.
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo atteso");
        assert_eq!(hit.label, "edit_file: src/lib.rs");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "edit_file");
        assert!(hit.failed, "l'ultima occorrenza e' fallita");
    }

    #[test]
    fn repeated_action_edit_identico_riuscito_non_stallo() {
        // Stesso path + stesso old_string ma la PRIMA occorrenza RIESCE:
        // ridondanza innocua (falso-doppione), nessuno stallo.
        let msgs = vec![
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input(
                "edit_file",
                json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
            ),
            human_tool_result(None, true, "old_string non trovato"),
        ];
        assert_eq!(detect_repeated_action(&msgs, 24), (None, 0));
    }

    #[test]
    fn repeated_action_read_only_riuscito_e_stallo() {
        // FIX #2 (NON-convergenza, regola H): una LETTURA ripetuta IDENTICA, anche
        // RIUSCITA entrambe le volte, e' stallo per i read-only (l'agente rilegge
        // lo stesso file senza avanzare). A differenza dei produttivi, la prima
        // occorrenza riuscita NON la esclude dal conteggio.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "src/main.rs"})),
            human_tool_result(Some(0), false, "fn main() {}"),
            ai_tool_input("read_file", json!({"path": "src/main.rs"})),
            human_tool_result(Some(0), false, "fn main() {}"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo read-only atteso");
        assert_eq!(hit.label, "read_file: src/main.rs");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
        // Letture su path DIVERSI: esplorazione legittima, nessuno stallo.
        let diversi = vec![
            ai_tool_input("read_file", json!({"path": "a.rs"})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input("read_file", json!({"path": "b.rs"})),
            human_tool_result(Some(0), false, "..."),
        ];
        let hit2 = detect_repeated_action_detailed(&diversi, 24);
        assert_eq!(
            hit2.map(|h| h.count),
            Some(1),
            "path diversi -> nessuno stallo"
        );
    }

    #[test]
    fn repeated_action_read_dopo_edit_e_verifica_non_stallo() {
        // FIX (regola H, incidente vite.config.ts): leggi -> MODIFICA -> rileggi per
        // verificare e' un pattern SANO, non uno stallo. La rilettura read-only DOPO
        // un edit RIUSCITO sullo stesso file NON deve contare come repeated_action
        // (prima faceva scattare l'ABORT a soglia 2 su un task GIA' risolto, con recap
        // falso "File toccati: nessuno").
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
            ai_tool_input(
                "edit_file",
                json!({"path": "vite.config.ts", "old_string": "35198"}),
            ),
            human_tool_result(Some(0), false, "applied"),
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: process.env.PORT"),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "read-dopo-edit e' verifica, non stallo; count={count}"
        );
        // Controprova: due read IDENTICHE senza edit in mezzo restano stallo.
        let loop_msgs = vec![
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
            ai_tool_input("read_file", json!({"path": "vite.config.ts"})),
            human_tool_result(Some(0), false, "port: 35198"),
        ];
        assert_eq!(
            detect_repeated_action_detailed(&loop_msgs, 24).map(|h| h.count),
            Some(2),
            "due read senza edit in mezzo restano stallo"
        );
    }

    #[test]
    fn repeated_action_read_consecutive_senza_progresso_resta_stallo() {
        // CASO 1 (anti-regressione): due read_file IDENTICHE CONSECUTIVE, senza alcuna
        // azione in mezzo, restano stallo reale (count 2). L'esclusione rilettura-dopo-
        // progresso NON deve indebolire l'anti-loop quando l'agente rilegge a vuoto.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24)
            .expect("due read consecutive senza progresso restano stallo");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
    }

    #[test]
    fn repeated_action_read_dopo_azione_produttiva_non_stallo() {
        // CASO 2 (fix): read_file A -> run_command (produttiva) -> read_file A identica.
        // La produttiva in mezzo "scusa" la rilettura (verifica/debugging), quindi la
        // signature read-only resta a count 1 -> sotto la soglia 2, nessuno stallo.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
            ai_tool_input(
                "run_command",
                json!({"command": "curl -s localhost:3000/health"}),
            ),
            human_tool_result(Some(0), false, "500"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "app.listen(...)"),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "read-dopo-azione-produttiva e' verifica, non stallo; count={count}"
        );
    }

    #[test]
    fn repeated_action_caso_reale_debugging_500_non_stallo() {
        // CASO 3 (incidente deepseek-v4-pro ridotto): durante il debug di un HTTP 500
        // l'agente legge il backend, esegue azioni produttive di diagnosi (curl, psql),
        // poi RILEGGE lo stesso file per verificare. Due read_file identiche intervallate
        // da azioni produttive NON sono uno stallo: l'agente sta CONVERGENDO.
        let msgs = vec![
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "run_command",
                json!({"command": "curl -s localhost:3000/users"}),
            ),
            human_tool_result(Some(0), false, "500 Internal Server Error"),
            ai_tool_input("run_command", json!({"command": "psql -c 'select 1'"})),
            human_tool_result(Some(0), false, "1 row"),
            ai_tool_input("read_file", json!({"path": "backend/index.js"})),
            human_tool_result(Some(0), false, "..."),
        ];
        let count = detect_repeated_action_detailed(&msgs, 24)
            .map(|h| h.count)
            .unwrap_or(0);
        assert!(
            count < 2,
            "rilettura dopo azioni di diagnosi e' debugging, non stallo; count={count}"
        );
    }

    #[test]
    fn repeated_action_run_command_falliti_resta_stallo() {
        // CASO 4 (anti-regressione tool produttivi): due run_command IDENTICI falliti
        // restano stallo (count 2). I tool produttivi NON sono toccati dall'esclusione
        // rilettura-dopo-progresso: contano sempre.
        let msgs = vec![
            ai_tool_input("run_command", json!({"command": "npm run build"})),
            human_tool_result(None, true, "build failed"),
            ai_tool_input("run_command", json!({"command": "npm run build"})),
            human_tool_result(None, true, "build failed"),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24)
            .expect("due run_command identici falliti restano stallo");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "run_command");
        assert!(hit.failed, "l'ultima occorrenza e' fallita");
    }

    #[test]
    fn repeated_action_grep_identico_e_stallo() {
        // grep stesso pattern ripetuto -> stallo (bersaglio = pattern).
        let msgs = vec![
            ai_tool_input("grep", json!({"pattern": "TODO", "path": "src"})),
            human_tool_result(Some(0), false, "match..."),
            ai_tool_input("grep", json!({"pattern": "TODO", "path": "src"})),
            human_tool_result(Some(0), false, "match..."),
        ];
        let hit = detect_repeated_action_detailed(&msgs, 24).expect("stallo grep atteso");
        assert_eq!(hit.tool_name, "grep");
        assert_eq!(hit.count, 2);
    }

    #[test]
    fn repeated_action_read_file_porzioni_diverse_non_stallo() {
        // Causa radice del falso-stallo "crea utente" (regola H): leggere PORZIONI
        // diverse dello stesso file (zoom progressivo limit:50 -> 30-330 -> 314-320)
        // e' esplorazione LEGITTIMA, non un loop. Con la signature sensibile al range
        // le tre letture sono azioni DISTINTE (count 1 ciascuna), sotto la soglia 2.
        let progressivo = vec![
            ai_tool_input("read_file", json!({"path": "src/big.ts", "limit": 50})),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 30, "end_line": 330}),
            ),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
        ];
        assert_eq!(
            detect_repeated_action_detailed(&progressivo, 24).map(|h| h.count),
            Some(1),
            "porzioni diverse dello stesso file -> nessuno stallo"
        );
        // Controprova: la STESSA porzione (range identico) ripetuta resta stallo reale.
        let identico = vec![
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
            ai_tool_input(
                "read_file",
                json!({"path": "src/big.ts", "start_line": 314, "end_line": 320}),
            ),
            human_tool_result(Some(0), false, "..."),
        ];
        let hit = detect_repeated_action_detailed(&identico, 24)
            .expect("stallo atteso su range identico");
        assert_eq!(hit.count, 2);
        assert_eq!(hit.tool_name, "read_file");
    }

    #[test]
    fn repeated_action_identita_universale_per_ogni_tool() {
        // CONTROLLO UNIVERSALE (regola L): per OGNI tool tracciato, due chiamate che
        // differiscono anche in UN SOLO argomento sono azioni DISTINTE (no falso
        // loop), mentre due chiamate IDENTICHE sono un loop. L'identita' deriva
        // dall'input COMPLETO (build_signature), quindi la proprieta' vale per
        // qualunque tool e argomento SENZA whitelist da mantenere. Questo test e' il
        // guard contro la reintroduzione di firme parziali (causa storica dei falsi
        // loop su read_file/range ed edit_file/old_string). Esito fallito ovunque
        // per neutralizzare l'esclusione "falso-doppione" dei tool produttivi.
        let cases: &[(&str, Value, Value)] = &[
            (
                "read_file",
                json!({"path": "a.ts"}),
                json!({"path": "a.ts", "start_line": 50}),
            ),
            (
                "read_file_lines",
                json!({"path": "a.ts", "start_line": 1, "end_line": 50}),
                json!({"path": "a.ts", "start_line": 51, "end_line": 100}),
            ),
            (
                "list_files",
                json!({"dir": "src"}),
                json!({"dir": "src/app"}),
            ),
            (
                "grep",
                json!({"pattern": "TODO", "path": "src"}),
                json!({"pattern": "TODO", "path": "lib"}),
            ),
            (
                "search_in_files",
                json!({"query": "auth", "path": "a"}),
                json!({"query": "auth", "path": "b"}),
            ),
            (
                "edit_file",
                json!({"path": "a.ts", "old_string": "x"}),
                json!({"path": "a.ts", "old_string": "y"}),
            ),
            (
                "write_file",
                json!({"path": "a.ts", "content": "x"}),
                json!({"path": "a.ts", "content": "y"}),
            ),
            (
                "run_command",
                json!({"command": "ls"}),
                json!({"command": "pwd"}),
            ),
        ];
        for (tool, base, variato) in cases {
            // Un argomento diverso -> azioni distinte -> nessuna signature a soglia 2.
            let diversi = vec![
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
                ai_tool_input(tool, variato.clone()),
                human_tool_result(None, true, "..."),
            ];
            let count_diversi = detect_repeated_action_detailed(&diversi, 24)
                .map(|h| h.count)
                .unwrap_or(0);
            assert!(
                count_diversi < 2,
                "{tool}: input diverso NON deve contare come loop (count {count_diversi})"
            );
            // Input IDENTICO ripetuto -> loop reale (count 2).
            let identici = vec![
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
                ai_tool_input(tool, base.clone()),
                human_tool_result(None, true, "..."),
            ];
            let hit = detect_repeated_action_detailed(&identici, 24)
                .unwrap_or_else(|| panic!("{tool}: input identico DEVE essere loop"));
            assert_eq!(hit.count, 2, "{tool}: input identico -> count 2");
        }
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
    fn pending_steps_report_min_items() {
        let cfg = RoutingConfig::default();
        let report =
            "Stato attuale: ok.\nProssimi passi necessari:\n1. Verificare X\n2. Eseguire Y";
        assert!(detect_pending_steps_report(Some(report), &cfg));
        // Un solo item < min_items(2).
        let uno = "Prossimi passi:\n1. Solo questo";
        assert!(!detect_pending_steps_report(Some(uno), &cfg));
    }

    #[test]
    fn pending_steps_report_with_parametrico() {
        // Punto unico parametrico (ADR 0018 fase 3): stessa semantica della
        // variante con RoutingConfig, per i call site con ExecutorConfig.
        let report = "Prossimi passi:\n1. Verificare X\n2. Eseguire Y";
        assert!(detect_pending_steps_report_with(Some(report), true, 2));
        assert!(!detect_pending_steps_report_with(Some(report), false, 2));
        assert!(!detect_pending_steps_report_with(Some(report), true, 3));
        assert!(!detect_pending_steps_report_with(None, true, 1));
        // La narrazione futura SENZA elenco puntato non e' piu' un segnale
        // (blacklist lessicale rimossa): copre task_complete + iteration cap.
        assert!(!detect_pending_steps_report_with(
            Some("Ottimo. Adesso creerò il file."),
            true,
            2
        ));
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
    fn software_task_da_delega_subagente() {
        // REGRESSIONE run 1daf83b3: il padre delega tutto a un subagente (zero
        // mutazioni proprie, intent fuori whitelist) -> il final_gate veniva
        // SALTATO e la dichiarazione allucinata del figlio chiudeva 'completed'
        // senza verifica. La delega e' lavoro software per procura: gate attivo.
        let cfg = RoutingConfig::default();
        let mut state = AgentState {
            user_intent: Some("architecture".into()), // fuori whitelist
            ..Default::default()
        };
        state.messages = vec![Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![ToolUse {
                id: "c1".to_string(),
                name: "dispatch_subagent".to_string(),
                input: json!({"task": "correggi gli errori"}),
                thought_signature: None,
            }],
            reasoning: None,
            thinking_signature: None,
        }];
        assert!(is_software_task(&state, &cfg));
        // Anche la variante plurale (fan-out) attiva il gate.
        if let Some(Message::Ai { tool_calls, .. }) = state.messages.first_mut() {
            tool_calls[0].name = "dispatch_subagents".to_string();
        }
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
                        input: if input.is_null() {
                            json!({})
                        } else {
                            input.clone()
                        },
                        thought_signature: None,
                    }]),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
                },
                RawMsg::AiText { text } => Message::Ai {
                    content: MessageContent::text(text.clone()),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
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
        assert!(
            cases.len() >= 20,
            "attesi >= 20 casi, trovati {}",
            cases.len()
        );

        let cfg = RoutingConfig::default();
        let mut checked = 0usize;
        for c in &cases {
            let msgs: Vec<Message> = c.messages.iter().map(RawMsg::to_message).collect();
            let got: Value = match c.group.as_str() {
                "has_filesystem_mutation" => {
                    Value::Bool(has_filesystem_mutation_in_history(&msgs, &cfg))
                }
                "has_tool_calls_in_history" => Value::Bool(has_tool_calls_in_history(&msgs)),
                "tool_result_outcome_after" => opt_bool(tool_result_outcome_after(&msgs, 0, 3)),
                "detect_repeated_failed_command" => {
                    let (cmd, count) = detect_repeated_failed_command(&msgs, 12);
                    json!({ "command": cmd, "count": count })
                }
                "detect_repeated_action" => {
                    let (label, count) = detect_repeated_action(&msgs, 24);
                    json!({ "label": label, "count": count })
                }
                "count_recent_request_port" => Value::from(count_recent_request_port(&msgs, 16)),
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
