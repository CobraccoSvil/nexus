//! `tool_dispatch`: helper PURI del `tool_dispatch_node` dell'executor.
//! Porting 1:1 da `brain/agents/nodes/helpers.py` (e blocchi del nodo in
//! `__init__.py`). Tutte le funzioni sono pure: nessun IO, nessuna lettura DB,
//! nessuna chiamata LLM/tool. Punto unico (regola L) di ciascun calcolo: il futuro
//! chiamante Rust delega qui invece di re-implementare.
//!
//! Costanti REALI verificate nel sorgente Python:
//!   - `RUN_NOTES_MAX_CHARS = 2400`            (helpers.py:451)
//!   - `MAX_TOOL_RESULT_CHARS = 6000`          (helpers.py:2298)
//!   - `MAX_CONTEXT_CHARS = 400_000`           (helpers.py:2300)
//!   - divisore token = `3.5`                  (helpers.py:3367/3389/3562)
//!
//! Funzioni:
//!   - [`apply_run_notes`]              -> `apply_run_notes` (helpers.py:474)
//!   - [`normalize_declared_outcome`]   -> `normalize_declared_outcome` (helpers.py:494)
//!   - [`estimate_tool_result_size_bytes`] -> `_estimate_tool_result_size_bytes` (helpers.py:3339)
//!   - [`extract_returned_bytes`]       -> `_extract_returned_bytes` (helpers.py:3650)
//!   - [`estimate_context_chars`]       -> `_estimate_context_chars` (helpers.py:2303)
//!   - [`current_context_token_estimate`] -> `_current_context_token_estimate` (helpers.py:3366)
//!   - [`append_reminder_block`]        -> `todo_reminder.append_reminder_block` (todo_reminder.py:96)

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cap delle note di run (tail-preserving). Vedi `RUN_NOTES_MAX_CHARS` Python.
pub const RUN_NOTES_MAX_CHARS: usize = 2400;

/// Cap del singolo tool_result (fallback di emergenza). Vedi `MAX_TOOL_RESULT_CHARS`.
pub const MAX_TOOL_RESULT_CHARS: usize = 6000;

/// Budget totale del contesto in char: oltre questa soglia i tool_result vengono
/// compressi. Vedi `MAX_CONTEXT_CHARS` Python.
pub const MAX_CONTEXT_CHARS: usize = 400_000;

/// Divisore char->token (stima ~ chars/3.5). Vedi le tre occorrenze `/3.5` Python.
pub const TOKEN_CHARS_DIVISOR: f64 = 3.5;

// ──────────────────────────────────────────────────────────────────────────
//  apply_run_notes
// ──────────────────────────────────────────────────────────────────────────

/// Applica un'azione `nexus_run_notes` (set/append) alle note correnti.
///
/// 1:1 con `apply_run_notes` Python: ritorna `None` se l'input e' invalido
/// (azione fuori da {set,append} o content vuoto dopo trim); `set` sostituisce,
/// `append` aggiunge una riga. Cap `RUN_NOTES_MAX_CHARS` tail-preserving:
/// `"[...]\n" + tail(len - 6)` (su byte/char ASCII e' lo stesso slice del Python).
///
/// NB: lo slicing tail replica `notes[-(RUN_NOTES_MAX_CHARS - 6):]` di Python, che
/// indicizza su CODEPOINT; qui usiamo i char per restare fedeli con testo non-ASCII.
pub fn apply_run_notes(current: Option<&str>, tool_input: &Value) -> Option<String> {
    let obj = tool_input.as_object()?;
    let action = obj
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let content = obj
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if (action != "set" && action != "append") || content.is_empty() {
        return None;
    }
    let mut notes = if action == "set" {
        content
    } else {
        match current {
            Some(c) if !c.is_empty() => format!("{}\n{}", c.trim_end(), content).trim().to_string(),
            _ => content,
        }
    };
    let notes_len = notes.chars().count();
    if notes_len > RUN_NOTES_MAX_CHARS {
        // Tail-preserving: tiene gli ultimi (RUN_NOTES_MAX_CHARS - 6) char.
        let keep = RUN_NOTES_MAX_CHARS - 6;
        let tail: String = notes.chars().skip(notes_len - keep).collect();
        notes = format!("[...]\n{tail}");
    }
    Some(notes)
}

// ──────────────────────────────────────────────────────────────────────────
//  normalize_declared_outcome (task_complete)
// ──────────────────────────────────────────────────────────────────────────

/// Outcome validi dichiarabili via `task_complete` (`_VALID_OUTCOMES` Python).
pub const VALID_OUTCOMES: &[&str] = &["done", "blocked", "needs_input"];

/// Valida/normalizza l'input di `task_complete`. `None` se invalido (outcome fuori
/// enum o input non-oggetto): il chiamante ricade sui segnali strutturali come se la
/// dichiarazione non ci fosse. 1:1 con `normalize_declared_outcome` Python.
///
/// L'output mantiene SEMPRE `outcome` e `summary` (anche vuoto), e aggiunge
/// `next_step`/`blocked_by` SOLO se truthy (stringa non vuota dopo trim), come
/// Python (`if v:`). Le chiavi sono inserite in ordine: outcome, summary, next_step,
/// blocked_by (preserve_order del workspace mantiene l'ordine d'inserimento).
pub fn normalize_declared_outcome(tool_input: &Value) -> Option<Value> {
    let obj = tool_input.as_object()?;
    let outcome = obj
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !VALID_OUTCOMES.contains(&outcome.as_str()) {
        return None;
    }
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let mut out = serde_json::Map::new();
    out.insert("outcome".to_string(), Value::String(outcome));
    out.insert("summary".to_string(), Value::String(summary));
    for k in ["next_step", "blocked_by"] {
        // Python: `v = tool_input.get(k); if v: out[k] = str(v).strip()`.
        // `if v` e' truthy: una stringa vuota e' falsy -> non inclusa. Per fedelta'
        // consideriamo "presente e non vuoto dopo trim". (gli input sono stringhe).
        if let Some(v) = obj.get(k).and_then(Value::as_str) {
            let v = v.trim();
            if !v.is_empty() {
                out.insert(k.to_string(), Value::String(v.to_string()));
            }
        }
    }
    Some(Value::Object(out))
}

// ──────────────────────────────────────────────────────────────────────────
//  estimate_tool_result_size_bytes (upper-bound per-tool)
// ──────────────────────────────────────────────────────────────────────────

/// Stima upper-bound dei byte attesi nel tool_result di `tool_name`.
/// 1:1 con `_estimate_tool_result_size_bytes` Python.
///
/// Per i tool di lettura allegato: `length` (default 102_400) * overhead, dove
/// overhead = 1.4 per encoding auto/base64, 1.05 altrimenti. Heuristiche fisse per
/// gli altri tool noti; default 5_000.
pub fn estimate_tool_result_size_bytes(tool_name: &str, args: &Value) -> i64 {
    let obj = args.as_object();
    if tool_name == "nexus_read_attachment" || tool_name == "nexus_read_archive_entry" {
        // length: int(length) se presente e parsabile, altrimenti 102_400.
        let length_i: i64 = obj
            .and_then(|o| o.get("length"))
            .map(parse_length_field)
            .unwrap_or(102_400);
        let encoding = obj
            .and_then(|o| o.get("encoding"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .to_lowercase();
        let overhead = if encoding == "auto" || encoding == "base64" {
            1.4
        } else {
            1.05
        };
        return (length_i as f64 * overhead) as i64;
    }
    match tool_name {
        "nexus_extract_pdf_text" => 100_000,
        "nexus_extract_docx_text" | "nexus_extract_xlsx_data" | "nexus_extract_figma_structure" => {
            80_000
        }
        "nexus_list_archive_entries" | "nexus_list_attachments" | "nexus_inspect_attachment" => {
            4_000
        }
        "nexus_describe_image_attachment" => 8_000,
        // Trascrizione audio: testo variabile (proporzionale alla durata); stima
        // come la descrizione immagine, sopra il default per non sottostimare.
        "nexus_transcribe_audio" => 8_000,
        _ => 5_000,
    }
}

/// Replica `int(length) if length is not None else 102_400` con fallback 102_400 in
/// caso di valore non convertibile (Python: `except: length_i = 102_400`). Accetta
/// numero JSON o stringa numerica (Python `int(...)` su entrambi).
fn parse_length_field(v: &Value) -> i64 {
    if v.is_null() {
        return 102_400;
    }
    if let Some(i) = v.as_i64() {
        return i;
    }
    if let Some(f) = v.as_f64() {
        return f as i64;
    }
    if let Some(s) = v.as_str() {
        // Python int("123") riesce; int("12.5") solleva -> fallback.
        if let Ok(i) = s.trim().parse::<i64>() {
            return i;
        }
    }
    102_400
}

// ──────────────────────────────────────────────────────────────────────────
//  extract_returned_bytes (parse length dal tool_result)
// ──────────────────────────────────────────────────────────────────────────

/// Estrae i byte effettivamente letti dal tool_result (`length` nel JSON).
/// 1:1 con `_extract_returned_bytes` Python: 0 se il content non e' JSON-oggetto o
/// `length` non e' un int (>= 0, `max(0, v)`). Nota: Python richiede `isinstance(v, int)`,
/// quindi un `length` float NON conta (resta 0).
pub fn extract_returned_bytes(result_content: &str) -> i64 {
    if result_content.is_empty() {
        return 0;
    }
    let Ok(data) = serde_json::from_str::<Value>(result_content) else {
        return 0;
    };
    let Some(obj) = data.as_object() else {
        return 0;
    };
    match obj.get("length") {
        // isinstance(v, int): un Number intero. as_i64 esclude i float JSON (12.0
        // sarebbe i64 in serde, ma Python `12.0` e' float -> non int). I tool_result
        // reali emettono `length` come intero JSON, coerente con as_i64.
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(0).max(0),
        _ => 0,
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  estimate_context_chars / current_context_token_estimate
//
//  Modello di messaggio dict-like (forma LangChain BaseMessage Python): `content`
//  str o lista di blocchi, piu' `anthropic_content` in additional_kwargs. I blocchi
//  sono dict arbitrari (Value): le due funzioni Python iterano i CAMPI dei dict in
//  modo diverso, quindi qui replichiamo la stessa semantica byte-fedele.
// ──────────────────────────────────────────────────────────────────────────

/// Messaggio nella forma usata dalle stime di contesto Python (`BaseMessage`).
///
/// - `content`: `Value` — stringa, lista di blocchi, o altro;
/// - `anthropic_content`: lista di blocchi (dict) da `additional_kwargs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextMessage {
    /// `m.content`: stringa o lista di blocchi (o assente -> Null).
    #[serde(default)]
    pub content: Value,
    /// `m.additional_kwargs["anthropic_content"]`: lista di blocchi dict (o assente).
    #[serde(default)]
    pub anthropic_content: Value,
}

/// `_estimate_context_chars`: somma i char di `m.content` (SOLO se stringa) PIU', per
/// ogni blocco di `anthropic_content`, la SOLA chiave `content` se e' stringa. 1:1.
///
/// NB: NON conta i blocchi di `m.content` quando e' una lista (Python guarda solo
/// `isinstance(m.content, str)`), ne' i campi diversi da `content` nei blocchi
/// anthropic. E' una stima volutamente piu' grezza di [`current_context_token_estimate`].
pub fn estimate_context_chars(messages: &[ContextMessage]) -> i64 {
    let mut total: i64 = 0;
    for m in messages {
        if let Value::String(s) = &m.content {
            total += s.chars().count() as i64;
        }
        if let Value::Array(blocks) = &m.anthropic_content {
            for b in blocks {
                if let Some(Value::String(c)) = b.as_object().and_then(|o| o.get("content")) {
                    total += c.chars().count() as i64;
                }
            }
        }
    }
    total
}

/// `_current_context_token_estimate`: stima i token totali (~ chars/3.5).
///
/// Somma: `len(system_text)` + per ogni messaggio `len(m.content)` se stringa + per
/// ogni blocco (sia in `m.content` lista che in `anthropic_content`) TUTTE le
/// stringhe nei VALUE del dict. Infine `int(total_chars / 3.5)`. 1:1 col Python.
pub fn current_context_token_estimate(messages: &[ContextMessage], system_text: &str) -> i64 {
    let mut total_chars: i64 = system_text.chars().count() as i64;
    for m in messages {
        match &m.content {
            Value::String(s) => total_chars += s.chars().count() as i64,
            Value::Array(blocks) => {
                for b in blocks {
                    total_chars += block_string_values_chars(b);
                }
            }
            _ => {}
        }
        if let Value::Array(blocks) = &m.anthropic_content {
            for b in blocks {
                total_chars += block_string_values_chars(b);
            }
        } else if let Value::String(s) = &m.anthropic_content {
            // Python: `elif isinstance(anth, str): total += len(anth)`.
            total_chars += s.chars().count() as i64;
        }
    }
    (total_chars as f64 / TOKEN_CHARS_DIVISOR) as i64
}

/// Somma i char di tutti i VALUE di tipo stringa di un blocco dict (`for v in
/// b.values(): if isinstance(v, str): total += len(v)`). 0 se il blocco non e' dict.
fn block_string_values_chars(block: &Value) -> i64 {
    let Some(obj) = block.as_object() else {
        return 0;
    };
    obj.values()
        .filter_map(Value::as_str)
        .map(|s| s.chars().count() as i64)
        .sum()
}

// ──────────────────────────────────────────────────────────────────────────
//  append_reminder_block
// ──────────────────────────────────────────────────────────────────────────

/// Aggiunge in coda alla lista di blocchi anthropic_content un blocco text con il
/// system-reminder. 1:1 con `todo_reminder.append_reminder_block`: no-op se il testo
/// e' vuoto; altrimenti push di `{"type":"text","text":"<system-reminder>\n{t}\n</system-reminder>"}`.
pub fn append_reminder_block(blocks: &mut Vec<Value>, reminder_text: &str) {
    if reminder_text.is_empty() {
        return;
    }
    blocks.push(serde_json::json!({
        "type": "text",
        "text": format!("<system-reminder>\n{reminder_text}\n</system-reminder>"),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn run_notes_set_e_append() {
        // set sostituisce.
        assert_eq!(
            apply_run_notes(Some("vecchio"), &json!({"action": "set", "content": " nuovo "})),
            Some("nuovo".to_string())
        );
        // append aggiunge una riga.
        assert_eq!(
            apply_run_notes(Some("riga1"), &json!({"action": "append", "content": "riga2"})),
            Some("riga1\nriga2".to_string())
        );
        // append senza note correnti = content.
        assert_eq!(
            apply_run_notes(None, &json!({"action": "append", "content": "primo"})),
            Some("primo".to_string())
        );
    }

    #[test]
    fn run_notes_invalido() {
        assert_eq!(apply_run_notes(None, &json!({"action": "x", "content": "y"})), None);
        assert_eq!(apply_run_notes(None, &json!({"action": "set", "content": "  "})), None);
        assert_eq!(apply_run_notes(None, &json!("non oggetto")), None);
    }

    #[test]
    fn run_notes_cap_tail() {
        let big = "a".repeat(RUN_NOTES_MAX_CHARS + 100);
        let out = apply_run_notes(None, &json!({"action": "set", "content": big})).unwrap();
        assert!(out.starts_with("[...]\n"));
        assert_eq!(out.chars().count(), RUN_NOTES_MAX_CHARS - 6 + "[...]\n".chars().count());
    }

    #[test]
    fn normalize_outcome_valido() {
        let out = normalize_declared_outcome(&json!({
            "outcome": "DONE", "summary": " fatto ", "next_step": "", "blocked_by": " dep "
        }))
        .unwrap();
        assert_eq!(out["outcome"], json!("done"));
        assert_eq!(out["summary"], json!("fatto"));
        // next_step vuoto -> assente; blocked_by presente.
        assert!(out.get("next_step").is_none());
        assert_eq!(out["blocked_by"], json!("dep"));
    }

    #[test]
    fn normalize_outcome_invalido() {
        assert_eq!(normalize_declared_outcome(&json!({"outcome": "fatto"})), None);
        assert_eq!(normalize_declared_outcome(&json!([1, 2])), None);
    }

    #[test]
    fn size_bytes_attachment_overhead() {
        // base64/auto -> 1.4x.
        assert_eq!(
            estimate_tool_result_size_bytes("nexus_read_attachment", &json!({"length": 1000})),
            1400
        );
        // text -> 1.05x.
        assert_eq!(
            estimate_tool_result_size_bytes(
                "nexus_read_attachment",
                &json!({"length": 1000, "encoding": "text"})
            ),
            1050
        );
        // length assente -> default 102_400 * 1.4.
        assert_eq!(
            estimate_tool_result_size_bytes("nexus_read_archive_entry", &json!({})),
            (102_400.0 * 1.4) as i64
        );
        // tool noti.
        assert_eq!(estimate_tool_result_size_bytes("nexus_extract_pdf_text", &json!({})), 100_000);
        assert_eq!(estimate_tool_result_size_bytes("tool_qualunque", &json!({})), 5_000);
    }

    #[test]
    fn returned_bytes_da_length() {
        assert_eq!(extract_returned_bytes(r#"{"length": 512}"#), 512);
        assert_eq!(extract_returned_bytes(r#"{"length": -5}"#), 0);
        assert_eq!(extract_returned_bytes(r#"{"other": 1}"#), 0);
        assert_eq!(extract_returned_bytes("non json"), 0);
        assert_eq!(extract_returned_bytes(""), 0);
    }

    #[test]
    fn context_chars_solo_content_str_e_block_content() {
        let msgs = vec![
            ContextMessage { content: json!("ciao"), anthropic_content: Value::Null },
            ContextMessage {
                content: json!(["ignorato perche' lista"]),
                anthropic_content: json!([{ "type": "text", "content": "AB" }, { "type": "tool_use", "name": "x" }]),
            },
        ];
        // "ciao"(4) + block content "AB"(2) = 6. La lista content del 2o e i campi
        // diversi da "content" non contano in estimate_context_chars.
        assert_eq!(estimate_context_chars(&msgs), 6);
    }

    #[test]
    fn token_estimate_somma_tutte_le_stringhe() {
        let msgs = vec![ContextMessage {
            content: json!("abcdefg"), // 7
            anthropic_content: json!([{ "type": "text", "text": "xyz" }]), // "text"3 + "xyz"3 = 6... e "type"="text" 4
        }];
        // system "sys"=3 + content 7 + block: type(4)+text(3) -> tutte le stringhe = 7.
        // totale char = 3 + 7 + 4 + 3 = 17 ; /3.5 = 4 (int).
        let got = current_context_token_estimate(&msgs, "sys");
        assert_eq!(got, ((3 + 7 + 4 + 3) as f64 / 3.5) as i64);
    }

    #[test]
    fn reminder_block_no_op_se_vuoto() {
        let mut blocks = vec![json!({"type": "text", "text": "x"})];
        append_reminder_block(&mut blocks, "");
        assert_eq!(blocks.len(), 1);
        append_reminder_block(&mut blocks, "ricorda");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["text"], json!("<system-reminder>\nricorda\n</system-reminder>"));
    }
}

/// Golden di parita' 1:1 vs Python per gli helper puri del tool_dispatch. Carica
/// `/tmp/golden_dispatch_pure.json` (vedi `gen_golden_dispatch_pure.py`, che importa
/// le funzioni REALI dal brain). I gruppi del predictive_cap sono ignorati qui (li
/// valuta `predictive_cap::golden` sullo STESSO file).
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    /// Mappa una spec di messaggio dello script ({content, anthropic_content}) in
    /// [`ContextMessage`]: i campi assenti/null diventano `Value::Null`.
    fn spec_to_ctx(spec: &Value) -> ContextMessage {
        ContextMessage {
            content: spec.get("content").cloned().unwrap_or(Value::Null),
            anthropic_content: spec.get("anthropic_content").cloned().unwrap_or(Value::Null),
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_dispatch_pure.json generato da gen_golden_dispatch_pure.py"]
    fn golden_dispatch_pure() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_dispatch_pure.json",
            "gen_golden_dispatch_pure.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        let mut checked = 0usize;
        for c in &cases {
            let inp = &c.input;
            let got: Value = match c.group.as_str() {
                "apply_run_notes" => {
                    let current = inp.get("current").and_then(Value::as_str);
                    let tool_input = inp.get("tool_input").cloned().unwrap_or(Value::Null);
                    match apply_run_notes(current, &tool_input) {
                        Some(s) => Value::String(s),
                        None => Value::Null,
                    }
                }
                "normalize_declared_outcome" => {
                    let tool_input = inp.get("tool_input").cloned().unwrap_or(Value::Null);
                    normalize_declared_outcome(&tool_input).unwrap_or(Value::Null)
                }
                "estimate_tool_result_size_bytes" => {
                    let tn = inp.get("tool_name").and_then(Value::as_str).unwrap_or("");
                    let args = inp.get("args").cloned().unwrap_or(Value::Null);
                    Value::from(estimate_tool_result_size_bytes(tn, &args))
                }
                "extract_returned_bytes" => {
                    let content = inp.get("result_content").and_then(Value::as_str).unwrap_or("");
                    Value::from(extract_returned_bytes(content))
                }
                "estimate_context_chars" => {
                    let msgs: Vec<ContextMessage> = inp
                        .get("messages")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().map(spec_to_ctx).collect())
                        .unwrap_or_default();
                    Value::from(estimate_context_chars(&msgs))
                }
                "current_context_token_estimate" => {
                    let msgs: Vec<ContextMessage> = inp
                        .get("messages")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().map(spec_to_ctx).collect())
                        .unwrap_or_default();
                    let system = inp.get("system_text").and_then(Value::as_str).unwrap_or("");
                    Value::from(current_context_token_estimate(&msgs, system))
                }
                "append_reminder_block" => {
                    let mut blocks: Vec<Value> = inp
                        .get("blocks")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let text = inp.get("reminder_text").and_then(Value::as_str).unwrap_or("");
                    append_reminder_block(&mut blocks, text);
                    Value::Array(blocks)
                }
                // I gruppi del predictive_cap sono valutati altrove (stesso file JSON).
                "predictive_cap_check" | "predictive_cap_sentinel" => continue,
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        assert!(checked >= 40, "attesi >= 40 casi dispatch, verificati {checked}");
        println!("golden dispatch_pure (tool_dispatch): {checked} casi verificati, tutti verdi");
    }
}
