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

/// Outcome validi dichiarabili via `task_complete` (`_VALID_OUTCOMES` Python +
/// `partial` da ADR 0034: lavoro completato solo in parte, dichiarato onestamente).
pub const VALID_OUTCOMES: &[&str] = &["done", "blocked", "needs_input", "partial"];

/// Categorie macchina della causa di blocco (`blocker`, ADR 0034): segnale ENUM,
/// mai dedotto dalla prosa (regola M). Fuori enum -> campo scartato (il resto
/// della dichiarazione resta valido).
pub const VALID_BLOCKERS: &[&str] = &[
    "dependency",
    "credential",
    "permission",
    "service",
    "request_ambiguity",
    "safety",
];

/// Cap sul numero di voci accettate in `files_touched` (self-report, solo
/// display/telemetria: il ground truth resta `modified_files_from_messages`).
const FILES_TOUCHED_CAP: usize = 50;

/// Valida/normalizza l'input di `task_complete`. `None` se invalido (outcome fuori
/// enum o input non-oggetto): il chiamante ricade sui segnali strutturali come se la
/// dichiarazione non ci fosse. Base 1:1 con `normalize_declared_outcome` Python,
/// estesa dai campi ADR 0034 (`blocker`, `refusal`, `files_touched`).
///
/// L'output mantiene SEMPRE `outcome` e `summary` (anche vuoto), e aggiunge
/// `next_step`/`blocked_by` SOLO se truthy (stringa non vuota dopo trim), come
/// Python (`if v:`). `blocker` solo se nell'enum [`VALID_BLOCKERS`]; `refusal`
/// solo se `true`; `files_touched` solo se array non vuoto di stringhe non vuote
/// (cap [`FILES_TOUCHED_CAP`]). Le chiavi sono inserite in ordine d'inserimento
/// (preserve_order del workspace).
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
    // `blocker` (ADR 0034): categoria macchina, accettata SOLO se nell'enum.
    if let Some(b) = obj.get("blocker").and_then(Value::as_str) {
        let b = b.trim().to_lowercase();
        if VALID_BLOCKERS.contains(&b.as_str()) {
            out.insert("blocker".to_string(), Value::String(b));
        }
    }
    // `refusal` (ADR 0034): bool, incluso solo se true (assente = false).
    if obj.get("refusal").and_then(Value::as_bool) == Some(true) {
        out.insert("refusal".to_string(), Value::Bool(true));
    }
    // `files_touched` (ADR 0034): self-report, solo display/telemetria.
    if let Some(arr) = obj.get("files_touched").and_then(Value::as_array) {
        let files: Vec<Value> = arr
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(FILES_TOUCHED_CAP)
            .map(|s| Value::String(s.to_string()))
            .collect();
        if !files.is_empty() {
            out.insert("files_touched".to_string(), Value::Array(files));
        }
    }
    Some(Value::Object(out))
}

// ──────────────────────────────────────────────────────────────────────────
//  normalize_review_verdict (review_verdict, Fase B ultracode)
// ──────────────────────────────────────────────────────────────────────────

/// Verdetti validi dichiarabili via `review_verdict` (segnale ENUM, regola M:
/// mai dedotto dalla prosa della review).
pub const VALID_REVIEW_VERDICTS: &[&str] = &["pass", "fail", "needs_changes"];

/// Severita' valide di un finding/rischio. Fuori enum -> default `media` (il
/// finding resta: la severita' e' un attributo, non un gate di validita').
///
/// Deriva dal PUNTO UNICO del vocabolario ([`super::severity::Severity`], regola
/// L): resta un `&[&str]` perche' e' cosi' che il test di coerenza cross-crate
/// lo confronta con l'enum dello schema del tool. Il RICONOSCIMENTO delega a
/// `Severity::try_parse` ([`normalize_severity`]), non a questa lista.
pub const VALID_FINDING_SEVERITIES: &[&str] = &[
    super::severity::Severity::High.as_str(),
    super::severity::Severity::Medium.as_str(),
    super::severity::Severity::Low.as_str(),
];

/// Severita' sanificata alla FRONTIERA (input LLM): riconosciuta dal punto unico
/// [`super::severity::Severity::try_parse`]; fuori vocabolario -> `media`, cosi'
/// a valle i panel leggono sempre un valore canonico. Regola L: unica sede della
/// sanificazione, condivisa da review/advisory/debate.
fn normalize_severity(raw: Option<&str>) -> String {
    raw.and_then(super::severity::Severity::try_parse)
        .unwrap_or(super::severity::Severity::Medium)
        .as_str()
        .to_string()
}

/// Sanifica una lista di stringhe di un verdetto strutturato (`requirements`,
/// `recommendations`, `key_arguments`): trim, scarto delle vuote, lista bounded.
fn normalize_string_list(obj: &serde_json::Map<String, Value>, key: &str) -> Vec<Value> {
    obj.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let s = v.as_str()?.trim();
                    (!s.is_empty()).then(|| Value::String(s.to_string()))
                })
                .take(ADVISORY_LIST_CAP)
                .collect()
        })
        .unwrap_or_default()
}

/// Sanifica la lista `risks` di un verdetto strutturato. PUNTO UNICO (regola L)
/// condiviso da `advisory_verdict` e `debate_position`: stessa forma
/// (`{severity, area?, description}`), stesse regole (description non vuota
/// obbligatoria, severity normalizzata, area solo se valorizzata, lista
/// bounded). Erano due blocchi identici, uno per tool.
///
/// I `findings` della review NON passano di qui: hanno una forma diversa
/// (`file` obbligatorio, `line` opzionale) — condividerebbero la firma ma non
/// la semantica, e forzarli in un'unica funzione parametrica sarebbe il tipo di
/// astrazione che si paga a ogni lettura.
fn normalize_risk_list(obj: &serde_json::Map<String, Value>) -> Vec<Value> {
    obj.get("risks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let ro = r.as_object()?;
                    let description = ro.get("description").and_then(Value::as_str)?.trim();
                    if description.is_empty() {
                        return None;
                    }
                    let mut out = serde_json::Map::new();
                    out.insert(
                        "severity".to_string(),
                        Value::String(normalize_severity(
                            ro.get("severity").and_then(Value::as_str),
                        )),
                    );
                    if let Some(area) = ro.get("area").and_then(Value::as_str) {
                        let area = area.trim();
                        if !area.is_empty() {
                            out.insert("area".to_string(), Value::String(area.to_string()));
                        }
                    }
                    out.insert(
                        "description".to_string(),
                        Value::String(description.to_string()),
                    );
                    Some(Value::Object(out))
                })
                .take(ADVISORY_LIST_CAP)
                .collect()
        })
        .unwrap_or_default()
}

/// Cap sul numero di findings accettati (stesso razionale di
/// [`FILES_TOUCHED_CAP`]: self-report bounded, mai illimitato).
const FINDINGS_CAP: usize = 50;

/// Valida/normalizza l'input di `review_verdict` (gemello di
/// [`normalize_declared_outcome`] per il canale del REVISORE, Fase B ultracode).
/// `None` se invalido (verdict fuori enum, input non-oggetto, oppure
/// fail/needs_changes SENZA alcun finding valido: un verdetto negativo senza
/// evidenza non e' componibile da un coordinatore e va rifiutato alla fonte).
///
/// L'output mantiene SEMPRE `verdict` e `summary` (anche vuoto); `findings` e'
/// incluso solo se non vuoto dopo la sanificazione: ogni finding richiede
/// `file` e `description` non vuoti, `severity` fuori enum ricade su `media`,
/// `line` e' incluso solo se intero positivo. Cap [`FINDINGS_CAP`].
pub fn normalize_review_verdict(tool_input: &Value) -> Option<Value> {
    let obj = tool_input.as_object()?;
    let verdict = obj
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !VALID_REVIEW_VERDICTS.contains(&verdict.as_str()) {
        return None;
    }
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let findings: Vec<Value> = obj
        .get("findings")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let fo = f.as_object()?;
                    let file = fo.get("file").and_then(Value::as_str)?.trim();
                    let description = fo.get("description").and_then(Value::as_str)?.trim();
                    if file.is_empty() || description.is_empty() {
                        return None;
                    }
                    let severity = normalize_severity(fo.get("severity").and_then(Value::as_str));
                    let mut out = serde_json::Map::new();
                    out.insert("file".to_string(), Value::String(file.to_string()));
                    if let Some(line) = fo.get("line").and_then(Value::as_i64) {
                        if line > 0 {
                            out.insert("line".to_string(), Value::from(line));
                        }
                    }
                    out.insert("severity".to_string(), Value::String(severity));
                    out.insert(
                        "description".to_string(),
                        Value::String(description.to_string()),
                    );
                    Some(Value::Object(out))
                })
                .take(FINDINGS_CAP)
                .collect()
        })
        .unwrap_or_default();
    // Verdetto negativo senza evidenza: rifiutato alla fonte (regola M — il
    // coordinatore non deve mai dover "credere" a un fail senza findings).
    if verdict != "pass" && findings.is_empty() {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("verdict".to_string(), Value::String(verdict));
    out.insert("summary".to_string(), Value::String(summary));
    if !findings.is_empty() {
        out.insert("findings".to_string(), Value::Array(findings));
    }
    Some(Value::Object(out))
}

// ──────────────────────────────────────────────────────────────────────────
//  normalize_advisory_verdict (advisory_verdict, consiglio di figure a monte)
// ──────────────────────────────────────────────────────────────────────────

/// Verdetti validi dichiarabili via `advisory_verdict` (segnale ENUM, regola M:
/// mai dedotto dalla prosa del parere).
pub const VALID_ADVISORY_VERDICTS: &[&str] = &["proceed", "proceed_with_changes", "block"];

/// Cap sul numero di elementi (requirements/risks/recommendations) per parere
/// (self-report bounded, stesso razionale di [`FINDINGS_CAP`]).
const ADVISORY_LIST_CAP: usize = 30;

/// Valida/normalizza l'input di `advisory_verdict` (gemello di
/// [`normalize_review_verdict`] per il canale delle FIGURE del consiglio a monte).
/// `None` se invalido: verdict fuori enum, input non-oggetto, oppure
/// verdict=block SENZA alcun rischio con descrizione (un veto senza evidenza non
/// e' componibile da un coordinatore e va rifiutato alla fonte, regola M).
///
/// L'output mantiene SEMPRE `verdict` e `summary`; `requirements`/`risks`/
/// `recommendations` inclusi solo se non vuoti dopo la sanificazione. Ogni
/// requirement/recommendation e' una stringa non vuota (trim); ogni risk richiede
/// `description` non vuota, `severity` fuori enum ricade su `media`, `area`
/// inclusa solo se non vuota. La forma dell'output e' quella letta dal punto unico
/// [`super::advisory_panel::compose_advisory_synthesis`] nel campo `advisory`.
pub fn normalize_advisory_verdict(tool_input: &Value) -> Option<Value> {
    let obj = tool_input.as_object()?;
    let verdict = obj
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !VALID_ADVISORY_VERDICTS.contains(&verdict.as_str()) {
        return None;
    }
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let requirements = normalize_string_list(obj, "requirements");
    let recommendations = normalize_string_list(obj, "recommendations");
    let risks = normalize_risk_list(obj);
    // Veto senza evidenza: rifiutato alla fonte (regola M — il coordinatore non
    // deve mai dover "credere" a un block senza rischi con descrizione).
    if verdict == "block" && risks.is_empty() {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("verdict".to_string(), Value::String(verdict));
    out.insert("summary".to_string(), Value::String(summary));
    if !requirements.is_empty() {
        out.insert("requirements".to_string(), Value::Array(requirements));
    }
    if !risks.is_empty() {
        out.insert("risks".to_string(), Value::Array(risks));
    }
    if !recommendations.is_empty() {
        out.insert("recommendations".to_string(), Value::Array(recommendations));
    }
    if let Some(cd) = normalize_contested_decision(obj.get("contested_decision")) {
        out.insert("contested_decision".to_string(), cd);
    }
    Some(Value::Object(out))
}

/// Minimo di opzioni perche' una decisione sia CONTESA: una sola alternativa non
/// e' una scelta, e' una constatazione. Soglia di forma del dato (non config di
/// business): sotto questa il campo non esiste proprio.
const CONTESTED_MIN_OPTIONS: usize = 2;

/// Cap sulle opzioni di una decisione contesa: oltre, il dibattito diventa un
/// sondaggio (ogni opzione va difesa da un avvocato). Stesso razionale bounded
/// di [`ADVISORY_LIST_CAP`].
const CONTESTED_MAX_OPTIONS: usize = 5;

/// Valida/normalizza il campo `contested_decision` di `advisory_verdict`: la
/// DICHIARAZIONE di una decisione architetturale aperta, che innesca il dibattito
/// a tesi contrapposte (regola M: segnale strutturato, mai dedotto dalla prosa
/// del parere).
///
/// `None` (campo assente dall'output) se: non e' un oggetto, `topic` vuoto, o le
/// opzioni distinte non vuote sono meno di [`CONTESTED_MIN_OPTIONS`]. Una
/// dichiarazione monca NON deve convocare avvocati: sarebbe spesa garantita per
/// un dibattito senza contraddittorio.
///
/// Le opzioni sono deduplicate (case-insensitive, trim) preservando l'ordine di
/// prima apparizione: e' l'ordine che [`super::debate_panel::plan_debate`] usa
/// per il round-robin e [`super::debate_panel::compose_debate_synthesis`] per il
/// tally stabile.
pub fn normalize_contested_decision(raw: Option<&Value>) -> Option<Value> {
    let obj = raw?.as_object()?;
    let topic = obj.get("topic").and_then(Value::as_str)?.trim();
    if topic.is_empty() {
        return None;
    }
    let mut options: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for o in obj.get("options").and_then(Value::as_array)? {
        let Some(s) = o.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let key = s.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        options.push(Value::String(s.to_string()));
        if options.len() >= CONTESTED_MAX_OPTIONS {
            break;
        }
    }
    if options.len() < CONTESTED_MIN_OPTIONS {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("topic".to_string(), Value::String(topic.to_string()));
    out.insert("options".to_string(), Value::Array(options));
    Some(Value::Object(out))
}

/// Posizioni dichiarabili da un avvocato via `debate_position` (segnale ENUM,
/// regola M). Derivate dal punto unico [`super::debate_panel::Stance`].
pub const VALID_DEBATE_STANCES: &[&str] = &[
    super::debate_panel::Stance::Support.as_str(),
    super::debate_panel::Stance::Oppose.as_str(),
];

/// Valida/normalizza l'input di `debate_position` (gemello di
/// [`normalize_advisory_verdict`] per il canale degli AVVOCATI del dibattito).
///
/// `None` se invalido: input non-oggetto, `assigned_position` vuota (senza la
/// chiave di attribuzione il voto non e' assegnabile a un'opzione), `stance`
/// fuori enum, oppure `stance=oppose` SENZA alcun rischio con descrizione —
/// arrendere la propria tesi e' un segnale FORTE (squalifica l'opzione anche in
/// minoranza): senza evidenza non e' componibile e va rifiutato alla fonte,
/// esattamente come un `block` senza rischi.
///
/// L'output mantiene sempre `assigned_position`, `stance`, `summary`; le liste
/// solo se non vuote. La forma e' quella letta da
/// [`super::debate_panel::compose_debate_synthesis`] nel campo `debate`.
pub fn normalize_debate_position(tool_input: &Value) -> Option<Value> {
    let obj = tool_input.as_object()?;
    let assigned = obj
        .get("assigned_position")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if assigned.is_empty() {
        return None;
    }
    let stance = obj
        .get("stance")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !VALID_DEBATE_STANCES.contains(&stance.as_str()) {
        return None;
    }
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let key_arguments = normalize_string_list(obj, "key_arguments");
    let risks = normalize_risk_list(obj);
    // Resa senza evidenza: rifiutata alla fonte (regola M). Un `oppose` squalifica
    // l'opzione anche in minoranza: deve portare prove, non solo una conclusione.
    if stance == super::debate_panel::Stance::Oppose.as_str() && risks.is_empty() {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("assigned_position".to_string(), Value::String(assigned));
    out.insert("stance".to_string(), Value::String(stance));
    out.insert("summary".to_string(), Value::String(summary));
    if !key_arguments.is_empty() {
        out.insert("key_arguments".to_string(), Value::Array(key_arguments));
    }
    if !risks.is_empty() {
        out.insert("risks".to_string(), Value::Array(risks));
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

/// Appiattisce nel buffer ESATTAMENTE le stringhe che
/// [`current_context_token_estimate`] conta (stesso perimetro, punto unico —
/// ADR 0016 D1): serve al conteggio token REALE via `TokenCounter`, dove il
/// testo concreto e' necessario (una BPE non lavora su conteggi di char).
/// Separatore '\n' tra i frammenti: innocuo per la stima, evita che due
/// frammenti si saldino in un token spurio.
pub fn flatten_context_text(messages: &[ContextMessage], system_text: &str) -> String {
    let mut out = String::with_capacity(4096);
    let mut push = |s: &str| {
        if !s.is_empty() {
            out.push_str(s);
            out.push('\n');
        }
    };
    push(system_text);
    for m in messages {
        match &m.content {
            Value::String(s) => push(s),
            Value::Array(blocks) => {
                for b in blocks {
                    if let Some(obj) = b.as_object() {
                        for v in obj.values() {
                            if let Some(s) = v.as_str() {
                                push(s);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        match &m.anthropic_content {
            Value::Array(blocks) => {
                for b in blocks {
                    if let Some(obj) = b.as_object() {
                        for v in obj.values() {
                            if let Some(s) = v.as_str() {
                                push(s);
                            }
                        }
                    }
                }
            }
            Value::String(s) => push(s),
            _ => {}
        }
    }
    out
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
            apply_run_notes(
                Some("vecchio"),
                &json!({"action": "set", "content": " nuovo "})
            ),
            Some("nuovo".to_string())
        );
        // append aggiunge una riga.
        assert_eq!(
            apply_run_notes(
                Some("riga1"),
                &json!({"action": "append", "content": "riga2"})
            ),
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
        assert_eq!(
            apply_run_notes(None, &json!({"action": "x", "content": "y"})),
            None
        );
        assert_eq!(
            apply_run_notes(None, &json!({"action": "set", "content": "  "})),
            None
        );
        assert_eq!(apply_run_notes(None, &json!("non oggetto")), None);
    }

    #[test]
    fn run_notes_cap_tail() {
        let big = "a".repeat(RUN_NOTES_MAX_CHARS + 100);
        let out = apply_run_notes(None, &json!({"action": "set", "content": big})).unwrap();
        assert!(out.starts_with("[...]\n"));
        assert_eq!(
            out.chars().count(),
            RUN_NOTES_MAX_CHARS - 6 + "[...]\n".chars().count()
        );
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
        assert_eq!(
            normalize_declared_outcome(&json!({"outcome": "fatto"})),
            None
        );
        assert_eq!(normalize_declared_outcome(&json!([1, 2])), None);
    }

    #[test]
    fn normalize_outcome_adr0034_campi_estesi() {
        // "partial" e' un outcome valido; blocker nell'enum passa; refusal=true
        // passa; files_touched filtrato (vuoti scartati, trim applicato).
        let out = normalize_declared_outcome(&json!({
            "outcome": "PARTIAL",
            "summary": "meta' lavoro",
            "blocker": " SERVICE ",
            "refusal": true,
            "files_touched": [" src/a.ts ", "", "src/b.ts"]
        }))
        .unwrap();
        assert_eq!(out["outcome"], json!("partial"));
        assert_eq!(out["blocker"], json!("service"));
        assert_eq!(out["refusal"], json!(true));
        assert_eq!(out["files_touched"], json!(["src/a.ts", "src/b.ts"]));
    }

    #[test]
    fn normalize_outcome_adr0034_campi_estesi_scartati() {
        // blocker fuori enum -> scartato (il resto valido resta); refusal=false
        // -> assente; files_touched non-array -> assente.
        let out = normalize_declared_outcome(&json!({
            "outcome": "blocked",
            "summary": "fermo",
            "blocker": "colpa-del-meteo",
            "refusal": false,
            "files_touched": "src/a.ts"
        }))
        .unwrap();
        assert_eq!(out["outcome"], json!("blocked"));
        assert!(out.get("blocker").is_none());
        assert!(out.get("refusal").is_none());
        assert!(out.get("files_touched").is_none());
    }

    #[test]
    fn normalize_review_verdict_pass_e_needs_changes() {
        // pass senza findings: valido (nessun difetto trovato).
        let pass = normalize_review_verdict(&json!({
            "verdict": " PASS ",
            "summary": "  tutto ok  "
        }))
        .unwrap();
        assert_eq!(pass["verdict"], json!("pass"));
        assert_eq!(pass["summary"], json!("tutto ok"));
        assert!(pass.get("findings").is_none());

        // needs_changes con finding completo: severita' normalizzata, line inclusa.
        let nc = normalize_review_verdict(&json!({
            "verdict": "needs_changes",
            "summary": "un fix",
            "findings": [{
                "file": " src/a.rs ",
                "line": 42,
                "severity": "ALTA",
                "description": " off-by-one nel cap "
            }]
        }))
        .unwrap();
        assert_eq!(nc["verdict"], json!("needs_changes"));
        let f = &nc["findings"][0];
        assert_eq!(f["file"], json!("src/a.rs"));
        assert_eq!(f["line"], json!(42));
        assert_eq!(f["severity"], json!("alta"));
        assert_eq!(f["description"], json!("off-by-one nel cap"));
    }

    #[test]
    fn normalize_review_verdict_invalidi() {
        // verdict fuori enum -> None.
        assert!(normalize_review_verdict(&json!({"verdict": "boh", "summary": "x"})).is_none());
        // input non-oggetto -> None.
        assert!(normalize_review_verdict(&json!("fail")).is_none());
        // fail SENZA findings validi -> None (verdetto negativo senza evidenza).
        assert!(
            normalize_review_verdict(&json!({"verdict": "fail", "summary": "brutto"})).is_none()
        );
        // fail con findings tutti invalidi (file/description vuoti) -> None.
        assert!(normalize_review_verdict(&json!({
            "verdict": "fail",
            "summary": "brutto",
            "findings": [{"file": "", "description": "x"}, {"file": "a.rs", "description": ""}]
        }))
        .is_none());
    }

    #[test]
    fn normalize_review_verdict_sanifica_findings() {
        // severita' fuori enum -> media; line non positiva -> assente; finding
        // senza file -> scartato (il valido resta).
        let out = normalize_review_verdict(&json!({
            "verdict": "fail",
            "summary": "difetti",
            "findings": [
                {"file": "a.rs", "line": 0, "severity": "critica", "description": "bug"},
                {"description": "orfano senza file"}
            ]
        }))
        .unwrap();
        let findings = out["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["severity"], json!("media"));
        assert!(findings[0].get("line").is_none());
    }

    #[test]
    fn normalize_advisory_verdict_proceed_e_block() {
        // proceed senza risks: valido (via libera dalla lente della figura).
        let p = normalize_advisory_verdict(&json!({"verdict": " PROCEED ", "summary": "  ok  "}))
            .unwrap();
        assert_eq!(p["verdict"], json!("proceed"));
        assert_eq!(p["summary"], json!("ok"));
        assert!(p.get("risks").is_none());

        // block con risk completo: severity normalizzata, area/requirements/reco trim.
        let b = normalize_advisory_verdict(&json!({
            "verdict": "block",
            "summary": "manca PKCE",
            "requirements": [" usa PKCE ", ""],
            "risks": [{"severity": "ALTA", "area": " auth ", "description": " redirect aperto "}],
            "recommendations": ["aggiungi test"]
        }))
        .unwrap();
        assert_eq!(b["verdict"], json!("block"));
        assert_eq!(b["requirements"], json!(["usa PKCE"]));
        let r = &b["risks"][0];
        assert_eq!(r["severity"], json!("alta"));
        assert_eq!(r["area"], json!("auth"));
        assert_eq!(r["description"], json!("redirect aperto"));
        assert_eq!(b["recommendations"], json!(["aggiungi test"]));
    }

    #[test]
    fn normalize_advisory_verdict_invalidi() {
        // verdict fuori enum -> None.
        assert!(normalize_advisory_verdict(&json!({"verdict": "boh", "summary": "x"})).is_none());
        // input non-oggetto -> None.
        assert!(normalize_advisory_verdict(&json!("block")).is_none());
        // block SENZA rischi -> None (veto senza evidenza rifiutato alla fonte).
        assert!(
            normalize_advisory_verdict(&json!({"verdict": "block", "summary": "no"})).is_none()
        );
        // block con rischi tutti invalidi (description vuota) -> None.
        assert!(normalize_advisory_verdict(&json!({
            "verdict": "block",
            "summary": "no",
            "risks": [{"severity": "alta", "description": ""}]
        }))
        .is_none());
    }

    #[test]
    fn normalize_advisory_verdict_proceed_with_changes_requisiti() {
        // proceed_with_changes: risks opzionali, requirements preservati.
        let out = normalize_advisory_verdict(&json!({
            "verdict": "proceed_with_changes",
            "summary": "quasi",
            "requirements": ["valida input"]
        }))
        .unwrap();
        assert_eq!(out["verdict"], json!("proceed_with_changes"));
        assert_eq!(out["requirements"], json!(["valida input"]));
        assert!(out.get("risks").is_none());
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
        assert_eq!(
            estimate_tool_result_size_bytes("nexus_extract_pdf_text", &json!({})),
            100_000
        );
        assert_eq!(
            estimate_tool_result_size_bytes("tool_qualunque", &json!({})),
            5_000
        );
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
            ContextMessage {
                content: json!("ciao"),
                anthropic_content: Value::Null,
            },
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
            content: json!("abcdefg"),                                     // 7
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
        assert_eq!(
            blocks[1]["text"],
            json!("<system-reminder>\nricorda\n</system-reminder>")
        );
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
            anthropic_content: spec
                .get("anthropic_content")
                .cloned()
                .unwrap_or(Value::Null),
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
                    let content = inp
                        .get("result_content")
                        .and_then(Value::as_str)
                        .unwrap_or("");
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
                    let text = inp
                        .get("reminder_text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
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
        assert!(
            checked >= 40,
            "attesi >= 40 casi dispatch, verificati {checked}"
        );
        println!("golden dispatch_pure (tool_dispatch): {checked} casi verificati, tutti verdi");
    }
}
