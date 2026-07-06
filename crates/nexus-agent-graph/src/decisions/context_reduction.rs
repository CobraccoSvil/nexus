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
    /// `m.type == "tool"` (`ToolMessage` Python). Distingue un risultato di tool
    /// da un `HumanMessage`/`AIMessage`: serve a ricostruire il `role="tool"` del
    /// messaggio wire (continuita' tool_use/tool_result, bug 2026-06-26). I
    /// messaggi COMPRESSI diventano `HumanMessage` (vedi
    /// [`HistoryMessage::rebuilt_human`]) e perdono questo flag, come nel Python:
    /// i loro tool_use/tool_result sono gia' degradati a sintesi inline (nessuna
    /// coppia da referenziare).
    #[serde(default)]
    pub is_tool: bool,
    /// `tool_call_id` del `ToolMessage` (round-trip col `tool_use.id` dell'assistant
    /// che lo ha richiesto). `None` per tutti i ruoli != tool. Azzerato dalla
    /// compressione (`rebuilt_human`).
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Reasoning (`reasoning_content`) di un turno `assistant` in thinking mode
    /// (DeepSeek), preservato attraverso la riduzione di contesto per poterlo
    /// RI-PASSARE all'API (vincolo HTTP 400). `None` per i ruoli != assistant e
    /// per i turni senza reasoning. La compressione (`rebuilt_human`) lo azzera:
    /// un messaggio degradato a sintesi non porta piu' il pensiero originale.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` (Anthropic) del turno `assistant`,
    /// preservata attraverso la riduzione di contesto per il round-trip (HTTP 400
    /// senza). Gemella per-messaggio del `reasoning`; azzerata dalla compressione
    /// (`rebuilt_human`). `None` per gli altri ruoli/provider.
    #[serde(default)]
    pub thinking_signature: Option<String>,
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
            // Un messaggio compresso diventa `HumanMessage`: perde il ruolo `tool`
            // e l'id (i suoi tool_use/tool_result sono gia' degradati a sintesi
            // inline, non c'e' piu' una coppia da referenziare). Coerente col Python.
            is_tool: false,
            tool_call_id: None,
            // Il messaggio compresso e' un HumanMessage di sintesi: non porta piu'
            // il reasoning originale (vincolo round-trip DeepSeek non applicabile).
            reasoning: None,
            thinking_signature: None,
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

// ──────────────────────────────────────────────────────────────────────────
//  5b) hard cap post-brake (ADR 0016 fase D2)
// ──────────────────────────────────────────────────────────────────────────

/// ADR 0016 D2: true se, DOPO upscale+brake, la stima resta oltre `ratio*window`.
///
/// `window <= 0` o `hard_cap_ratio <= 0` disattivano il gate (default sicuro a
/// config assente: il run procede come oggi, nessun falso positivo da DB down).
pub fn check_hard_cap(est_tokens: i64, window: i64, hard_cap_ratio: f64) -> bool {
    if window <= 0 || hard_cap_ratio <= 0.0 {
        return false;
    }
    est_tokens >= (window as f64 * hard_cap_ratio) as i64
}

/// Punto unico di sostituzione dei placeholder del template
/// `system.context_overflow` (`%ESTIMATED_TOKENS%` / `%MAX_WINDOW%`).
///
/// Il testo redazionale vive SOLO nel DB (regola G): qui nessun default umano.
pub fn render_overflow_message(template: &str, est_tokens: i64, window: i64) -> String {
    template
        .replace("%ESTIMATED_TOKENS%", &est_tokens.to_string())
        .replace("%MAX_WINDOW%", &window.to_string())
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
        ..Default::default()
    };
    let mut out = messages.to_vec();
    out.push(reminder);
    (out, system_text.to_string())
}

// ──────────────────────────────────────────────────────────────────────────
//  9) ROLLING SUMMARY (parte PURA): cutoff + serializzazione + applicazione
// ──────────────────────────────────────────────────────────────────────────
//
// Il SUMMARIZER vero (chiamata LLM al modello economico) e' I/O e vive dietro il
// trait [`crate::runtime::ports::SummaryStore`]. Qui restano le tre primitive
// PURE (regola L): DECIDERE dove tagliare la history (`select_rolling_summary_cutoff`),
// SERIALIZZARE il prefisso da riassumere (`serialize_prefix_for_summary`) e
// APPLICARE il riassunto sostituendo il prefisso con un solo messaggio
// (`apply_rolling_summary`). Il nodo executor (call site) le orchestra: cutoff ->
// serialize -> SummaryStore.summarize (I/O) -> apply. Su guasto LLM il nodo
// degrada lasciando la history invariata (compress/token_brake fanno il resto).

/// Soglia minima di messaggi nel prefisso sotto la quale NON vale la pena
/// chiamare l'LLM summarizer (il costo della chiamata supera il risparmio). Un
/// prefisso con < 2 messaggi da riassumere fa ritornare `None` al cutoff.
pub const ROLLING_SUMMARY_MIN_PREFIX: usize = 2;

/// `true` se il messaggio aprirebbe il suffisso con un `tool_result` ORFANO,
/// cioe' un risultato di tool il cui `tool_use` corrispondente finirebbe nel
/// prefisso riassunto (rompendo il pairing tool_use/tool_result -> HTTP 400).
///
/// Due forme di tool_result nella history (vedi `message_to_history`):
///   - `Message::Tool` -> `is_tool = true` (role=tool wire, id in `tool_call_id`);
///   - un `HumanMessage` che porta blocchi `tool_result` in `anthropic_content`
///     (forma Anthropic inline) il cui PRIMO blocco e' un `tool_result`.
fn opens_with_tool_result(m: &HistoryMessage) -> bool {
    if m.is_tool {
        return true;
    }
    // Forma inline: anthropic_content che inizia con un blocco tool_result e NON
    // contiene un tool_use proprio (un turno misto user+nuovo tool_use non e'
    // orfano). Conservativo: se il PRIMO blocco e' tool_result, lo trattiamo come
    // apertura orfana (il suo tool_use sta in un assistant precedente).
    match m.anthropic_blocks().and_then(|b| b.first()) {
        Some(first) => {
            first
                .as_object()
                .and_then(|o| o.get("type"))
                .and_then(Value::as_str)
                == Some("tool_result")
        }
        None => false,
    }
}

/// DECIDE il punto di taglio (cutoff) per il rolling summary: i messaggi
/// `hist[0..cutoff]` vengono riassunti, `hist[cutoff..]` restano intatti.
///
/// - Cutoff base = `len - keep_recent`. Se `<= 0` -> `None` (niente da riassumere).
/// - PAIRING tool_use/tool_result (vincolo non negoziabile): se `hist[cutoff]`
///   aprirebbe il suffisso con un `tool_result` orfano (il cui `tool_use` finisce
///   nel prefisso), INCREMENTA il cutoff finche' il primo messaggio del suffisso
///   non e' piu' un tool_result. Cosi' i tool_result iniziali del suffisso vengono
///   ASSORBITI nel prefisso riassunto e il suffisso parte da un messaggio "pulito".
/// - Se dopo l'aggiustamento `cutoff >= len` -> `None` (nessun suffisso residuo).
/// - Se il prefisso `hist[0..cutoff]` e' GIA' tutto messaggi summary -> `None`
///   (niente di nuovo da riassumere, evita di riassumere un riassunto).
/// - Se il prefisso ha meno di [`ROLLING_SUMMARY_MIN_PREFIX`] messaggi -> `None`.
///
/// PURA: nessuna chiamata LLM/DB. `keep_recent` arriva dal call site (config DB).
pub fn select_rolling_summary_cutoff(hist: &[HistoryMessage], keep_recent: i64) -> Option<usize> {
    let len = hist.len();
    let base = len as i64 - keep_recent;
    if base <= 0 {
        return None;
    }
    let mut cutoff = base as usize;
    // Aggiusta in avanti finche' il suffisso non parte con un tool_result orfano.
    while cutoff < len && opens_with_tool_result(&hist[cutoff]) {
        cutoff += 1;
    }
    if cutoff >= len {
        return None;
    }
    if cutoff < ROLLING_SUMMARY_MIN_PREFIX {
        return None;
    }
    // Niente da riassumere se il prefisso e' gia' tutto sintesi.
    if hist[..cutoff].iter().all(HistoryMessage::is_summary) {
        return None;
    }
    Some(cutoff)
}

/// SERIALIZZA il prefisso `hist[0..cutoff]` in TESTO leggibile per il summarizer
/// LLM (non JSON): una riga `[ruolo]: contenuto` per messaggio. Il ruolo deriva
/// dai flag (`is_human` -> human, `is_tool` -> tool, altrimenti assistant). Il
/// contenuto estrae il testo da `content` (stringa o blocchi `text`) e annota i
/// `tool_use` (`<tool nome(args)>`) e i `tool_result` (`<tool_result: ...>`) in
/// forma sintetica, cosi' il modello vede sia il dialogo sia le azioni.
///
/// PURA: nessun I/O. Robusta su content opaco (fallback a stringa JSON compatta).
pub fn serialize_prefix_for_summary(hist: &[HistoryMessage], cutoff: usize) -> String {
    let upper = cutoff.min(hist.len());
    let mut out = String::new();
    for m in &hist[..upper] {
        let role = if m.is_human {
            "human"
        } else if m.is_tool {
            "tool"
        } else {
            "assistant"
        };
        let body = serialize_message_body(m);
        if body.trim().is_empty() {
            continue;
        }
        out.push('[');
        out.push_str(role);
        out.push_str("]: ");
        out.push_str(body.trim());
        out.push('\n');
    }
    out
}

/// Estrae il corpo testuale di un messaggio per [`serialize_prefix_for_summary`]:
/// testo da `content`, piu' annotazioni sintetiche dei blocchi `tool_use`/
/// `tool_result` (sia in `content` se lista, sia in `anthropic_content`).
fn serialize_message_body(m: &HistoryMessage) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 1) Testo del content.
    match &m.content {
        Value::String(s) => {
            if !s.trim().is_empty() {
                parts.push(s.clone());
            }
        }
        Value::Array(blocks) => parts.extend(serialize_blocks(blocks)),
        Value::Null => {}
        other => parts.push(compact_json(other)),
    }

    // 2) Blocchi anthropic_content (tool_use/tool_result/text).
    if let Some(blocks) = m.anthropic_blocks() {
        parts.extend(serialize_blocks(blocks));
    }

    parts.join(" ")
}

/// Annota i blocchi Anthropic-style in forma testuale sintetica.
fn serialize_blocks(blocks: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        let Some(obj) = b.as_object() else { continue };
        match obj.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = obj.get("text").and_then(Value::as_str) {
                    if !t.trim().is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
            Some("tool_use") => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("?");
                let args = obj
                    .get("input")
                    .map(compact_json)
                    .unwrap_or_default();
                out.push(format!("<tool {name}({args})>"));
            }
            Some("tool_result") => {
                let content = obj
                    .get("content")
                    .map(|c| match c {
                        Value::String(s) => s.clone(),
                        other => compact_json(other),
                    })
                    .unwrap_or_default();
                out.push(format!("<tool_result: {content}>"));
            }
            _ => {}
        }
    }
    out
}

/// Serializzazione JSON compatta best-effort (fallback su content opaco). Mai
/// panica: su errore ritorna stringa vuota.
fn compact_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// APPLICA il rolling summary: sostituisce `hist[0..cutoff]` con UN SOLO
/// `HumanMessage` di sintesi e mantiene `hist[cutoff..]` INVARIATO.
///
/// Il messaggio di sintesi:
///   - `content = "[RIASSUNTO conversazione precedente]\n{summary_text}"`;
///   - `is_human = true` (cosi' il primo messaggio della history resta human);
///   - `rolling_summary = true` (riconosciuto da [`HistoryMessage::is_summary`],
///     preservato dalle riduzioni successive);
///   - tutti gli altri campi al default (no tool, no reasoning).
///
/// Il primo messaggio risultante e' human e il suffisso e' intatto: il pairing
/// tool_use/tool_result del suffisso resta valido (il cutoff lo garantisce via
/// [`select_rolling_summary_cutoff`]). PURA: nessun I/O.
pub fn apply_rolling_summary(
    hist: &[HistoryMessage],
    cutoff: usize,
    summary_text: &str,
) -> Vec<HistoryMessage> {
    let upper = cutoff.min(hist.len());
    let summary = HistoryMessage {
        is_human: true,
        content: Value::String(format!(
            "[RIASSUNTO conversazione precedente]\n{summary_text}"
        )),
        anthropic_content: Value::Null,
        nexus_summary: false,
        rolling_summary: true,
        is_tool: false,
        tool_call_id: None,
        reasoning: None,
        thinking_signature: None,
    };
    let mut out: Vec<HistoryMessage> = Vec::with_capacity(hist.len() - upper + 1);
    out.push(summary);
    out.extend_from_slice(&hist[upper..]);
    out
}

// ──────────────────────────────────────────────────────────────────────────
//  10) CONTINUITY-TRIM (parte PURA): coseno + selezione atomi + decisione + apply
// ──────────────────────────────────────────────────────────────────────────
//
// Compressione SEMANTICA del prefisso vecchio: invece del troncamento POSIZIONALE,
// scarta interi "atomi" (turno assistant + i suoi tool_result) semanticamente
// IRRILEVANTI al FOCUS del turno corrente. L'EMBEDDING (I/O) vive dietro il trait
// [`crate::runtime::ports::EmbeddingStore`]; qui restano le primitive PURE (regola
// L): FOCUS -> CANDIDATI -> DECISIONE (coseno) -> APPLY. Su guasto embedder il nodo
// salta il trim (history invariata) e compress/token_brake fanno il resto.
//
// SICUREZZA PAIRING (vincolo non negoziabile, HTTP 400): un atomo droppabile e'
// BILANCIATO (contiene sia il tool_use sia tutti i suoi tool_result), cosi'
// rimuoverlo interamente non lascia mai un tool_result orfano. I messaggi human
// (intento utente) e i riassunti non sono mai candidati.

/// Similarita' coseno fra due vettori. `0.0` (nessuna similarita' definibile) se un
/// vettore e' vuoto, di lunghezza diversa dall'altro, o a norma nulla. PURA.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// `true` se il messaggio porta almeno un blocco `tool_use` (in `anthropic_content`):
/// e' l'apertura di un turno assistant con chiamata tool (la cui coppia sono i
/// tool_result successivi). PURA.
fn message_has_tool_use(m: &HistoryMessage) -> bool {
    m.anthropic_blocks()
        .map(|blocks| {
            blocks.iter().any(|b| {
                b.as_object().and_then(|o| o.get("type")).and_then(Value::as_str)
                    == Some("tool_use")
            })
        })
        .unwrap_or(false)
}

/// Un ATOMO candidato al continuity-trim: gli indici (contigui) che lo compongono
/// nella history e il testo serializzato per l'embedding. Droppare TUTTI gli indici
/// insieme preserva il pairing (l'atomo e' bilanciato).
#[derive(Debug, Clone, PartialEq)]
pub struct ContinuityCandidate {
    /// Indici (nella history) che compongono l'atomo (turno assistant + tool_result).
    pub indices: Vec<usize>,
    /// Testo serializzato dell'atomo (riusa [`serialize_message_body`]), per l'embed.
    pub text: String,
}

/// FOCUS del turno per il continuity-trim: testo dell'ULTIMO messaggio human della
/// history (l'intento corrente dell'utente). Riusa [`serialize_message_body`]. Vuoto
/// se non c'e' alcun human. PURA.
pub fn continuity_focus_text(hist: &[HistoryMessage]) -> String {
    hist.iter()
        .rev()
        .find(|m| m.is_human)
        .map(serialize_message_body)
        .unwrap_or_default()
}

/// Serializza gli indici `[start, end)` come testo unico per l'embedding dell'atomo.
fn serialize_atom(hist: &[HistoryMessage], start: usize, end: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in &hist[start..end.min(hist.len())] {
        let body = serialize_message_body(m);
        if !body.trim().is_empty() {
            parts.push(body);
        }
    }
    parts.join(" ")
}

/// SELEZIONA gli atomi candidati al continuity-trim nel prefisso vecchio
/// `hist[0..len-keep_recent]`. Un atomo e' un turno assistant con tool_use + i suoi
/// tool_result contigui (bilanciato) OPPURE un assistant testuale standalone.
/// ESCLUSI (mai candidati): messaggi human (ancore intento), riassunti, e ogni
/// atomo con tool_use che tocca il confine `keep_recent` (i cui tool_result
/// potrebbero cadere nella coda preservata -> rischio orfano). PURA.
pub fn select_continuity_trim_candidates(
    hist: &[HistoryMessage],
    keep_recent: i64,
) -> Vec<ContinuityCandidate> {
    let len = hist.len();
    let prefix_end = (len as i64 - keep_recent.max(0)).max(0) as usize;
    if prefix_end == 0 {
        return Vec::new();
    }
    let mut out: Vec<ContinuityCandidate> = Vec::new();
    let mut i = 0usize;
    while i < prefix_end {
        let m = &hist[i];
        // Ancore non droppabili: human (intento) e riassunti.
        if m.is_human || m.is_summary() {
            i += 1;
            continue;
        }
        if message_has_tool_use(m) {
            // Atomo = assistant(tool_use) + tool_result contigui che rispondono.
            let mut j = i + 1;
            while j < prefix_end && (hist[j].is_tool || opens_with_tool_result(&hist[j])) {
                j += 1;
            }
            // Droppabile solo se l'atomo e' CHIUSO prima del confine keep_recent: se
            // il run di tool_result tocca prefix_end i risultati potrebbero
            // continuare nella coda preservata -> orfano. Conservativo: salta.
            if j < prefix_end {
                out.push(ContinuityCandidate {
                    indices: (i..j).collect(),
                    text: serialize_atom(hist, i, j),
                });
            }
            i = j;
        } else if m.is_tool || opens_with_tool_result(m) {
            // tool_result la cui coppia sta in un atomo precedente gia' consumato o
            // fuori prefisso: non droppabile isolatamente (rischio orfano).
            i += 1;
        } else {
            // Assistant testuale standalone: atomo droppabile singolo.
            let text = serialize_message_body(m);
            if !text.trim().is_empty() {
                out.push(ContinuityCandidate { indices: vec![i], text });
            }
            i += 1;
        }
    }
    out
}

/// DECIDE quali INDICI della history scartare: per ogni candidato calcola il coseno
/// del suo vettore vs `focus`; scarta i candidati sotto `min_score` (meno rilevanti
/// prima), fino al cap `max_drop_msgs` messaggi totali. `cand_vecs[k]` e' il vettore
/// del candidato `candidates[k]` (stesso ordine). Ritorna gli indici ORDINATI e
/// unici. PURA. Cap/soglia arrivano dal call site (config DB, regola G).
pub fn decide_continuity_drops(
    focus: &[f32],
    cand_vecs: &[Vec<f32>],
    candidates: &[ContinuityCandidate],
    min_score: f32,
    max_drop_msgs: usize,
) -> Vec<usize> {
    if focus.is_empty() || max_drop_msgs == 0 {
        return Vec::new();
    }
    // (score, k) per i soli candidati SOTTO soglia (irrilevanti al focus).
    let mut ranked: Vec<(f32, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(k, _)| {
            let v = cand_vecs.get(k)?;
            let score = cosine_similarity(focus, v);
            (score < min_score).then_some((score, k))
        })
        .collect();
    // Meno rilevanti (score piu' basso) prima; tiebreak per indice (determinismo).
    ranked.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    let mut drops: Vec<usize> = Vec::new();
    for (_, k) in ranked {
        let atom = &candidates[k];
        if drops.len() + atom.indices.len() > max_drop_msgs {
            continue; // salterebbe il cap: prova gli atomi successivi piu' piccoli.
        }
        drops.extend_from_slice(&atom.indices);
        if drops.len() >= max_drop_msgs {
            break;
        }
    }
    drops.sort_unstable();
    drops.dedup();
    drops
}

/// APPLICA il continuity-trim: ritorna la history senza gli indici in `drop_indices`
/// (preserva l'ordine dei restanti). Gli atomi rimossi sono bilanciati -> nessun
/// tool_result orfano. PURA. Indici fuori range ignorati.
pub fn apply_continuity_trim(
    hist: &[HistoryMessage],
    drop_indices: &[usize],
) -> Vec<HistoryMessage> {
    if drop_indices.is_empty() {
        return hist.to_vec();
    }
    let drop: std::collections::HashSet<usize> = drop_indices.iter().copied().collect();
    hist.iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, m)| m.clone())
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────
//  11) OFFLOAD ELIGIBILITY (parte PURA): contenuti tool_result che saranno compressi
// ──────────────────────────────────────────────────────────────────────────

/// Enumera i CONTENUTI dei tool_result che [`compress_old_tool_results`] comprimera'
/// nella modalita' A GENERAZIONI (`cutoff_index = Some`): i tool_result sotto il
/// `cutoff` con content stringa piu' lungo di `threshold` (stessa selezione di
/// [`compress_blocks`], boundary `i < cutoff`). Il call site (executor) offloada
/// questi contenuti su RAG PRIMA di comprimere e costruisce un marker con `ref`
/// (regola L: la SELEZIONE e' qui, l'I/O di offload sta fuori). PURA. `threshold` =
/// `max_content_chars` di fase.
pub fn contents_eligible_for_offload(
    hist: &[HistoryMessage],
    cutoff_index: usize,
    threshold: usize,
) -> Vec<String> {
    let boundary = cutoff_index.min(hist.len());
    let mut out: Vec<String> = Vec::new();
    for m in &hist[..boundary] {
        let Some(blocks) = m.anthropic_blocks() else {
            continue;
        };
        for block in blocks {
            let Some(obj) = block.as_object() else { continue };
            if obj.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            if let Some(Value::String(content)) = obj.get("content") {
                if content.chars().count() > threshold {
                    out.push(content.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod golden;
