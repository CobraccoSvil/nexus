//! Batch API multi-provider del gateway.
//!
//! Ultimo pezzo del consolidamento "tutto sul gateway": espone la Batch API che
//! oggi vive nel brain Python (`brain/providers/anthropic_batch.py` e
//! `brain/providers/google_batch.py`). A regime questi due moduli Python vengono
//! eliminati e il brain chiama il gateway via `POST /v1/batch` + `GET
//! /v1/batch/{provider}/{batch_id}`.
//!
//! Le batch API sono pensate per task NON urgenti (documentazione, analisi,
//! ottimizzazione): latenza da decine di secondi a minuti, costo ridotto.
//!
//! Stato implementazione:
//!   - ANTHROPIC: completo (Message Batches API REST). Submit, stato, risultati.
//!   - GOOGLE: 501 documentato. Il flusso Vertex Batch richiede `files.upload`
//!     (JSONL) + `batches.create(src=...)` + `files.download` dei risultati: un
//!     upload/download di file su Cloud Storage o sull'endpoint files che non e'
//!     riducibile a una chiamata REST pulita in questo passo. Implementarlo a
//!     meta' (es. solo submit senza recupero risultati) sarebbe fragile; quindi
//!     ritorna 501 con un messaggio che indica cosa manca (vedi `submit_google` /
//!     `fetch_google_results`).
//!
//! Regola G: nessun modello hardcoded (arriva dal chiamante nel campo `model`
//! di ogni richiesta). Regola F: mai loggare prompt/response in chiaro. Regola L:
//! il contratto delle singole richieste/risposte riusa `types.rs`
//! (`LlmRequest`/`LlmResponse`), niente tipo parallelo.

use serde::{Deserialize, Serialize};

use crate::types::{LlmRequest, LlmResponse, LlmToolCall, LlmUsage, ToolFunctionCall};

/// Versione API Messages richiesta dall'header `anthropic-version`. Allineata al
/// provider Anthropic non-batch ([`crate::providers::anthropic`]).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Endpoint REST delle Message Batches di Anthropic (relativo alla base_url).
const ANTHROPIC_BATCHES_PATH: &str = "/messages/batches";

/// `max_tokens` di default per le richieste batch che non lo specificano.
/// Non e' un nome di modello (regola G): e' il tetto di generazione richiesto
/// obbligatoriamente dall'API Messages, allineato al provider non-batch.
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ── Contratto pubblico (request/response degli endpoint) ─────────────────────

/// Singola richiesta del batch: un `custom_id` univoco + i parametri di
/// completion. Riusa il contratto `LlmRequest` (regola L) ma il `model` e i
/// parametri di generazione di OGNI richiesta sono autonomi (il batch puo'
/// mischiare modelli diversi dello stesso provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequestItem {
    /// Identificatore univoco scelto dal chiamante per ricollegare il risultato
    /// alla richiesta. Anthropic lo echeggia in ogni risultato.
    pub custom_id: String,
    /// Parametri della completion (model, messages, max_tokens, tools, ...).
    #[serde(flatten)]
    pub request: LlmRequest,
}

/// Body di `POST /v1/batch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBatchBody {
    /// Provider su cui creare il batch (`anthropic`, `google`).
    pub provider: String,
    /// Lista delle richieste, ciascuna con il suo `custom_id`.
    pub requests: Vec<BatchRequestItem>,
}

/// Stato canonico del batch, normalizzato cross-provider. Allineato ai valori
/// che il brain Python si aspetta (`in_progress`/`ended`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    /// Il batch e' ancora in elaborazione: i risultati non sono disponibili.
    InProgress,
    /// Il batch e' terminato: i risultati sono disponibili.
    Ended,
}

impl BatchStatus {
    /// `true` se il batch e' terminato (risultati pronti).
    pub fn is_ended(self) -> bool {
        matches!(self, BatchStatus::Ended)
    }
}

/// Conteggi per stato delle singole richieste del batch (telemetria di avanzamento).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BatchRequestCounts {
    #[serde(default)]
    pub processing: u32,
    #[serde(default)]
    pub succeeded: u32,
    #[serde(default)]
    pub errored: u32,
    #[serde(default)]
    pub canceled: u32,
    #[serde(default)]
    pub expired: u32,
}

/// Risposta di `POST /v1/batch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBatchResponse {
    pub batch_id: String,
    pub status: BatchStatus,
}

/// Esito di una singola richiesta del batch, ricollegato al suo `custom_id`.
/// `response` valorizzato in caso di successo, `error` in caso di fallimento;
/// esattamente uno dei due e' presente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResultItem {
    pub custom_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<LlmResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Risposta di `GET /v1/batch/{provider}/{batch_id}`. `results` e' presente solo
/// quando lo stato e' `ended`; mentre il batch e' in corso e' una lista vuota.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStatusResponse {
    pub status: BatchStatus,
    #[serde(default)]
    pub request_counts: BatchRequestCounts,
    #[serde(default)]
    pub results: Vec<BatchResultItem>,
}

// ── Anthropic: costruzione body submit ───────────────────────────────────────

/// Costruisce il body JSON di `POST {base_url}/messages/batches` dal contratto
/// del gateway. Ogni richiesta diventa `{custom_id, params}` dove `params` e' il
/// body Messages (`model`, `max_tokens`, `messages`, `system`, `tools`).
///
/// Funzione pura (niente rete): e' il punto testabile della serializzazione.
/// Regola G: il `model` arriva da ogni `LlmRequest`, mai hardcoded.
pub fn build_anthropic_batch_body(requests: &[BatchRequestItem]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = requests
        .iter()
        .map(|item| {
            serde_json::json!({
                "custom_id": item.custom_id,
                "params": build_anthropic_params(&item.request),
            })
        })
        .collect();
    serde_json::json!({ "requests": items })
}

/// Costruisce il blocco `params` (body Messages) di una singola richiesta batch.
/// Estrae il `system` come campo separato (parita' col provider non-batch) e
/// mappa i messaggi user/assistant. Versione mirata al caso d'uso batch
/// (documentazione/analisi): testo + system + tools opzionali; lo streaming non
/// si applica al batch.
fn build_anthropic_params(req: &LlmRequest) -> serde_json::Value {
    let mut system: Option<String> = None;
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for msg in &req.messages {
        if msg.role == "system" {
            system = Some(content_to_string(&msg.content));
            continue;
        }
        messages.push(serde_json::json!({
            "role": msg.role,
            "content": content_to_string(&msg.content),
        }));
    }

    let mut params = serde_json::json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
    });
    let obj = params
        .as_object_mut()
        .expect("json!{{...}} produce sempre un oggetto");
    if let Some(sys) = system {
        obj.insert("system".to_string(), serde_json::Value::String(sys));
    }
    if let Some(temp) = req.temperature {
        if let Some(n) = serde_json::Number::from_f64(temp as f64) {
            obj.insert("temperature".to_string(), serde_json::Value::Number(n));
        }
    }
    if let Some(tools) = &req.tools {
        let mapped: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "input_schema": t.function.parameters,
                })
            })
            .collect();
        obj.insert("tools".to_string(), serde_json::Value::Array(mapped));
    }
    params
}

/// Serializza il content di un messaggio a stringa (testo diretto o JSON dei
/// blocchi). Allineato a `content_to_string` del provider Anthropic non-batch.
fn content_to_string(content: &crate::types::MessageContent) -> String {
    match content {
        crate::types::MessageContent::Text(s) => s.clone(),
        crate::types::MessageContent::Blocks(blocks) => {
            serde_json::to_string(blocks).unwrap_or_default()
        }
    }
}

// ── Anthropic: parsing stato + risultati ─────────────────────────────────────

/// Forma JSON della retrieve `GET {base_url}/messages/batches/{id}`.
#[derive(Debug, Deserialize)]
struct AnthropicBatchInfo {
    /// `in_progress`, `canceling`, `ended`.
    processing_status: String,
    #[serde(default)]
    request_counts: AnthropicRequestCounts,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicRequestCounts {
    #[serde(default)]
    processing: u32,
    #[serde(default)]
    succeeded: u32,
    #[serde(default)]
    errored: u32,
    #[serde(default)]
    canceled: u32,
    #[serde(default)]
    expired: u32,
}

/// Mappa lo stato Anthropic (`processing_status`) allo stato canonico del
/// gateway. Solo `ended` rende disponibili i risultati; ogni altro valore
/// (`in_progress`, `canceling`, sconosciuti) e' trattato come `in_progress`
/// (fail-safe: non si tenta il fetch dei risultati prima del tempo).
pub fn map_anthropic_status(processing_status: &str) -> BatchStatus {
    match processing_status {
        "ended" => BatchStatus::Ended,
        _ => BatchStatus::InProgress,
    }
}

/// Parsa il JSON della retrieve nel contratto `BatchStatusResponse` (senza
/// risultati: quelli arrivano da un endpoint separato). Funzione pura testabile.
pub fn parse_anthropic_status(body: &serde_json::Value) -> anyhow::Result<BatchStatusResponse> {
    let info: AnthropicBatchInfo = serde_json::from_value(body.clone())
        .map_err(|e| anyhow::anyhow!("retrieve batch Anthropic non valida: {e}"))?;
    Ok(BatchStatusResponse {
        status: map_anthropic_status(&info.processing_status),
        request_counts: BatchRequestCounts {
            processing: info.request_counts.processing,
            succeeded: info.request_counts.succeeded,
            errored: info.request_counts.errored,
            canceled: info.request_counts.canceled,
            expired: info.request_counts.expired,
        },
        results: Vec::new(),
    })
}

/// Estrae l'id del batch dalla risposta di submit. Anthropic ritorna `{ "id":
/// "msgbatch_..." , ... }`.
pub fn parse_anthropic_batch_id(body: &serde_json::Value) -> anyhow::Result<String> {
    body.get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("submit batch Anthropic senza campo 'id'"))
}

/// Riga del file risultati `GET {base_url}/messages/batches/{id}/results` (JSONL):
/// un oggetto per riga con `custom_id` + `result`.
#[derive(Debug, Deserialize)]
struct AnthropicResultLine {
    custom_id: String,
    result: AnthropicResult,
}

/// Esito di una singola richiesta nel file risultati. `type` discrimina:
/// `succeeded` -> `message`; `errored`/`canceled`/`expired` -> `error`.
#[derive(Debug, Deserialize)]
struct AnthropicResult {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<AnthropicResultMessage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResultMessage {
    #[serde(default)]
    content: Vec<AnthropicResultBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: AnthropicResultUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicResultBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicResultUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

/// Mappa lo `stop_reason` Anthropic ai valori canonici del contratto. Allineato
/// a `map_stop_reason` del provider non-batch.
fn map_stop_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("end_turn") {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
    .to_string()
}

/// Converte una riga di risultato nel contratto `BatchResultItem`. Il successo
/// produce un `LlmResponse` (testo concatenato + tool_use + usage); il
/// fallimento produce un `error` testuale (Regola F: e' il messaggio d'errore
/// del provider, non prompt utente).
fn result_line_to_item(line: AnthropicResultLine) -> BatchResultItem {
    if line.result.kind == "succeeded" {
        if let Some(message) = line.result.message {
            let mut text = String::new();
            let mut tool_calls: Vec<LlmToolCall> = Vec::new();
            for block in message.content {
                match block {
                    AnthropicResultBlock::Text { text: t } => text.push_str(&t),
                    AnthropicResultBlock::ToolUse { id, name, input } => {
                        tool_calls.push(LlmToolCall {
                            id,
                            kind: "function".to_string(),
                            function: ToolFunctionCall {
                                name,
                                arguments: serde_json::to_string(&input)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            },
                        });
                    }
                    AnthropicResultBlock::Other => {}
                }
            }
            let response = LlmResponse {
                content: text,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                usage: LlmUsage {
                    input_tokens: message.usage.input_tokens,
                    output_tokens: message.usage.output_tokens,
                    cache_read_tokens: message.usage.cache_read_input_tokens,
                    cache_creation_tokens: message.usage.cache_creation_input_tokens,
                },
                model_used: message.model.unwrap_or_default(),
                provider_used: "anthropic".to_string(),
                latency_ms: 0,
                finish_reason: map_stop_reason(message.stop_reason.as_deref()),
                privacy_rerouted: None,
                reasoning: None,
                thinking_signature: None,
            };
            return BatchResultItem {
                custom_id: line.custom_id,
                response: Some(response),
                error: None,
            };
        }
        // type succeeded ma senza message: degradato a errore esplicito.
        return BatchResultItem {
            custom_id: line.custom_id,
            response: None,
            error: Some("risultato 'succeeded' senza message".to_string()),
        };
    }

    // errored / canceled / expired: messaggio d'errore dal payload.
    let error = line
        .result
        .error
        .map(|e| e.to_string())
        .unwrap_or_else(|| format!("batch item {}", line.result.kind));
    BatchResultItem {
        custom_id: line.custom_id,
        response: None,
        error: Some(error),
    }
}

/// Parsa il file risultati JSONL (una riga per richiesta) nel vettore di
/// `BatchResultItem`. Le righe vuote o non parsabili sono saltate (best-effort:
/// una riga corrotta non deve far fallire l'intero batch). Funzione pura.
pub fn parse_anthropic_results(jsonl: &str) -> Vec<BatchResultItem> {
    let mut out: Vec<BatchResultItem> = Vec::new();
    for line in jsonl.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<AnthropicResultLine>(trimmed) {
            Ok(parsed) => out.push(result_line_to_item(parsed)),
            Err(_) => {
                // Regola F: non logghiamo il contenuto della riga (puo' contenere
                // la response). Solo un warn senza payload.
                tracing::warn!("batch Anthropic: riga risultati non parsabile, saltata");
            }
        }
    }
    out
}

/// Costruisce l'URL della retrieve/submit batch Anthropic.
pub fn anthropic_batches_url(base_url: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), ANTHROPIC_BATCHES_PATH)
}

/// Costruisce l'URL della retrieve di un batch specifico.
pub fn anthropic_batch_url(base_url: &str, batch_id: &str) -> String {
    format!("{}/{}", anthropic_batches_url(base_url), batch_id)
}

/// Costruisce l'URL del file risultati di un batch.
pub fn anthropic_results_url(base_url: &str, batch_id: &str) -> String {
    format!("{}/results", anthropic_batch_url(base_url, batch_id))
}

/// Header comuni alle chiamate batch Anthropic (auth + versione API).
/// La Message Batches API e' GA: nessun beta header richiesto (parita' col
/// provider Python che usa `client.messages.batches`, non `client.beta`).
pub fn anthropic_headers(api_key: &str) -> [(&'static str, String); 2] {
    [
        ("x-api-key", api_key.to_string()),
        ("anthropic-version", ANTHROPIC_VERSION.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        LlmMessage, LlmToolDefinition, MessageContent, RequestMetadata, ToolFunctionDef,
    };

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            tenant_id: "t".to_string(),
            user_id: "u".to_string(),
            request_id: "r".to_string(),
            sensitivity_tier: 0,
            feature: "batch".to_string(),
        }
    }

    fn req(model: &str, system: Option<&str>, user: &str) -> LlmRequest {
        let mut messages = Vec::new();
        if let Some(s) = system {
            messages.push(LlmMessage {
                role: "system".to_string(),
                content: MessageContent::Text(s.to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            });
        }
        messages.push(LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(user.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        });
        LlmRequest {
            model: model.to_string(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn build_body_serializza_custom_id_e_params() {
        let items = vec![
            BatchRequestItem {
                custom_id: "doc-1".to_string(),
                request: req("claude-x", Some("sei un assistente"), "analizza"),
            },
            BatchRequestItem {
                custom_id: "doc-2".to_string(),
                request: req("claude-y", None, "ottimizza"),
            },
        ];
        let body = build_anthropic_batch_body(&items);
        let arr = body["requests"].as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // Prima richiesta: custom_id + params con system separato.
        assert_eq!(arr[0]["custom_id"], "doc-1");
        assert_eq!(arr[0]["params"]["model"], "claude-x");
        assert_eq!(arr[0]["params"]["system"], "sei un assistente");
        assert_eq!(arr[0]["params"]["max_tokens"], DEFAULT_MAX_TOKENS);
        // system NON e' tra i messages.
        let m0 = arr[0]["params"]["messages"].as_array().unwrap();
        assert_eq!(m0.len(), 1);
        assert_eq!(m0[0]["role"], "user");
        assert_eq!(m0[0]["content"], "analizza");

        // Seconda richiesta: niente system -> campo assente.
        assert_eq!(arr[1]["custom_id"], "doc-2");
        assert_eq!(arr[1]["params"]["model"], "claude-y");
        assert!(arr[1]["params"].get("system").is_none());
    }

    #[test]
    fn build_body_mappa_tools_su_input_schema() {
        let mut r = req("claude-x", None, "usa il tool");
        r.tools = Some(vec![LlmToolDefinition {
            kind: "function".to_string(),
            function: ToolFunctionDef {
                name: "search".to_string(),
                description: Some("cerca".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }]);
        let items = vec![BatchRequestItem {
            custom_id: "c-1".to_string(),
            request: r,
        }];
        let body = build_anthropic_batch_body(&items);
        let t = &body["requests"][0]["params"]["tools"][0];
        assert_eq!(t["name"], "search");
        assert_eq!(t["description"], "cerca");
        assert_eq!(t["input_schema"]["type"], "object");
    }

    #[test]
    fn parse_batch_id_da_submit() {
        let body = serde_json::json!({
            "id": "msgbatch_abc123",
            "type": "message_batch",
            "processing_status": "in_progress"
        });
        assert_eq!(parse_anthropic_batch_id(&body).unwrap(), "msgbatch_abc123");
    }

    #[test]
    fn parse_batch_id_assente_e_errore() {
        let body = serde_json::json!({ "type": "message_batch" });
        assert!(parse_anthropic_batch_id(&body).is_err());
    }

    #[test]
    fn map_status_solo_ended_e_terminale() {
        assert_eq!(map_anthropic_status("ended"), BatchStatus::Ended);
        assert_eq!(map_anthropic_status("in_progress"), BatchStatus::InProgress);
        assert_eq!(map_anthropic_status("canceling"), BatchStatus::InProgress);
        // Stato sconosciuto -> in_progress (fail-safe: non recupera risultati).
        assert_eq!(map_anthropic_status("boh"), BatchStatus::InProgress);
        assert!(BatchStatus::Ended.is_ended());
        assert!(!BatchStatus::InProgress.is_ended());
    }

    #[test]
    fn parse_status_in_progress_con_counts() {
        let body = serde_json::json!({
            "id": "msgbatch_x",
            "processing_status": "in_progress",
            "request_counts": {
                "processing": 3,
                "succeeded": 1,
                "errored": 0,
                "canceled": 0,
                "expired": 0
            }
        });
        let parsed = parse_anthropic_status(&body).unwrap();
        assert_eq!(parsed.status, BatchStatus::InProgress);
        assert_eq!(parsed.request_counts.processing, 3);
        assert_eq!(parsed.request_counts.succeeded, 1);
        // Stato in corso: nessun risultato nel payload di status.
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn parse_status_ended() {
        let body = serde_json::json!({
            "processing_status": "ended",
            "request_counts": { "succeeded": 2 }
        });
        let parsed = parse_anthropic_status(&body).unwrap();
        assert!(parsed.status.is_ended());
        assert_eq!(parsed.request_counts.succeeded, 2);
    }

    #[test]
    fn parse_results_succeeded_mappa_su_custom_id() {
        // Due righe JSONL: una succeeded testuale, una errored.
        let jsonl = concat!(
            r#"{"custom_id":"doc-1","result":{"type":"succeeded","message":{"model":"claude-x","stop_reason":"end_turn","content":[{"type":"text","text":"ciao "},{"type":"text","text":"mondo"}],"usage":{"input_tokens":10,"output_tokens":4}}}}"#,
            "\n",
            r#"{"custom_id":"doc-2","result":{"type":"errored","error":{"type":"invalid_request_error","message":"boom"}}}"#,
            "\n"
        );
        let items = parse_anthropic_results(jsonl);
        assert_eq!(items.len(), 2);

        // doc-1: successo, testo concatenato, usage, niente error.
        let doc1 = items.iter().find(|i| i.custom_id == "doc-1").unwrap();
        let resp = doc1.response.as_ref().unwrap();
        assert_eq!(resp.content, "ciao mondo");
        assert_eq!(resp.model_used, "claude-x");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 4);
        assert!(doc1.error.is_none());

        // doc-2: errore, niente response.
        let doc2 = items.iter().find(|i| i.custom_id == "doc-2").unwrap();
        assert!(doc2.response.is_none());
        assert!(doc2.error.as_ref().unwrap().contains("boom"));
    }

    #[test]
    fn parse_results_tool_use() {
        let jsonl = r#"{"custom_id":"c-1","result":{"type":"succeeded","message":{"model":"claude-x","stop_reason":"tool_use","content":[{"type":"tool_use","id":"tu_1","name":"calc","input":{"x":2}}],"usage":{"input_tokens":5,"output_tokens":9}}}}"#;
        let items = parse_anthropic_results(jsonl);
        assert_eq!(items.len(), 1);
        let resp = items[0].response.as_ref().unwrap();
        assert_eq!(resp.finish_reason, "tool_calls");
        let calls = resp.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tu_1");
        assert_eq!(calls[0].function.name, "calc");
        assert_eq!(calls[0].function.arguments, r#"{"x":2}"#);
    }

    #[test]
    fn parse_results_salta_righe_vuote_e_corrotte() {
        let jsonl = concat!(
            "\n",
            "non json\n",
            r#"{"custom_id":"ok","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"x"}],"usage":{"input_tokens":1,"output_tokens":1}}}}"#,
            "\n",
            "   \n"
        );
        let items = parse_anthropic_results(jsonl);
        // Solo la riga valida e' presente.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].custom_id, "ok");
    }

    #[test]
    fn url_builder_normalizza_slash_finale() {
        assert_eq!(
            anthropic_batches_url("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/messages/batches"
        );
        assert_eq!(
            anthropic_batch_url("https://api.anthropic.com/v1", "msgbatch_1"),
            "https://api.anthropic.com/v1/messages/batches/msgbatch_1"
        );
        assert_eq!(
            anthropic_results_url("https://api.anthropic.com/v1", "msgbatch_1"),
            "https://api.anthropic.com/v1/messages/batches/msgbatch_1/results"
        );
    }

    #[test]
    fn headers_contengono_api_key_e_version() {
        let h = anthropic_headers("secret-key");
        assert_eq!(h[0].0, "x-api-key");
        assert_eq!(h[0].1, "secret-key");
        assert_eq!(h[1].0, "anthropic-version");
        assert_eq!(h[1].1, ANTHROPIC_VERSION);
    }
}
