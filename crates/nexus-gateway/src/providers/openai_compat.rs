//! Client OpenAI-compatibile CONDIVISO.
//!
//! Punto unico (regola L) per tutti i provider che parlano il dialetto OpenAI
//! Chat Completions: OpenAI, Mistral, DeepSeek, vLLM. I provider concreti non
//! ereditano nulla, ma COMPONGONO un'istanza di [`OpenAiCompatClient`]
//! parametrizzata con `base_url`, `api_key` e capacita' proprie.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts`:
//! - costruzione richiesta `POST {base_url}/chat/completions`
//! - mapping `ChatCompletion` JSON -> [`LlmResponse`]
//! - streaming SSE (`response.bytes_stream()` + parser righe `data: {json}`)
//!
//! Regola G: nessun modello hardcoded, arriva sempre da `req.model`.
//! Regola F: mai loggare prompt/response in chiaro.

use std::time::Instant;

use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::ChunkStream;
use crate::types::{
    LlmRequest, LlmResponse, LlmStreamChunk, LlmToolCall, LlmUsage, ToolCallDelta,
    ToolCallDeltaFunction, ToolFunctionCall,
};

/// Dialetto di reasoning di un endpoint OpenAI-compatibile. Centralizza (regola
/// L) le differenze tra i provider che parlano il dialetto OpenAI ma gestiscono
/// il reasoning in modi diversi. La detection per-modello (es. o-series OpenAI)
/// resta a carico del provider, che sceglie il dialetto a runtime via
/// [`OpenAiCompatClient::with_reasoning`] / [`resolve_reasoning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningDialect {
    /// Nessuna gestione speciale: parametri base, niente reasoning (Mistral, e
    /// OpenAI per i modelli chat non-reasoning). I `reasoning_content` even-
    /// tualmente presenti nella response sono comunque letti (best-effort).
    None,
    /// DeepSeek: thinking governato da `extra_body.thinking.type`
    /// (enabled/disabled); il reasoning torna nel campo `reasoning_content`
    /// (response e stream delta).
    DeepSeek,
    /// OpenAI o-series / gpt-5 / gpt-4.5: usa `max_completion_tokens` al posto di
    /// `max_tokens` e accetta `reasoning_effort`; non espone il reasoning come
    /// testo, solo i `reasoning_tokens` in `completion_tokens_details`.
    OpenAiReasoning,
}

/// Configurazione di reasoning risolta per una richiesta. `dialect` indica come
/// parlare col provider; `enabled` se il thinking va attivato; `effort` il
/// livello per i modelli o-series (low/medium/high).
#[derive(Debug, Clone)]
pub struct ResolvedReasoning {
    pub dialect: ReasoningDialect,
    pub enabled: bool,
    pub effort: Option<String>,
}

impl ResolvedReasoning {
    /// Nessun reasoning, dialetto base: il default per i provider che non lo
    /// gestiscono (Mistral) e per le richieste senza `thinking`.
    pub fn none() -> Self {
        Self {
            dialect: ReasoningDialect::None,
            enabled: false,
            effort: None,
        }
    }
}

/// Client HTTP riusabile verso un endpoint OpenAI-compatibile.
///
/// Composto (non ereditato) dai provider concreti. Il `provider_name` viene
/// scritto in `LlmResponse.provider_used` cosi' ogni wrapper riporta la propria
/// identita' senza dover rimappare la risposta.
#[derive(Clone)]
pub struct OpenAiCompatClient {
    http: Client,
    base_url: String,
    api_key: String,
    provider_name: String,
}

impl OpenAiCompatClient {
    /// Costruisce il client. `base_url` senza slash finale (es.
    /// `https://api.mistral.ai/v1`); l'endpoint `/chat/completions` viene
    /// aggiunto internamente.
    pub fn new(
        http: Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        provider_name: impl Into<String>,
    ) -> Self {
        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url,
            api_key: api_key.into(),
            provider_name: provider_name.into(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// Esegue una completion non-streaming e mappa il risultato in
    /// [`LlmResponse`]. Dialetto base, nessun reasoning (Mistral, vLLM, OpenAI
    /// chat non-reasoning): delega a [`Self::complete_with_reasoning`].
    pub async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        self.complete_with_reasoning(req, &ResolvedReasoning::none())
            .await
    }

    /// Variante con reasoning esplicito: i provider che lo gestiscono
    /// (DeepSeek, OpenAI o-series) passano il [`ResolvedReasoning`] risolto.
    pub async fn complete_with_reasoning(
        &self,
        req: &LlmRequest,
        reasoning: &ResolvedReasoning,
    ) -> anyhow::Result<LlmResponse> {
        let body = build_request_body(req, false, reasoning);
        let start = Instant::now();

        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: il body d'errore puo' contenere dettagli del provider
            // ma non prompt/response utente; lo propaghiamo al caller (la Fase 3
            // distingue il billing error), senza loggarlo qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("{} HTTP {}: {}", self.provider_name, status.as_u16(), text);
        }

        let parsed: ChatCompletion = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        from_chat_completion(parsed, req.model.clone(), &self.provider_name, latency_ms)
    }

    /// Esegue una completion in streaming. Legge `bytes_stream()`, accumula i
    /// byte e parsa le righe SSE `data: {json}` fino a `[DONE]`, emettendo un
    /// [`LlmStreamChunk`] per ogni delta.
    ///
    /// Implementazione: un task `tokio::spawn` consuma il `bytes_stream()` (dove
    /// il tipo concreto e' inferito, cosi' non serve nominare `bytes::Bytes` nei
    /// campi) e spinge i chunk parsati in un canale; lo stream restituito legge
    /// dal canale. Cosi' lo `ChunkStream` e' `'static + Send` come da contratto.
    pub async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        self.stream_with_reasoning(req, &ResolvedReasoning::none())
            .await
    }

    /// Variante streaming con reasoning esplicito (vedi
    /// [`Self::complete_with_reasoning`]).
    pub async fn stream_with_reasoning(
        &self,
        req: &LlmRequest,
        reasoning: &ResolvedReasoning,
    ) -> anyhow::Result<ChunkStream> {
        let body = build_request_body(req, true, reasoning);

        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("{} HTTP {}: {}", self.provider_name, status.as_u16(), text);
        }

        let provider_name = self.provider_name.clone();
        let model_used = req.model.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut parser = SseParser {
                line_buf: String::new(),
                pending: std::collections::VecDeque::new(),
                done: false,
                provider_name,
                model_used,
            };

            loop {
                match bytes.next().await {
                    Some(Ok(buf)) => {
                        parser.line_buf.push_str(&String::from_utf8_lossy(&buf));
                        parser.drain_lines();
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(Err(anyhow::Error::new(e))).await;
                        return;
                    }
                    None => {
                        // Fine stream: processa l'eventuale residuo nel buffer.
                        let leftover = std::mem::take(&mut parser.line_buf);
                        for line in leftover.lines() {
                            parser.parse_line(line);
                        }
                        while let Some(chunk) = parser.pending.pop_front() {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                }

                // Inoltra i chunk pronti; se il consumer ha chiuso, termina.
                while let Some(chunk) = parser.pending.pop_front() {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
                if parser.done {
                    return;
                }
            }
        });

        let out = ReceiverStream::new(rx);
        Ok(out.boxed())
    }

    /// Probe di salute: una HEAD/GET su `{base_url}/models`. Ritorna `false` su
    /// qualunque errore (rete, auth, status non 2xx).
    pub async fn healthcheck(&self) -> bool {
        let url = format!("{}/models", self.base_url);
        match self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

/// Parser SSE riusabile: accumula righe, le decodifica in [`LlmStreamChunk`] e
/// le accoda in `pending`. Stateful ma autonomo dal trasporto (testabile senza
/// rete: vedi i test sotto).
struct SseParser {
    line_buf: String,
    pending: std::collections::VecDeque<LlmStreamChunk>,
    done: bool,
    provider_name: String,
    model_used: String,
}

impl SseParser {
    /// Estrae dal buffer tutte le righe complete (terminate da `\n`) e le parsa,
    /// lasciando nel buffer l'eventuale riga parziale finale.
    fn drain_lines(&mut self) {
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].to_string();
            // Rimuove la riga consumata (incluso il '\n').
            self.line_buf.drain(..=idx);
            self.parse_line(&line);
        }
    }

    /// Parsa una singola riga SSE. Le righe utili iniziano con `data:`; `[DONE]`
    /// chiude lo stream. Le altre (commenti, righe vuote) sono ignorate.
    fn parse_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return,
        };
        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            self.done = true;
            return;
        }
        let parsed: ChatCompletionChunk = match serde_json::from_str(payload) {
            Ok(p) => p,
            // Frammento JSON non valido: lo ignoriamo (puo' arrivare spezzato in
            // un blocco di byte successivo gia' gestito dal buffer riga).
            Err(_) => return,
        };
        if let Some(chunk) = chunk_from_sse(parsed, &self.provider_name, &self.model_used) {
            self.pending.push_back(chunk);
        }
    }
}

/// Costruisce il corpo JSON della richiesta `/chat/completions`.
///
/// `stream=true` aggiunge anche `stream_options.include_usage` per ottenere il
/// conteggio token nell'ultimo chunk (parita' col TS).
///
/// `reasoning` governa le differenze di dialetto (regola L, punto unico):
///   - [`ReasoningDialect::None`] (Mistral, vLLM, OpenAI chat): `max_tokens`
///     standard, nessun parametro reasoning;
///   - [`ReasoningDialect::OpenAiReasoning`] (o-series/gpt-5): `max_tokens`
///     diventa `max_completion_tokens`, temperatura omessa (non accettata) e si
///     invia `reasoning_effort` se presente;
///   - [`ReasoningDialect::DeepSeek`]: `extra_body.thinking.type` enabled/disabled.
fn build_request_body(
    req: &LlmRequest,
    stream: bool,
    reasoning: &ResolvedReasoning,
) -> ChatCompletionRequest {
    let messages = req.messages.iter().map(to_wire_message).collect();
    let tools = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| WireTool {
                kind: "function".to_string(),
                function: WireToolFn {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    parameters: t.function.parameters.clone(),
                    strict: t.function.strict,
                },
            })
            .collect()
    });

    // o-series: tetto di output via max_completion_tokens; max_tokens omesso e
    // temperatura non inviata (l'API la rifiuta sui modelli reasoning).
    let is_openai_reasoning = reasoning.dialect == ReasoningDialect::OpenAiReasoning;
    let (max_tokens, max_completion_tokens) = if is_openai_reasoning {
        (None, req.max_tokens)
    } else {
        (req.max_tokens, None)
    };
    let temperature = if is_openai_reasoning {
        None
    } else {
        req.temperature
    };
    let reasoning_effort = if is_openai_reasoning {
        reasoning.effort.clone()
    } else {
        None
    };

    // DeepSeek: thinking ufficiale via extra_body. Lo inviamo SOLO quando vogliamo
    // forzare uno stato esplicito (disabled per task interni/tool; enabled su
    // richiesta thinking). Senza extra_body DeepSeek usa il suo default.
    let extra_body = if reasoning.dialect == ReasoningDialect::DeepSeek {
        let kind = if reasoning.enabled { "enabled" } else { "disabled" };
        Some(serde_json::json!({ "thinking": { "type": kind } }))
    } else {
        None
    };

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        temperature,
        max_tokens,
        max_completion_tokens,
        reasoning_effort,
        extra_body,
        tools,
        response_format: req.response_format.clone(),
        stream: if stream { Some(true) } else { None },
        stream_options: if stream {
            Some(StreamOptions { include_usage: true })
        } else {
            None
        },
    }
}

/// Converte un [`crate::types::LlmMessage`] nel formato wire OpenAI.
///
/// Il `content` viene serializzato a stringa quando e' una lista di blocchi
/// (parita' col TS che fa `JSON.stringify`). Per i messaggi `assistant` con
/// tool-call il content puo' essere `null`.
fn to_wire_message(msg: &crate::types::LlmMessage) -> WireMessage {
    use crate::types::MessageContent;

    let content_str = match &msg.content {
        MessageContent::Text(s) => Some(s.clone()),
        MessageContent::Blocks(blocks) => {
            serde_json::to_string(blocks).ok().or(Some(String::new()))
        }
    };

    let tool_calls = msg.tool_calls.as_ref().map(|calls| {
        calls
            .iter()
            .map(|tc| WireToolCall {
                id: tc.id.clone(),
                kind: "function".to_string(),
                function: WireToolCallFn {
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                },
            })
            .collect::<Vec<_>>()
    });

    // assistant con tool_calls: content puo' essere null (parita' TS).
    let content = if msg.role == "assistant" && tool_calls.is_some() {
        match &msg.content {
            MessageContent::Text(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    } else {
        content_str
    };

    WireMessage {
        role: msg.role.clone(),
        content,
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls,
        name: msg.name.clone(),
    }
}

/// Mappa una [`ChatCompletion`] non-streaming in [`LlmResponse`].
fn from_chat_completion(
    resp: ChatCompletion,
    model_used: String,
    provider_name: &str,
    latency_ms: u64,
) -> anyhow::Result<LlmResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{}: nessuna choice nella risposta", provider_name))?;

    let tool_calls: Option<Vec<LlmToolCall>> = choice.message.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|tc| LlmToolCall {
                id: tc.id,
                kind: "function".to_string(),
                function: ToolFunctionCall {
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                },
            })
            .collect()
    });

    let usage = LlmUsage {
        input_tokens: resp
            .usage
            .as_ref()
            .map(|u| u.prompt_tokens)
            .unwrap_or(0),
        output_tokens: resp
            .usage
            .as_ref()
            .map(|u| u.completion_tokens)
            .unwrap_or(0),
        // Prompt caching automatico (DeepSeek `prompt_cache_hit_tokens`, OpenAI
        // `prompt_tokens_details.cached_tokens`): sottoinsieme dell'input.
        cache_read_tokens: resp.usage.as_ref().and_then(|u| u.cached_input_tokens()),
        cache_creation_tokens: None,
    };

    let finish_reason = normalize_finish_reason(choice.finish_reason.as_deref());

    // Reasoning DeepSeek: arriva nel campo separato `reasoning_content`. OpenAI
    // o-series non espone il reasoning come testo (solo i token, gia' nel usage),
    // quindi qui resta `None` per quel dialetto.
    let reasoning = choice
        .message
        .reasoning_content
        .filter(|r| !r.is_empty());

    Ok(LlmResponse {
        content: choice.message.content.unwrap_or_default(),
        tool_calls,
        usage,
        model_used,
        provider_used: provider_name.to_string(),
        latency_ms,
        finish_reason,
        privacy_rerouted: None,
        reasoning,
        // Dialetto OpenAI-compat: nessuna signature opaca da ri-passare.
        thinking_signature: None,
    })
}

/// Mappa un chunk SSE in [`LlmStreamChunk`]. Ritorna `None` se il chunk non
/// porta delta utili (es. solo metadati di apertura).
fn chunk_from_sse(
    chunk: ChatCompletionChunk,
    provider_name: &str,
    model_used: &str,
) -> Option<LlmStreamChunk> {
    let usage = chunk.usage.map(|u| LlmUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_read_tokens: u.cached_input_tokens(),
        cache_creation_tokens: None,
    });

    let choice = chunk.choices.into_iter().next();
    let finish_reason = choice
        .as_ref()
        .and_then(|c| c.finish_reason.clone())
        .map(|r| normalize_finish_reason(Some(&r)));

    let delta = choice.as_ref().and_then(|c| c.delta.as_ref());

    // Tool-call delta: emette il primo (parita' col TS che yield-a tc[0]).
    if let Some(d) = delta {
        if let Some(tc) = d.tool_calls.as_ref().and_then(|v| v.first()) {
            return Some(LlmStreamChunk {
                delta: String::new(),
                tool_call_delta: Some(ToolCallDelta {
                    index: tc.index,
                    id: tc.id.clone(),
                    function: tc.function.as_ref().map(|f| ToolCallDeltaFunction {
                        name: f.name.clone(),
                        arguments: f.arguments.clone(),
                    }),
                }),
                finish_reason: None,
                usage: None,
                provider_used: Some(provider_name.to_string()),
                model_used: Some(model_used.to_string()),
                reasoning_delta: None,
            });
        }
    }

    let content_delta = delta.and_then(|d| d.content.clone()).unwrap_or_default();
    // Reasoning DeepSeek in streaming: campo separato `reasoning_content` nel
    // delta. Va in `reasoning_delta`, non in `delta` (parita' col round-trip
    // reasoning del brain).
    let reasoning_delta = delta
        .and_then(|d| d.reasoning_content.clone())
        .filter(|r| !r.is_empty());

    // Niente delta di testo, niente reasoning, niente finish, niente usage:
    // chunk vuoto, salta.
    if content_delta.is_empty()
        && reasoning_delta.is_none()
        && finish_reason.is_none()
        && usage.is_none()
    {
        return None;
    }

    // L'usage va riportato solo all'ultimo chunk (quando c'e' finish_reason),
    // come nel TS.
    let usage = if finish_reason.is_some() { usage } else { None };

    Some(LlmStreamChunk {
        delta: content_delta,
        tool_call_delta: None,
        finish_reason,
        usage,
        provider_used: Some(provider_name.to_string()),
        model_used: Some(model_used.to_string()),
        reasoning_delta,
    })
}

/// Normalizza il `finish_reason` ai valori canonici del contratto. I valori non
/// noti collassano a `stop` (parita' col `finishReasonMap` del TS).
fn normalize_finish_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("stop") {
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        _ => "stop",
    }
    .to_string()
}

/// Detection di errore di billing/crediti esauriti (per la Fase 3: cooldown
/// automatico del provider). Pattern case-insensitive ispirati ai messaggi reali
/// di OpenAI/Mistral/DeepSeek e ai 402 Payment Required.
pub fn is_billing_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("insufficient_quota")
        || m.contains("exceeded your current quota")
        || m.contains("payment required")
        || m.contains("billing")
        || (m.contains("credit balance") && m.contains("too low"))
}

// ---------------------------------------------------------------------------
// Tipi wire (formato OpenAI Chat Completions). Separati dai tipi di contratto
// per non accoppiare la serializzazione del dialetto provider al contratto del
// gateway.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Tetto di output per i modelli o-series/gpt-5 (al posto di `max_tokens`).
    #[serde(rename = "max_completion_tokens", skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    /// Livello di reasoning (low/medium/high) per i modelli o-series.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// Campi extra appiattiti nel body radice (DeepSeek `thinking`): il client
    /// OpenAI ufficiale fonde `extra_body` nel top-level, quindi facciamo lo
    /// stesso con `serde(flatten)`. `None` => nessun campo aggiunto.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    extra_body: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: String,
    // Serializziamo sempre `content` (anche null) per i messaggi assistant con
    // tool-call, dove l'API richiede esplicitamente `content: null`.
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: String,
    function: WireToolFn,
}

#[derive(Debug, Serialize)]
struct WireToolFn {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireToolCallFn,
}

#[derive(Debug, Serialize)]
struct WireToolCallFn {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletion {
    #[serde(default)]
    choices: Vec<RespChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct RespChoice {
    message: RespMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RespMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<RespToolCall>>,
    /// Reasoning DeepSeek (campo separato dal content). Assente sugli altri
    /// provider OpenAI-compat.
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RespToolCall {
    id: String,
    function: RespToolCallFn,
}

#[derive(Debug, Deserialize)]
struct RespToolCallFn {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    /// DeepSeek: token di input serviti dal context caching automatico.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    /// OpenAI: dettaglio dei token di input, con `cached_tokens`.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

impl WireUsage {
    /// Token di input serviti da cache, normalizzati cross-provider: DeepSeek li
    /// espone in `prompt_cache_hit_tokens`, OpenAI in
    /// `prompt_tokens_details.cached_tokens`. Ritorna `None` se entrambi assenti
    /// o a zero.
    fn cached_input_tokens(&self) -> Option<u32> {
        let hit = self
            .prompt_cache_hit_tokens
            .or_else(|| self.prompt_tokens_details.as_ref().and_then(|d| d.cached_tokens));
        hit.filter(|&n| n > 0)
    }
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Option<ChunkDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCallDelta>>,
    /// Delta del reasoning DeepSeek in streaming (campo separato).
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkToolCallDeltaFn>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCallDeltaFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    fn sample_request() -> LlmRequest {
        LlmRequest {
            model: "test-model".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
            }],
            temperature: Some(0.5),
            max_tokens: Some(256),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: RequestMetadata {
                tenant_id: "t".to_string(),
                user_id: "u".to_string(),
                request_id: "r".to_string(),
                sensitivity_tier: 0,
                feature: "f".to_string(),
            },
        }
    }

    #[test]
    fn request_body_serializza_campi_base() {
        let req = sample_request();
        let body = build_request_body(&req, false, &ResolvedReasoning::none());
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["model"], "test-model");
        assert_eq!(json["temperature"], 0.5);
        assert_eq!(json["max_tokens"], 256);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "ciao");
        // stream non richiesto: campo assente.
        assert!(json.get("stream").is_none());
        assert!(json.get("stream_options").is_none());
        // Dialetto base: nessun campo reasoning.
        assert!(json.get("max_completion_tokens").is_none());
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn request_body_streaming_aggiunge_include_usage() {
        let req = sample_request();
        let body = build_request_body(&req, true, &ResolvedReasoning::none());
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    // --- Dialetti reasoning (passo 2) --------------------------------------

    #[test]
    fn dialetto_openai_reasoning_usa_max_completion_tokens() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            enabled: true,
            effort: Some("high".to_string()),
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning)).unwrap();

        // max_tokens -> max_completion_tokens; temperatura omessa; effort inviato.
        assert!(json.get("max_tokens").is_none());
        assert_eq!(json["max_completion_tokens"], 256);
        assert!(json.get("temperature").is_none());
        assert_eq!(json["reasoning_effort"], "high");
    }

    #[test]
    fn dialetto_openai_reasoning_senza_effort_non_lo_invia() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            enabled: true,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning)).unwrap();
        assert_eq!(json["max_completion_tokens"], 256);
        // Nessun effort configurato: il campo non c'e' (default del modello).
        assert!(json.get("reasoning_effort").is_none());
    }

    #[test]
    fn dialetto_deepseek_enabled_aggiunge_thinking_appiattito() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: true,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning)).unwrap();

        // extra_body appiattito nel body radice: thinking.type=enabled.
        assert_eq!(json["thinking"]["type"], "enabled");
        // max_tokens standard (DeepSeek non e' o-series).
        assert_eq!(json["max_tokens"], 256);
        assert!(json.get("max_completion_tokens").is_none());
    }

    #[test]
    fn dialetto_deepseek_disabled_aggiunge_thinking_disabled() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: false,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning)).unwrap();
        assert_eq!(json["thinking"]["type"], "disabled");
    }

    #[test]
    fn deserializza_reasoning_content_deepseek() {
        let raw = r#"{
            "choices": [{
                "message": {"content": "risposta", "reasoning_content": "ho riflettuto"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "prompt_cache_hit_tokens": 4}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "deepseek", 1).unwrap();

        assert_eq!(resp.content, "risposta");
        assert_eq!(resp.reasoning.as_deref(), Some("ho riflettuto"));
        // Cache hit DeepSeek normalizzato.
        assert_eq!(resp.usage.cache_read_tokens, Some(4));
    }

    #[test]
    fn deserializza_cache_openai_prompt_tokens_details() {
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 12}}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "openai", 1).unwrap();
        assert_eq!(resp.usage.cache_read_tokens, Some(12));
    }

    #[test]
    fn response_senza_reasoning_ha_reasoning_none() {
        let raw = r#"{
            "choices": [{"message": {"content": "ok", "reasoning_content": ""}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "deepseek", 1).unwrap();
        // reasoning vuoto -> None; cache assente -> None.
        assert!(resp.reasoning.is_none());
        assert!(resp.usage.cache_read_tokens.is_none());
    }

    #[test]
    fn sse_reasoning_content_emette_reasoning_delta() {
        let raw = r#"{
            "choices": [{"delta": {"reasoning_content": "penso"}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "deepseek", "m").expect("chunk reasoning");
        assert_eq!(out.reasoning_delta.as_deref(), Some("penso"));
        assert_eq!(out.delta, "");
    }

    #[test]
    fn deserializza_response_in_llm_response() {
        let raw = r#"{
            "choices": [{
                "message": {"content": "risposta", "tool_calls": null},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp =
            from_chat_completion(parsed, "m".to_string(), "openai", 42).unwrap();

        assert_eq!(resp.content, "risposta");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.provider_used, "openai");
        assert_eq!(resp.latency_ms, 42);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn deserializza_response_con_tool_calls() {
        let raw = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "do_thing", "arguments": "{\"a\":1}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 7}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "openai", 1).unwrap();

        assert_eq!(resp.content, "");
        assert_eq!(resp.finish_reason, "tool_calls");
        let calls = resp.tool_calls.expect("tool_calls presenti");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "do_thing");
        assert_eq!(calls[0].function.arguments, "{\"a\":1}");
    }

    #[test]
    fn parsa_evento_sse_data_in_chunk() {
        let raw = r#"{
            "choices": [{"delta": {"content": "Hel"}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "openai", "m").expect("chunk emesso");

        assert_eq!(out.delta, "Hel");
        assert!(out.finish_reason.is_none());
        assert!(out.usage.is_none());
        assert_eq!(out.provider_used.as_deref(), Some("openai"));
    }

    #[test]
    fn sse_chunk_finale_riporta_usage() {
        let raw = r#"{
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 2}
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "openai", "m").expect("chunk finale");

        assert_eq!(out.delta, "");
        assert_eq!(out.finish_reason.as_deref(), Some("stop"));
        let usage = out.usage.expect("usage all'ultimo chunk");
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn sse_tool_call_delta() {
        let raw = r#"{
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call_x",
                "function": {"name": "f", "arguments": "{}"}
            }]}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "deepseek", "m").expect("tool delta");

        let tcd = out.tool_call_delta.expect("tool_call_delta presente");
        assert_eq!(tcd.index, 0);
        assert_eq!(tcd.id.as_deref(), Some("call_x"));
        assert_eq!(tcd.function.unwrap().name.as_deref(), Some("f"));
    }

    fn empty_parser() -> SseParser {
        SseParser {
            line_buf: String::new(),
            pending: std::collections::VecDeque::new(),
            done: false,
            provider_name: "openai".to_string(),
            model_used: "m".to_string(),
        }
    }

    #[test]
    fn parse_sse_line_consuma_data_e_done() {
        let mut st = empty_parser();

        st.parse_line(
            r#"data: {"choices":[{"delta":{"content":"x"},"finish_reason":null}]}"#,
        );
        assert_eq!(st.pending.len(), 1);
        assert_eq!(st.pending[0].delta, "x");

        st.parse_line("data: [DONE]");
        assert!(st.done);
    }

    #[test]
    fn drain_lines_gestisce_riga_parziale() {
        let mut st = empty_parser();
        // Primo blocco: una riga completa + una parziale (senza '\n' finale).
        st.line_buf.push_str(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\ndata: {\"choices\":[{\"del",
        );
        st.drain_lines();
        // Solo la prima riga e' completa: un chunk pronto.
        assert_eq!(st.pending.len(), 1);
        assert_eq!(st.pending[0].delta, "a");
        // Il resto del secondo evento arriva dopo: ora la riga si completa.
        st.line_buf
            .push_str("ta\":{\"content\":\"b\"}}]}\n");
        st.drain_lines();
        assert_eq!(st.pending.len(), 2);
        assert_eq!(st.pending[1].delta, "b");
    }

    #[test]
    fn finish_reason_sconosciuto_collassa_a_stop() {
        assert_eq!(normalize_finish_reason(Some("boh")), "stop");
        assert_eq!(normalize_finish_reason(None), "stop");
        assert_eq!(normalize_finish_reason(Some("length")), "length");
        assert_eq!(normalize_finish_reason(Some("tool_calls")), "tool_calls");
    }

    #[test]
    fn billing_error_pattern() {
        assert!(is_billing_error("Error: insufficient_quota for org"));
        assert!(is_billing_error("You exceeded your current quota"));
        assert!(is_billing_error("402 Payment Required"));
        assert!(is_billing_error(
            "Your credit balance is too low to access the API"
        ));
        assert!(is_billing_error("BILLING hard limit reached"));
        assert!(!is_billing_error("rate limit exceeded, retry later"));
        assert!(!is_billing_error("model not found"));
    }
}
