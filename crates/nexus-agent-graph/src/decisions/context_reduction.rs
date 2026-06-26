//! `context_reduction`: parte PURA della riduzione del contesto dell'executor.
//!
//! Porting 1:1 (FASE 2a) della fetta DETERMINISTICA del context management del
//! brain (`brain/agents/nodes/__init__.py` area ~2680-2911 + `helpers.py`).
//! Ogni funzione e' un PUNTO UNICO (regola L) del proprio concern: il futuro
//! chiamante Rust (il nodo executor, in un PR successivo) delega qui invece di
//! re-implementare la logica.
//!
//! ## Confine PURO / I/O (separazione netta)
//!
//! Questo modulo contiene SOLO la parte pura: stessa entrata -> stessa uscita,
//! nessuna lettura DB, nessuna chiamata LLM/embeddings/tiktoken/Qdrant. Le parti
//! I/O dell'executor NON sono qui (TODO espliciti, diventeranno trait dedicati):
//!   - `summarizer.summarize_old_messages` (LLM small)  -> trait `SummaryStore`;
//!   - `_offload_system_prompt_if_huge` (Qdrant)        -> trait `EmbeddingStore`;
//!   - `apply_continuity_trim` (embeddings/cosine)      -> trait `EmbeddingStore`;
//!   - `_apply_rolling_summary` (Qdrant batch)          -> trait `EmbeddingStore`;
//!   - `_smart_upscale_model`, `_model_context_window`  -> DB (routing/catalog).
//!
//! Due punti di confine sono parametrizzati con CALLBACK pure cosi' la decisione
//! resta qui e l'I/O resta fuori (regola G - niente IO nella primitiva):
//!   - [`compress_old_tool_results`] accetta `marker_fn`: in produzione fa
//!     l'offload RAG del contenuto (I/O); il default puro [`degraded_marker`]
//!     replica il marker "degraded" del Python quando l'offload e' assente.
//!   - [`apply_token_brake`] accetta `token_estimator`: in produzione e' tiktoken
//!     (`_estimate_context_tokens`, I/O esterno); la decisione di troncamento e
//!     i cap sono qui, puri.
//!
//! ## Modello messaggio della history
//!
//! Le funzioni Python operano sul dualismo `m.content` (str o lista di blocchi)
//! + `m.additional_kwargs["anthropic_content"]` (lista di blocchi `tool_use`/
//! `tool_result`/`text`). I tool_result manipolati stanno SEMPRE in
//! `anthropic_content`. [`HistoryMessage`] modella questa forma 1:1 (blocchi
//! opachi come [`Value`], coerente con [`super::tool_dispatch::ContextMessage`]).
//! Le funzioni Python che modificano i blocchi ricreano un `HumanMessage` con
//! `additional_kwargs={anthropic_content}` (perdendo gli altri kwargs e
//! diventando di tipo human): [`HistoryMessage::rebuilt_human`] replica esatto.
//!
//! Costanti/primitive RIUSATE (regola L), NON ridefinite:
//!   - [`MAX_CONTEXT_CHARS`], [`MAX_TOOL_RESULT_CHARS`], [`TOKEN_CHARS_DIVISOR`]
//!     da [`super::tool_dispatch`];
//!   - [`build_turn_focus_directive`], [`TURN_FOCUS_MARKER`] da [`super::turn_focus`];
//!   - [`MessageContent::flatten_text`] per l'estrazione testo;
//!   - [`crate::py_json::py_json_dumps`] (`SortKeys::Yes`) per la canonicalizzazione
//!     args della signature.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::py_json::{py_json_dumps, SortKeys};

// Riuso esplicito dei punti unici gia' portati (regola L): NON ridefiniti qui.
pub use super::tool_dispatch::{MAX_CONTEXT_CHARS, MAX_TOOL_RESULT_CHARS, TOKEN_CHARS_DIVISOR};
pub use super::turn_focus::{build_turn_focus_directive, TURN_FOCUS_MARKER};

// ──────────────────────────────────────────────────────────────────────────
//  Marcatori di idempotenza delle iniezioni nel system_text (helpers.py)
// ──────────────────────────────────────────────────────────────────────────

/// `_LANG_REMINDER_MARKER` (helpers.py:644). Idempotenza del reminder lingua.
pub const LANG_REMINDER_MARKER: &str = "[[NEXUS_LANG_REMINDER]]";

/// `_VERIFY_DIRECTIVE_MARKER` (helpers.py:1241). Idempotenza direttiva verifica.
pub const VERIFY_DIRECTIVE_MARKER: &str = "[[NEXUS_VERIFY_DIRECTIVE]]";

/// `_RAG_REMINDER_MARKER` (helpers.py:673). Idempotenza forced-RAG reminder.
pub const RAG_REMINDER_MARKER: &str = "[[NEXUS_FORCED_RAG_REMINDER]]";

// ──────────────────────────────────────────────────────────────────────────
//  Modello messaggio della history
// ──────────────────────────────────────────────────────────────────────────

/// Un messaggio della history nella forma su cui operano le trasformazioni di
/// context reduction del brain (`BaseMessage` LangChain).
///
/// - `is_human`: `m.type == "human"` (`_first_human_index`); le funzioni di dedup
///   ricreano `HumanMessage` -> `is_human=true`.
/// - `content`: `m.content` (str o lista di blocchi o altro).
/// - `anthropic_content`: `m.additional_kwargs["anthropic_content"]` (lista di
///   blocchi dict opachi). `Value::Null`/non-array = assente.
/// - `nexus_summary` / `rolling_summary`: flag `additional_kwargs` per
///   `_is_summary_message` (preservazione del riassunto rolling).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessage {
    /// `m.type == "human"`.
    #[serde(default)]
    pub is_human: bool,
    /// `m.content`: stringa, lista di blocchi, o altro.
    #[serde(default)]
    pub content: Value,
    /// `additional_kwargs["anthropic_content"]`: lista di blocchi (o assente).
    #[serde(default)]
    pub anthropic_content: Value,
    /// `additional_kwargs["nexus_summary"]` truthy.
    #[serde(default)]
    pub nexus_summary: bool,
    /// `additional_kwargs["rolling_summary"]` truthy.
    #[serde(default)]
    pub rolling_summary: bool,
}

impl HistoryMessage {
    /// Ricrea il messaggio come `HumanMessage(content, additional_kwargs={
    /// "anthropic_content": new_blocks})`: 1:1 con la ricostruzione Python nelle
    /// funzioni di dedup/drop/compress. Il content e' preservato; i flag summary
    /// vengono PERSI (Python non li ricopia nei nuovi additional_kwargs).
    fn rebuilt_human(content: Value, new_blocks: Vec<Value>) -> Self {
        HistoryMessage {
            is_human: true,
            content,
            anthropic_content: Value::Array(new_blocks),
            nexus_summary: false,
            rolling_summary: false,
        }
    }

    /// `additional_kwargs.get("anthropic_content")` se e' una lista, altrimenti
    /// `None` (replica `if not isinstance(blocks, list)`).
    fn anthropic_blocks(&self) -> Option<&Vec<Value>> {
        self.anthropic_content.as_array()
    }

    /// `_is_summary_message`: flag truthy oppure `content` stringa che inizia
    /// (dopo `lstrip`) con `"[RIASSUNTO"`.
    fn is_summary(&self) -> bool {
        if self.nexus_summary || self.rolling_summary {
            return true;
        }
        match &self.content {
            Value::String(s) => s.trim_start().starts_with("[RIASSUNTO"),
            _ => false,
        }
    }
}

/// `_first_human_index`: indice del primo messaggio human, -1 se assente.
pub fn first_human_index(messages: &[HistoryMessage]) -> i64 {
    messages
        .iter()
        .position(|m| m.is_human)
        .map(|i| i as i64)
        .unwrap_or(-1)
}

// ──────────────────────────────────────────────────────────────────────────
//  1) _should_compress_now (decisione di fase, PURA)
// ──────────────────────────────────────────────────────────────────────────

/// Config DB-driven del context management (`_load_ctx_mgmt_config`, mig 0199).
///
/// Regola G: NON e' letta dal DB qui dentro; il chiamante la passa esplicita.
/// I default valgono solo come riferimento (vedi `_CTX_MGMT_DEFAULTS` Python).
#[derive(Debug, Clone, PartialEq)]
pub struct CtxMgmtConfig {
    /// `compress_start_iter` (default 5): sotto questa iter non si comprime.
    pub compress_start_iter: i64,
    /// `compress_phase_boundaries` (default [5,10,20,50]).
    pub compress_phase_boundaries: Vec<i64>,
    /// `compress_phase_keep_recent` (default [8,5,3,2]).
    pub compress_phase_keep_recent: Vec<i64>,
    /// `compress_phase_max_chars` (default [2000,1000,500,150]).
    pub compress_phase_max_chars: Vec<i64>,
}

/// Parametri scelti per la fase corrente (output di [`should_compress_now`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressParams {
    /// `keep_recent`: messaggi recenti da preservare.
    pub keep_recent: i64,
    /// `max_content_chars`: cap del singolo tool_result per la fase.
    pub max_content_chars: i64,
}

/// `_should_compress_now`: decide se comprimere ORA e con quali parametri.
///
/// 1:1 con helpers.py:3048. Sceglie la fase la cui boundary e' la MASSIMA `<=
/// iteration` (scan finche' `iteration >= b`, poi break: l'`idx` resta all'ultima
/// boundary soddisfatta). Sotto `compress_start_iter` ritorna `(false, {0,0})`.
///
/// Indici: usa lo stesso `idx` su `keep_recent`/`max_chars` (le tre liste sono
/// allineate per fase; se piu' corte di `boundaries` il Python farebbe IndexError
/// — qui clampiamo l'indice alla lunghezza disponibile, fail-safe non osservabile
/// in pratica perche' le liste sono sempre allineate).
pub fn should_compress_now(
    iteration: i64,
    cfg: &CtxMgmtConfig,
) -> (bool, CompressParams) {
    if iteration < cfg.compress_start_iter {
        return (false, CompressParams { keep_recent: 0, max_content_chars: 0 });
    }
    let mut idx = 0usize;
    for (i, b) in cfg.compress_phase_boundaries.iter().enumerate() {
        if iteration >= *b {
            idx = i;
        } else {
            break;
        }
    }
    let keep_recent = *cfg
        .compress_phase_keep_recent
        .get(idx.min(cfg.compress_phase_keep_recent.len().saturating_sub(1)))
        .unwrap_or(&0);
    let max_content_chars = *cfg
        .compress_phase_max_chars
        .get(idx.min(cfg.compress_phase_max_chars.len().saturating_sub(1)))
        .unwrap_or(&0);
    (true, CompressParams { keep_recent, max_content_chars })
}

// ──────────────────────────────────────────────────────────────────────────
//  2) _dedup_tool_results_history (dedup per SIGNATURE tool_name+args)
// ──────────────────────────────────────────────────────────────────────────

/// `_tool_use_signature`: `sha256(tool_name + "|" + json(args, sort_keys=True))`,
/// primi 16 char hex. Bit-identica al Python: la canonicalizzazione args usa il
/// PUNTO UNICO [`py_json_dumps`] con [`SortKeys::Yes`] (regola L). Il `default=str`
/// del Python non scatta mai su args JSON puri.
fn tool_use_signature(tool_name: &str, args: &Value) -> String {
    let args_json = py_json_dumps(args, SortKeys::Yes);
    let payload = format!("{tool_name}|{args_json}");
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    let digest = hasher.finalize();
    let mut hex16 = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hex16.push_str(&format!("{byte:02x}"));
    }
    hex16
}

/// Estrae `(id, name, input)` da un blocco se e' un `tool_use` con id non vuoto.
fn tool_use_id_name_input(block: &Value) -> Option<(String, String, Value)> {
    let obj = block.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    // Python: str(block.get("id","") or ""); skip se vuoto.
    let tid = obj.get("id").and_then(Value::as_str).unwrap_or("");
    if tid.is_empty() {
        return None;
    }
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    // block.get("input", {}) or {} : input assente/null/falsy -> {}.
    let input = match obj.get("input") {
        Some(v) if !v.is_null() => v.clone(),
        _ => json!({}),
    };
    Some((tid.to_string(), name, input))
}

/// `_dedup_tool_results_history`: per ogni signature (tool_name+args) tiene solo
/// l'ULTIMO tool_result; i precedenti diventano placeholder che cita il piu'
/// recente. Preserva il `tool_use_id` (pairing Anthropic). 1:1 con helpers.py:3088.
pub fn dedup_tool_results_history(messages: &[HistoryMessage]) -> Vec<HistoryMessage> {
    use std::collections::HashMap;

    // Step 1: tool_use_id -> signature. Scansiona SIA i blocchi di `m.content`
    // (se lista) SIA quelli di `anthropic_content`.
    let mut id_to_sig: HashMap<String, String> = HashMap::new();
    for m in messages {
        if let Value::Array(blocks) = &m.content {
            for b in blocks {
                if let Some((tid, name, input)) = tool_use_id_name_input(b) {
                    id_to_sig.insert(tid, tool_use_signature(&name, &input));
                }
            }
        }
        if let Some(blocks) = m.anthropic_blocks() {
            for b in blocks {
                if let Some((tid, name, input)) = tool_use_id_name_input(b) {
                    id_to_sig.insert(tid, tool_use_signature(&name, &input));
                }
            }
        }
    }

    // Step 2: ultima posizione (mi, bi) di tool_result per signature.
    let mut last_pos: HashMap<String, (usize, usize)> = HashMap::new();
    for (mi, m) in messages.iter().enumerate() {
        let Some(blocks) = m.anthropic_blocks() else { continue };
        for (bi, block) in blocks.iter().enumerate() {
            let Some(obj) = block.as_object() else { continue };
            if obj.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let tid = obj.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
            if let Some(sig) = id_to_sig.get(tid) {
                last_pos.insert(sig.clone(), (mi, bi));
            }
        }
    }

    // Step 3: sostituisci i tool_result non-ultimi con placeholder.
    let mut out: Vec<HistoryMessage> = Vec::with_capacity(messages.len());
    for (mi, m) in messages.iter().enumerate() {
        let Some(blocks) = m.anthropic_blocks() else {
            out.push(m.clone());
            continue;
        };
        let mut changed = false;
        let mut new_blocks: Vec<Value> = Vec::with_capacity(blocks.len());
        for (bi, block) in blocks.iter().enumerate() {
            let is_tr = block
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(Value::as_str)
                == Some("tool_result");
            if !is_tr {
                new_blocks.push(block.clone());
                continue;
            }
            let tid = block
                .as_object()
                .and_then(|o| o.get("tool_use_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(sig) = id_to_sig.get(tid) else {
                new_blocks.push(block.clone());
                continue;
            };
            match last_pos.get(sig) {
                None => new_blocks.push(block.clone()),
                Some(last) if *last == (mi, bi) => new_blocks.push(block.clone()),
                Some(last) => {
                    new_blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tid,
                        "content": format!(
                            "[dedup: stesso tool con stessi args, vedi risultato \
piu' recente in msg #{}]",
                            last.0
                        ),
                    }));
                    changed = true;
                }
            }
        }
        if changed {
            out.push(HistoryMessage::rebuilt_human(m.content.clone(), new_blocks));
        } else {
            out.push(m.clone());
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────
//  Legacy dedup per CONTENT (BP11) — usato dalla modalita' legacy di compress
// ──────────────────────────────────────────────────────────────────────────

/// `_dedup_tool_results` (BP11, helpers.py:2831): dedup dei tool_result per
/// CONTENT (sha1 dei primi 256 char normalizzati), non per chiamata. Tiene solo
/// l'ultima copia; le precedenti diventano placeholder `[deduped: ...]`. Skip dei
/// content < 200 char. Pura.
pub fn dedup_tool_results(messages: &[HistoryMessage]) -> Vec<HistoryMessage> {
    use std::collections::HashMap;

    // Serializza il content di un tool_result come fa il Python: stringa diretta,
    // oppure (se lista di blocchi) concatena i `text` con spazio. `None` se il
    // content non e' ne' stringa ne' lista (il Python lascia il blocco intatto).
    fn serialized_content(block: &Value) -> Option<String> {
        let content = block.as_object()?.get("content")?;
        match content {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => {
                let joined = items
                    .iter()
                    .filter_map(|b| {
                        let o = b.as_object()?;
                        if o.get("type").and_then(Value::as_str) == Some("text") {
                            // str(b.get("text","")) — un text non-stringa non
                            // accade nel contratto; usiamo as_str.
                            Some(o.get("text").and_then(Value::as_str).unwrap_or("").to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(joined)
            }
            _ => None,
        }
    }

    fn content_hash16(serialized: &str) -> String {
        // normalized = content.strip()[:256]; sha1 hex[:16].
        let normalized: String = serialized.trim().chars().take(256).collect();
        let mut hasher = Sha1::new();
        hasher.update(normalized.as_bytes());
        let digest = hasher.finalize();
        let mut hex16 = String::with_capacity(16);
        for byte in digest.iter().take(8) {
            hex16.push_str(&format!("{byte:02x}"));
        }
        hex16
    }

    // Passata 1: hash -> ultima (mi, bi). Solo content >= 200 char.
    let mut last_indices: HashMap<String, (usize, usize)> = HashMap::new();
    for (mi, m) in messages.iter().enumerate() {
        let Some(blocks) = m.anthropic_blocks() else { continue };
        for (bi, block) in blocks.iter().enumerate() {
            if block.as_object().and_then(|o| o.get("type")).and_then(Value::as_str)
                != Some("tool_result")
            {
                continue;
            }
            let Some(serialized) = serialized_content(block) else { continue };
            // Passata 1 Python (helpers.py:2862-2872): serializza il content sia
            // se stringa sia se lista (" ".join dei text block) e registra in
            // last_indices solo se la serializzazione e' >=200 char. Identica alla
            // passata 2: niente asimmetria str/lista. `serialized_content` replica
            // gia' la serializzazione, quindi l'unica condizione di skip e' la soglia.
            if serialized.chars().count() < 200 {
                continue;
            }
            last_indices.insert(content_hash16(&serialized), (mi, bi));
        }
    }

    // Passata 2: sostituisci le non-ultime con placeholder. Stessa
    // serializzazione e stessa soglia 200 della passata 1 (str e lista trattate
    // identicamente, come nel Python helpers.py:2889-2904).
    let mut out: Vec<HistoryMessage> = Vec::with_capacity(messages.len());
    for (mi, m) in messages.iter().enumerate() {
        let Some(blocks) = m.anthropic_blocks() else {
            out.push(m.clone());
            continue;
        };
        let mut changed = false;
        let mut new_blocks: Vec<Value> = Vec::with_capacity(blocks.len());
        for (bi, block) in blocks.iter().enumerate() {
            if block.as_object().and_then(|o| o.get("type")).and_then(Value::as_str)
                != Some("tool_result")
            {
                new_blocks.push(block.clone());
                continue;
            }
            let Some(serialized) = serialized_content(block) else {
                // content ne' str ne' lista -> blocco intatto (Python: continue
                // dopo append).
                new_blocks.push(block.clone());
                continue;
            };
            if serialized.is_empty() || serialized.chars().count() < 200 {
                new_blocks.push(block.clone());
                continue;
            }
            let h = content_hash16(&serialized);
            let last = last_indices.get(&h).copied().unwrap_or((mi, bi));
            if (mi, bi) != last {
                let mut nb = block.clone();
                if let Some(o) = nb.as_object_mut() {
                    o.insert(
                        "content".to_string(),
                        Value::String(format!(
                            "[deduped: contenuto identico al tool_result piu' \
recente in msg #{}]",
                            last.0
                        )),
                    );
                }
                new_blocks.push(nb);
                changed = true;
            } else {
                new_blocks.push(block.clone());
            }
        }
        if changed {
            out.push(HistoryMessage::rebuilt_human(m.content.clone(), new_blocks));
        } else {
            out.push(m.clone());
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────
//  3) _drop_unused_base64_payloads
// ──────────────────────────────────────────────────────────────────────────

/// `_looks_like_base64`: stringa lunga (>= `min_len`, default 200) senza newline
/// nei primi `min_len` char, con >= 90% di char base64 su un campione (<= 4096).
/// 1:1 con helpers.py:3196 (regex `[A-Za-z0-9+/=]`).
pub fn looks_like_base64(s: &str, min_len: usize) -> bool {
    // Python usa len(s) (codepoint) per la soglia e s[:min_len] (codepoint).
    let total = s.chars().count();
    if total < min_len {
        return false;
    }
    // "\n" in s[:min_len] : controllo sui primi min_len codepoint.
    let head: String = s.chars().take(min_len).collect();
    if head.contains('\n') {
        return false;
    }
    // sample = s if len(s) <= 4096 else s[:4096] (codepoint).
    let sample: String = if total <= 4096 {
        s.to_string()
    } else {
        s.chars().take(4096).collect()
    };
    let sample_len = sample.chars().count();
    let valid = sample
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .count();
    // valid / max(len(sample), 1) >= 0.9
    (valid as f64) / (sample_len.max(1) as f64) >= 0.9
}

/// `_drop_unused_base64_payloads`: sostituisce i body base64 di tool_result
/// vecchi NON citati nei `max_age` messaggi successivi con un placeholder. Gli
/// ultimi `keep_recent` (default 2) restano intatti. 1:1 con helpers.py:3208.
///
/// Regola G: `max_age` arriva come parametro esplicito (nel Python None ->
/// `_load_ctx_mgmt_config()["drop_unused_base64_age"]`); il chiamante lo passa.
pub fn drop_unused_base64_payloads(
    messages: &[HistoryMessage],
    max_age: i64,
    keep_recent: usize,
) -> Vec<HistoryMessage> {
    if max_age <= 0 || messages.len() <= keep_recent {
        return messages.to_vec();
    }
    let boundary = messages.len() - keep_recent;

    // Testo cumulativo per messaggio (only-text): content str + blocchi text di
    // content-lista + anthropic_content[*].content se stringa. join(" ").
    let text_per_msg: Vec<String> = messages
        .iter()
        .map(|m| {
            let mut parts: Vec<String> = Vec::new();
            match &m.content {
                Value::String(s) => parts.push(s.clone()),
                Value::Array(blocks) => {
                    for b in blocks {
                        if let Some(o) = b.as_object() {
                            if o.get("type").and_then(Value::as_str) == Some("text") {
                                // str(b.get("text",""))
                                parts.push(
                                    o.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
            if let Some(blocks) = m.anthropic_blocks() {
                for b in blocks {
                    if let Some(o) = b.as_object() {
                        if let Some(Value::String(bc)) = o.get("content") {
                            parts.push(bc.clone());
                        }
                    }
                }
            }
            parts.join(" ")
        })
        .collect();

    let mut out: Vec<HistoryMessage> = Vec::with_capacity(messages.len());
    for (mi, m) in messages.iter().enumerate() {
        if mi >= boundary {
            out.push(m.clone());
            continue;
        }
        let Some(blocks) = m.anthropic_blocks() else {
            out.push(m.clone());
            continue;
        };
        let mut changed = false;
        let mut new_blocks: Vec<Value> = Vec::with_capacity(blocks.len());
        for block in blocks {
            let is_tr = block
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(Value::as_str)
                == Some("tool_result");
            if !is_tr {
                new_blocks.push(block.clone());
                continue;
            }
            // content deve essere stringa base64.
            let content = match block.as_object().and_then(|o| o.get("content")) {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    new_blocks.push(block.clone());
                    continue;
                }
            };
            if !looks_like_base64(&content, 200) {
                new_blocks.push(block.clone());
                continue;
            }
            // prefix = content[:16] (codepoint).
            let prefix: String = content.chars().take(16).collect();
            // window_hi = min(len, mi+1+max_age).
            let window_hi = messages.len().min(mi + 1 + max_age as usize);
            let mut cited = false;
            for item in text_per_msg.iter().take(window_hi).skip(mi + 1) {
                if item.contains(&prefix) {
                    cited = true;
                    break;
                }
            }
            if cited {
                new_blocks.push(block.clone());
                continue;
            }
            // orig_len = len(content) (codepoint).
            let orig_len = content.chars().count();
            // {**block, "content": placeholder} — preserva le altre chiavi.
            let mut nb = block.clone();
            if let Some(o) = nb.as_object_mut() {
                o.insert(
                    "content".to_string(),
                    Value::String(format!(
                        "[contenuto base64 originale di {orig_len} byte rimosso \
dalla history per ottimizzazione context. Se serve rileggilo con il tool \
originale.]"
                    )),
                );
            }
            new_blocks.push(nb);
            changed = true;
        }
        if changed {
            out.push(HistoryMessage::rebuilt_human(m.content.clone(), new_blocks));
        } else {
            out.push(m.clone());
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────
//  4) _compress_old_tool_results (con marker_fn pura iniettabile)
// ──────────────────────────────────────────────────────────────────────────

/// Marker "degraded" del Python quando l'offload RAG non e' disponibile
/// (`_compress_marker` con `offload is None`): `"\n[... compresso: N char
/// originali ...]"` dove `N = content.chars().count()`. PURO.
///
/// In produzione il chiamante passa un `marker_fn` che fa l'offload RAG (I/O,
/// TODO `EmbeddingStore`) e ritorna il marker con `ref`. Qui resta solo la forma
/// pura/degraded, usata anche dai golden (offload disabilitato).
pub fn degraded_marker(content: &str) -> String {
    format!("\n[... compresso: {} char originali ...]", content.chars().count())
}

/// `_compress_old_tool_results`: comprime i tool_result dei messaggi vecchi.
///
/// 1:1 con __init__.py:1261. Due modalita':
/// - LEGACY (`cutoff_index = None`): dedup preliminare ([`dedup_tool_results`]) +
///   boundary mobile `len-keep_recent`; i recenti compressi con soglia `2x`.
/// - A GENERAZIONI (`cutoff_index = Some`): comprime SOLO `i < cutoff`, niente
///   dedup, i recenti restano intatti.
///
/// `marker_fn`: callback PURA che produce il marker da appendere al troncato (in
/// produzione fa l'offload RAG; per la parte pura/golden si usa [`degraded_marker`]).
pub fn compress_old_tool_results<F>(
    messages: &[HistoryMessage],
    keep_recent: usize,
    max_content_chars: usize,
    cutoff_index: Option<usize>,
    marker_fn: &F,
) -> Vec<HistoryMessage>
where
    F: Fn(&str) -> String,
{
    // Determina la modalita' e il boundary.
    let (working, boundary) = match cutoff_index {
        None => {
            // Legacy: dedup su tutta la history, poi boundary mobile.
            let deduped = dedup_tool_results(messages);
            if deduped.len() <= keep_recent {
                return deduped;
            }
            let b = deduped.len() - keep_recent;
            (deduped, b)
        }
        Some(ci) => {
            // A generazioni: boundary = clamp(cutoff, 0, len). 0 -> no-op.
            let b = ci.min(messages.len());
            if b == 0 {
                return messages.to_vec();
            }
            (messages.to_vec(), b)
        }
    };

    let recent_threshold = max_content_chars * 2;
    let mut out: Vec<HistoryMessage> = Vec::with_capacity(working.len());

    for (i, m) in working.iter().enumerate() {
        if i >= boundary {
            // Oltre il boundary.
            if cutoff_index.is_some() {
                // A generazioni: intatti.
                out.push(m.clone());
                continue;
            }
            // Legacy: comprimi i recenti con soglia 2x.
            let Some(blocks) = m.anthropic_blocks() else {
                out.push(m.clone());
                continue;
            };
            let (new_blocks, changed) = compress_blocks(
                blocks,
                recent_threshold,
                recent_threshold / 2,
                200,
                marker_fn,
            );
            if changed {
                out.push(HistoryMessage::rebuilt_human(m.content.clone(), new_blocks));
            } else {
                out.push(m.clone());
            }
            continue;
        }

        // Sotto il boundary: comprimi a max_content_chars.
        let Some(blocks) = m.anthropic_blocks() else {
            out.push(m.clone());
            continue;
        };
        let (new_blocks, changed) = compress_blocks(
            blocks,
            max_content_chars,
            max_content_chars / 2,
            100,
            marker_fn,
        );
        if changed {
            out.push(HistoryMessage::rebuilt_human(m.content.clone(), new_blocks));
        } else {
            out.push(m.clone());
        }
    }
    out
}

/// Comprime i soli blocchi `tool_result` con content stringa piu' lungo di
/// `threshold`: tiene `max(half, floor)` char + `marker_fn(content)`. Replica il
/// blocco interno comune di `_compress_old_tool_results` (`kept = max(.../2,
/// floor)`; `content[:kept] + _compress_marker(content)`). Restituisce
/// `(new_blocks, changed)`.
fn compress_blocks<F>(
    blocks: &[Value],
    threshold: usize,
    half: usize,
    floor: usize,
    marker_fn: &F,
) -> (Vec<Value>, bool)
where
    F: Fn(&str) -> String,
{
    let mut changed = false;
    let mut new_blocks: Vec<Value> = Vec::with_capacity(blocks.len());
    for block in blocks {
        let obj = block.as_object();
        let is_tr =
            obj.and_then(|o| o.get("type")).and_then(Value::as_str) == Some("tool_result");
        if !is_tr {
            new_blocks.push(block.clone());
            continue;
        }
        // content stringa piu' lunga di threshold (codepoint).
        let content = match obj.and_then(|o| o.get("content")) {
            Some(Value::String(s)) => s,
            _ => {
                new_blocks.push(block.clone());
                continue;
            }
        };
        if content.chars().count() > threshold {
            let kept = half.max(floor);
            let head: String = content.chars().take(kept).collect();
            let marker = marker_fn(content);
            let mut nb = block.clone();
            if let Some(o) = nb.as_object_mut() {
                o.insert(
                    "content".to_string(),
                    Value::String(format!("{head}{marker}")),
                );
            }
            new_blocks.push(nb);
            changed = true;
        } else {
            new_blocks.push(block.clone());
        }
    }
    (new_blocks, changed)
}

// ──────────────────────────────────────────────────────────────────────────
//  5) _apply_token_brake (con token_estimator puro iniettabile)
// ──────────────────────────────────────────────────────────────────────────

/// Marker del troncamento aggressivo (`_AGGRESSIVE_TRUNC_MARKER`, __init__.py:1372).
pub const AGGRESSIVE_TRUNC_MARKER: &str = "[...troncato per limite contesto...]";

/// Config del freno token (sottoinsieme di [`CtxMgmtConfig`] usato qui).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBrakeConfig {
    /// `max_context_ratio` (default 0.70).
    pub max_context_ratio: f64,
    /// `aggressive_keep_recent` (default 3).
    pub aggressive_keep_recent: usize,
    /// `aggressive_max_chars` (default 200).
    pub aggressive_max_chars: usize,
}

/// `_apply_token_brake`: se la stima token `>= ratio*window`, comprime aggressivo
/// fino a scendere sotto soglia (max 5 passate) o a non aver piu' nulla; se ancora
/// `>= window`, cap hard (richiesta originale + ultimi 2). 1:1 con __init__.py:1496.
///
/// Confine I/O: il conteggio token (tiktoken, `_estimate_context_tokens`) e il
/// `window` (DB) sono FUORI; qui entrano come `token_estimator` (callback pura) e
/// `window`. La DECISIONE e i troncamenti sono puri.
///
/// Nota di parita': il Python avvolge tutto in try/except che, in caso di
/// eccezione, ritorna i messaggi invariati. Qui le operazioni sono total (nessuna
/// eccezione possibile), quindi quel ramo e' irraggiungibile e non e' replicato.
pub fn apply_token_brake<E>(
    messages: &[HistoryMessage],
    window: i64,
    cfg: &TokenBrakeConfig,
    token_estimator: &E,
) -> Vec<HistoryMessage>
where
    E: Fn(&[HistoryMessage]) -> i64,
{
    let threshold_tokens = (window as f64 * cfg.max_context_ratio) as i64;
    let est_tokens = token_estimator(messages);
    if est_tokens < threshold_tokens {
        return messages.to_vec();
    }

    let mut work = messages.to_vec();
    let max_passes = 5;
    for _ in 0..max_passes {
        let (next, changed) = compress_aggressive_token_based(
            &work,
            cfg.aggressive_keep_recent,
            cfg.aggressive_max_chars,
        );
        work = next;
        let est = token_estimator(&work);
        if est < threshold_tokens || !changed {
            break;
        }
    }

    // Cap di sicurezza hard: se ancora >= window pieno, riduci all'osso.
    let est_after = token_estimator(&work);
    if est_after >= window {
        let first_human = first_human_index(&work);
        let n = work.len();
        let mut keep_idx: std::collections::BTreeSet<usize> = (n.saturating_sub(2)..n).collect();
        if first_human >= 0 {
            keep_idx.insert(first_human as usize);
        }
        work = work
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep_idx.contains(i))
            .map(|(_, m)| m)
            .collect();
    }
    work
}

/// `_compress_aggressive_token_based`: tronca TUTTI i messaggi vecchi (anche
/// assistant) a `max_content_chars`, preservando il PRIMO human, i summary, e gli
/// ultimi `keep_recent`. 1:1 con __init__.py:1464. Ritorna `(messages, changed)`.
fn compress_aggressive_token_based(
    messages: &[HistoryMessage],
    keep_recent: usize,
    max_content_chars: usize,
) -> (Vec<HistoryMessage>, bool) {
    let n = messages.len();
    if n <= keep_recent + 1 {
        return (messages.to_vec(), false);
    }
    let first_human = first_human_index(messages);
    let boundary = n - keep_recent;
    let mut out: Vec<HistoryMessage> = Vec::with_capacity(n);
    let mut any_changed = false;
    for (i, m) in messages.iter().enumerate() {
        if i >= boundary || (first_human >= 0 && i as i64 == first_human) || m.is_summary() {
            out.push(m.clone());
            continue;
        }
        let (new_m, changed) = truncate_message_content(m, max_content_chars);
        out.push(new_m);
        any_changed = any_changed || changed;
    }
    (out, any_changed)
}

/// `_truncate_message_content`: tronca aggressivamente un messaggio (__init__.py:1375).
///
/// Tronca i blocchi `text`/`tool_result` (su `text`/`content`), l'`input` dei
/// `tool_use` se la sua serializzazione JSON e' troppo lunga (mantenendo id/name),
/// e il `content` stringa diretto. Usa `degraded_marker` (parte pura del
/// `_compress_marker`) + `AGGRESSIVE_TRUNC_MARKER`. Ritorna `(msg, changed)`.
fn truncate_message_content(m: &HistoryMessage, max_content_chars: usize) -> (HistoryMessage, bool) {
    if let Some(blocks) = m.anthropic_blocks() {
        let mut changed = false;
        let mut new_blocks: Vec<Value> = Vec::with_capacity(blocks.len());
        for block in blocks {
            let Some(obj) = block.as_object() else {
                new_blocks.push(block.clone());
                continue;
            };
            let btype = obj.get("type").and_then(Value::as_str).unwrap_or("");
            match btype {
                "text" | "tool_result" => {
                    // I text portano il testo in "text"; i tool_result in "content".
                    // Alcuni text usano "content".
                    let (content_key, content): (&str, String) = if btype == "text" {
                        match obj.get("text") {
                            Some(Value::String(s)) => ("text", s.clone()),
                            _ => (
                                "content",
                                obj.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
                            ),
                        }
                    } else {
                        (
                            "content",
                            obj.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
                        )
                    };
                    // Il Python tronca solo se isinstance(content, str): se la
                    // chiave non era stringa, content="" -> len 0 <= max, no-op.
                    let is_str = if btype == "text" {
                        matches!(obj.get("text"), Some(Value::String(_)))
                            || matches!(obj.get("content"), Some(Value::String(_)))
                    } else {
                        matches!(obj.get("content"), Some(Value::String(_)))
                    };
                    if is_str && content.chars().count() > max_content_chars {
                        let floor = max_content_chars
                            .saturating_sub(AGGRESSIVE_TRUNC_MARKER.chars().count())
                            .max(50);
                        let head: String = content.chars().take(floor).collect();
                        let truncated =
                            format!("{head}{}{AGGRESSIVE_TRUNC_MARKER}", degraded_marker(&content));
                        let mut nb = block.clone();
                        if let Some(o) = nb.as_object_mut() {
                            o.insert(content_key.to_string(), Value::String(truncated));
                        }
                        new_blocks.push(nb);
                        changed = true;
                    } else {
                        new_blocks.push(block.clone());
                    }
                }
                "tool_use" => {
                    let tin = obj.get("input").cloned().unwrap_or(Value::Null);
                    // json.dumps(tin, ensure_ascii=False, default=str). Senza
                    // sort_keys (default Python). Riuso py_json_dumps(SortKeys::No).
                    let tin_str = py_json_dumps(&tin, SortKeys::No);
                    if tin_str.chars().count() > max_content_chars {
                        let head: String = tin_str.chars().take(max_content_chars).collect();
                        let mut nb = block.clone();
                        if let Some(o) = nb.as_object_mut() {
                            o.insert(
                                "input".to_string(),
                                json!({ "_truncated": format!("{head}{AGGRESSIVE_TRUNC_MARKER}") }),
                            );
                        }
                        new_blocks.push(nb);
                        changed = true;
                    } else {
                        new_blocks.push(block.clone());
                    }
                }
                _ => new_blocks.push(block.clone()),
            }
        }
        if changed {
            // Python: prova cls(...) (stesso tipo), fallback HumanMessage; in
            // entrambi i casi additional_kwargs={anthropic_content}. Il tipo
            // non e' osservabile sulle trasformazioni successive (solo human/
            // summary/first contano, e qui non e' first/summary), quindi
            // rebuilt_human e' fedele al risultato.
            let mut nm = HistoryMessage::rebuilt_human(m.content.clone(), new_blocks);
            // cls(content, additional_kwargs={anthropic_content}) NON forza
            // is_human se cls e' AIMessage: ma il tipo non incide oltre. Manteniamo
            // is_human dal messaggio originale per non alterare first_human futuri.
            nm.is_human = m.is_human;
            return (nm, true);
        }
        return (m.clone(), false);
    }

    // content stringa diretto (assistant senza blocchi).
    if let Value::String(content) = &m.content {
        if content.chars().count() > max_content_chars {
            let floor = max_content_chars
                .saturating_sub(AGGRESSIVE_TRUNC_MARKER.chars().count())
                .max(50);
            let head: String = content.chars().take(floor).collect();
            let new_content =
                format!("{head}{}{AGGRESSIVE_TRUNC_MARKER}", degraded_marker(content));
            let mut nm = m.clone();
            nm.content = Value::String(new_content);
            return (nm, true);
        }
    }
    (m.clone(), false)
}

// ──────────────────────────────────────────────────────────────────────────
//  6) _inject_language_reminder (PURA dato enabled+text)
// ──────────────────────────────────────────────────────────────────────────

/// `_inject_language_reminder` (helpers.py:818): inietta il reminder lingua in
/// TESTA al system (idempotente via [`LANG_REMINDER_MARKER`]) e lo ribadisce in
/// CODA. I messaggi NON vengono mai toccati (P3 prefix stabile). Ritorna il nuovo
/// `system_text`. No-op se `enabled=false` o `reminder_text` vuoto, o se il marker
/// e' gia' presente.
///
/// Regola G: `enabled`/`reminder_text` arrivano come parametri (nel Python da
/// `_load_language_reminder`, DB cache 60s); la funzione e' pura.
pub fn inject_language_reminder(system_text: &str, enabled: bool, reminder_text: &str) -> String {
    if !enabled || reminder_text.is_empty() {
        return system_text.to_string();
    }
    if system_text.contains(LANG_REMINDER_MARKER) {
        return system_text.to_string();
    }
    let lang_block = format!("### LINGUA RISPOSTA OBBLIGATORIA ###\n{reminder_text}");
    format!("{LANG_REMINDER_MARKER}\n{lang_block}\n\n{system_text}\n\n{lang_block}")
}

// ──────────────────────────────────────────────────────────────────────────
//  7) _inject_turn_focus (PURA, riusa marker + primitiva, regola L)
// ──────────────────────────────────────────────────────────────────────────

/// `_inject_turn_focus` (helpers.py:930): antepone la directive di focus al
/// system (idempotente via [`TURN_FOCUS_MARKER`], RIUSATO da [`super::turn_focus`]).
/// No-op se `directive` vuota o marker gia' presente. Ritorna il nuovo `system_text`.
///
/// Regola L: il marker e la costruzione della directive ([`build_turn_focus_directive`])
/// sono i punti unici gia' portati; qui si fa SOLO l'iniezione idempotente.
pub fn inject_turn_focus(system_text: &str, directive: &str) -> String {
    if directive.is_empty() {
        return system_text.to_string();
    }
    if system_text.contains(TURN_FOCUS_MARKER) {
        return system_text.to_string();
    }
    format!("{TURN_FOCUS_MARKER}\n{directive}\n\n{system_text}")
}

// ──────────────────────────────────────────────────────────────────────────
//  8a) _inject_verification_directive (PURA dato detection+enabled+text)
// ──────────────────────────────────────────────────────────────────────────

/// `_inject_verification_directive` (helpers.py:1295): se l'utente ha chiesto una
/// verifica (`detected`) e la direttiva e' abilitata, la appende in CODA al system
/// (idempotente via [`VERIFY_DIRECTIVE_MARKER`]). Ritorna il nuovo `system_text`.
///
/// Regola G: il detection lessicale (`_detect_verification_request`, keyword) e il
/// caricamento DB (`_load_verification_directive`) restano FUORI; entrano come
/// `detected`/`enabled`/`directive`. La funzione e' pura.
pub fn inject_verification_directive(
    system_text: &str,
    detected: bool,
    enabled: bool,
    directive: &str,
) -> String {
    if !detected {
        return system_text.to_string();
    }
    if !enabled || directive.is_empty() {
        return system_text.to_string();
    }
    if system_text.contains(VERIFY_DIRECTIVE_MARKER) {
        return system_text.to_string();
    }
    let block = format!("### AUTO-VERIFICA RICHIESTA DALL'UTENTE ###\n{directive}");
    format!("{system_text}\n\n{VERIFY_DIRECTIVE_MARKER}\n{block}")
}

// ──────────────────────────────────────────────────────────────────────────
//  8b) _inject_forced_rag_reminder (PURA dato ratio+text)
// ──────────────────────────────────────────────────────────────────────────

/// `_inject_forced_rag_reminder` (helpers.py:727): se `est_tokens >= ratio*window`
/// appende in coda ai messaggi un `HumanMessage` dedicato col reminder RAG (una
/// sola volta, idempotente sul marker negli ultimi 8 messaggi). Il system NON
/// viene toccato (P3). Ritorna `(messages, system_text)`.
///
/// Regola G: `ratio`/`reminder_text` arrivano come parametri (nel Python da
/// `_load_forced_rag_reminder`, DB); `est_tokens`/`window` da tiktoken/catalog
/// (I/O) ma qui sono solo numeri di confronto.
pub fn inject_forced_rag_reminder(
    messages: &[HistoryMessage],
    system_text: &str,
    est_tokens: i64,
    window: i64,
    ratio: f64,
    reminder_text: &str,
) -> (Vec<HistoryMessage>, String) {
    if window <= 0 || est_tokens <= 0 {
        return (messages.to_vec(), system_text.to_string());
    }
    if ratio <= 0.0 || reminder_text.is_empty() {
        return (messages.to_vec(), system_text.to_string());
    }
    let threshold = (window as f64 * ratio) as i64;
    if est_tokens < threshold {
        return (messages.to_vec(), system_text.to_string());
    }
    // Idempotenza: marker presente nel content (stringa) degli ultimi 8 messaggi.
    let tail_start = messages.len().saturating_sub(8);
    for m in &messages[tail_start..] {
        if let Value::String(s) = &m.content {
            if s.contains(RAG_REMINDER_MARKER) {
                return (messages.to_vec(), system_text.to_string());
            }
        }
    }
    let reminder = HistoryMessage {
        is_human: true,
        content: Value::String(format!(
            "{RAG_REMINDER_MARKER} ### RECUPERO ON-DEMAND DEL CONTESTO ###\n{reminder_text}"
        )),
        anthropic_content: Value::Null,
        nexus_summary: false,
        rolling_summary: false,
    };
    let mut out = messages.to_vec();
    out.push(reminder);
    (out, system_text.to_string())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod golden;
