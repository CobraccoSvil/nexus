//! Provider Anthropic (API Messages nativa).
//!
//! Porting di `packages/llm-gateway/src/providers/anthropic.ts`. A differenza
//! degli altri provider della Fase 2, Anthropic NON parla il dialetto OpenAI
//! Chat Completions: usa l'API Messages (`POST {base_url}/messages`) con un
//! formato proprio. Per questo NON compone [`OpenAiCompatClient`] ma ha un
//! client dedicato. Le differenze strutturali rispetto a OpenAI-compat:
//!   - il `system` prompt e' un campo separato, non un messaggio con `role`;
//!   - le tool-call sono `content block` `tool_use` (non `message.tool_calls`);
//!   - i tool-result tornano come messaggio `user` con block `tool_result`;
//!   - autenticazione via header `x-api-key` + `anthropic-version` (non Bearer);
//!   - `max_tokens` e' obbligatorio nella request;
//!   - lo streaming SSE usa eventi tipizzati (`content_block_delta`,
//!     `message_delta`, `message_stop`) anziche' chunk `chat.completion.chunk`.
//!
//! Regola G: nessun modello hardcoded (arriva da `req.model`). Regola F: mai
//! loggare prompt/response in chiaro.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use nexus_cache::TtlCache;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::{ChunkStream, LlmProvider};
use crate::types::{
    LlmRequest, LlmResponse, LlmStreamChunk, LlmToolCall, LlmUsage, MessageContent,
    SensitivityTier, ToolFunctionCall,
};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint Messages di default (override via costruttore, es. proxy aziendale).
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Versione API Messages richiesta dall'header `anthropic-version`.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta header richiesto dall'extended thinking interleaved (parita' col Python,
/// `betas = ["interleaved-thinking-2025-05-14"]`). Inviato via `anthropic-beta`
/// solo quando il thinking e' attivo per la richiesta.
const THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// `max_tokens` di default quando la request non lo specifica. Non e' un nome di
/// modello (regola G): e' il tetto di generazione richiesto obbligatoriamente
/// dall'API Messages, allineato al `?? 4096` del TS.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Chiave settings (regola G) del budget di extended thinking. TEXT nel DB,
/// interpretato come numero di token.
const THINKING_BUDGET_SETTING: &str = "anthropic_thinking_budget";

/// Budget thinking usato SOLO se il DB e' irraggiungibile e la richiesta ha
/// thinking abilitato (fallback graceful documentato, regola G). Non e' un
/// "magic default" per il routing: e' il tetto di sicurezza del solo budget
/// thinking quando i settings non sono leggibili.
const THINKING_BUDGET_DB_DOWN_FALLBACK: u32 = 2048;

/// TTL della cache settings (60s, come `policy_engine`/`cooldown`).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Provider Anthropic. Mantiene un client HTTP dedicato. Il budget di extended
/// thinking e' letto dai settings DB con cache TTL (punto unico `TtlCache`,
/// regola L). Il `PgPool` e' opzionale: assente nei test che esercitano solo la
/// mappatura request/response senza rete ne' DB.
pub struct AnthropicProvider {
    http: Client,
    base_url: String,
    api_key: String,
    db: Option<PgPool>,
    thinking_budget: TtlCache<(), u32>,
}

impl AnthropicProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). Il budget
    /// thinking non sara' leggibile dai settings: il thinking resta disattivo a
    /// meno che la request non porti un `budget_tokens` esplicito.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere il budget thinking dai
    /// settings (regola G). `base_url` opzionale (default Anthropic ufficiale);
    /// `api_key` iniettata dal chiamante (regola F: niente segreti nel codice).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url,
            api_key: api_key.into(),
            db,
            thinking_budget: TtlCache::new(SETTINGS_TTL),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.base_url)
    }

    /// Budget thinking corrente dai settings (cache TTL 60s). Se il DB e'
    /// irraggiungibile o la chiave assente, ricade su un budget di sicurezza
    /// documentato (`THINKING_BUDGET_DB_DOWN_FALLBACK`). Il valore viene comunque
    /// validato a valle (`resolve_thinking_budget`): se >= max_tokens il thinking
    /// resta disattivato.
    async fn configured_thinking_budget(&self) -> u32 {
        if let Some(b) = self.thinking_budget.get(&()) {
            return b;
        }
        let Some(db) = self.db.as_ref() else {
            return THINKING_BUDGET_DB_DOWN_FALLBACK;
        };
        let parsed = nexus_auth::get_setting(db, THINKING_BUDGET_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u32>().ok());
        let budget = parsed.unwrap_or(THINKING_BUDGET_DB_DOWN_FALLBACK);
        self.thinking_budget.insert((), budget);
        budget
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        200_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let configured = self.configured_thinking_budget().await;
        let thinking_budget = resolve_thinking_budget(req, configured);
        let body = build_request_body(req, false, thinking_budget);
        let start = Instant::now();

        let mut builder = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if thinking_budget.is_some() {
            builder = builder.header("anthropic-beta", THINKING_BETA);
        }
        let resp = builder.json(&body).send().await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: il body d'errore non contiene prompt utente ma dettagli
            // del provider; lo propaghiamo al caller (il cooldown della Fase 3
            // riconosce il billing via `is_billing_error`), senza loggarlo qui.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("anthropic HTTP {}: {}", status.as_u16(), text);
        }

        let parsed: AnthropicMessage = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_anthropic_message(parsed, req.model.clone(), latency_ms))
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let configured = self.configured_thinking_budget().await;
        let thinking_budget = resolve_thinking_budget(req, configured);
        let body = build_request_body(req, true, thinking_budget);

        let mut builder = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if thinking_budget.is_some() {
            builder = builder.header("anthropic-beta", THINKING_BETA);
        }
        let resp = builder.json(&body).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("anthropic HTTP {}: {}", status.as_u16(), text);
        }

        let model_used = req.model.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut parser = AnthropicSseParser::new(model_used);

            loop {
                match bytes.next().await {
                    Some(Ok(buf)) => {
                        parser.push_bytes(&String::from_utf8_lossy(&buf));
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(Err(anyhow::Error::new(e))).await;
                        return;
                    }
                    None => {
                        parser.flush_leftover();
                        while let Some(chunk) = parser.pending.pop_front() {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                }

                while let Some(chunk) = parser.pending.pop_front() {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn healthcheck(&self) -> bool {
        // GET /models: 2xx => provider raggiungibile. Su billing error l'API
        // ritorna comunque un 4xx alle chiamate `complete`, ma il probe modelli
        // resta valido per il re-probe reattivo del cooldown (Fase 3): quando i
        // crediti tornano, il primo healthcheck successivo riabilita il provider.
        let url = format!("{}/models", self.base_url);
        match self
            .http
            .get(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

/// Budget thinking effettivo per la richiesta. `Some(budget)` => extended
/// thinking attivo con quel budget; `None` => disattivato. Il budget e' risolto
/// a monte (settings DB, regola G) dal provider, non hardcoded qui.
///
/// Replica la guardia del Python (`max_tokens > thinking_budget`): un budget
/// >= `max_tokens` produrrebbe HTTP 400, quindi in quel caso il thinking resta
/// disattivato.
fn resolve_thinking_budget(req: &LlmRequest, configured_budget: u32) -> Option<u32> {
    let enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
    if !enabled {
        return None;
    }
    // Budget esplicito nella request ha priorita' su quello configurato.
    let budget = req
        .thinking
        .as_ref()
        .and_then(|t| t.budget_tokens)
        .unwrap_or(configured_budget);
    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if budget == 0 || budget >= max_tokens {
        return None;
    }
    Some(budget)
}

/// Costruisce il corpo JSON della request Messages a partire dal contratto LLM.
/// `thinking_budget` e' il budget effettivo gia' risolto (vedi
/// [`resolve_thinking_budget`]): `Some` => blocco thinking nel body.
fn build_request_body(
    req: &LlmRequest,
    stream: bool,
    thinking_budget: Option<u32>,
) -> AnthropicRequest {
    let (system, messages) = to_anthropic_messages(req);

    let tools = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.function.name.clone(),
                description: t.function.description.clone().unwrap_or_default(),
                input_schema: t.function.parameters.clone(),
            })
            .collect()
    });

    AnthropicRequest {
        model: req.model.clone(),
        max_tokens: req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        temperature: req.temperature,
        system,
        messages,
        tools,
        stream: if stream { Some(true) } else { None },
        thinking: thinking_budget.map(|budget_tokens| AnthropicThinking {
            kind: "enabled".to_string(),
            budget_tokens,
        }),
    }
}

/// Mappa i messaggi del contratto nel formato Anthropic: estrae il `system`
/// come campo separato, converte i tool-result in block `tool_result` (ruolo
/// `user`) e le tool-call assistant in block `tool_use`. Porting 1:1 di
/// `toAnthropicMessages` del TS.
fn to_anthropic_messages(req: &LlmRequest) -> (Option<String>, Vec<AnthropicMessageParam>) {
    let mut system: Option<String> = None;
    let mut messages: Vec<AnthropicMessageParam> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                system = Some(content_to_string(&msg.content));
            }
            "tool" => {
                messages.push(AnthropicMessageParam {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicBlock::ToolResult {
                        tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
                        content: content_to_string(&msg.content),
                    }]),
                });
            }
            "assistant" if msg.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) => {
                let mut blocks: Vec<AnthropicBlock> = Vec::new();
                // RI-PASSAGGIO extended thinking: se il turno assistant porta una
                // signature, il blocco `thinking` (anche con testo vuoto) va in
                // TESTA al content, prima dei tool_use. L'API Anthropic lo richiede
                // nei turni con tool, altrimenti HTTP 400 (parita' col Python
                // ~509-521, che mette il blocco thinking in testa a response.content).
                if let Some(signature) = &msg.thinking_signature {
                    if !signature.is_empty() {
                        blocks.push(AnthropicBlock::Thinking {
                            thinking: String::new(),
                            signature: signature.clone(),
                        });
                    }
                }
                if let Some(text) = assistant_text(&msg.content) {
                    if !text.is_empty() {
                        blocks.push(AnthropicBlock::Text { text });
                    }
                }
                if let Some(calls) = &msg.tool_calls {
                    for tc in calls {
                        // arguments e' una stringa JSON; se vuota/invalida si usa {}.
                        let input: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_else(|_| serde_json::json!({}));
                        blocks.push(AnthropicBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            input,
                        });
                    }
                }
                messages.push(AnthropicMessageParam {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(blocks),
                });
            }
            _ => {
                messages.push(AnthropicMessageParam {
                    role: msg.role.clone(),
                    content: AnthropicContent::Text(content_to_string(&msg.content)),
                });
            }
        }
    }

    (system, messages)
}

/// Estrae il testo "puro" di un messaggio assistant per il block `text`
/// iniziale: solo se il content e' una stringa (i blocchi strutturati non
/// vengono ri-serializzati come testo, parita' col TS).
fn assistant_text(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(s) => Some(s.clone()),
        MessageContent::Blocks(_) => Some(String::new()),
    }
}

/// Serializza il content di un messaggio a stringa (testo diretto o JSON dei
/// blocchi, come `JSON.stringify` del TS).
fn content_to_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => serde_json::to_string(blocks).unwrap_or_default(),
    }
}

/// Mappa una risposta Messages nel contratto [`LlmResponse`]: concatena i block
/// `text`, raccoglie i `tool_use` come [`LlmToolCall`], normalizza lo
/// `stop_reason` in `finish_reason`.
fn from_anthropic_message(
    resp: AnthropicMessage,
    model_used: String,
    latency_ms: u64,
) -> LlmResponse {
    let mut text = String::new();
    let mut tool_calls: Vec<LlmToolCall> = Vec::new();
    let mut reasoning = String::new();
    let mut thinking_signature: Option<String> = None;

    for block in resp.content {
        match block {
            AnthropicRespBlock::Text { text: t } => text.push_str(&t),
            AnthropicRespBlock::Thinking { thinking, signature } => {
                // Extended thinking: concatena il testo del ragionamento e
                // cattura la signature opaca (l'ultima vince) per il ri-passaggio
                // nei turni con tool (parita' col Python ~489-521).
                reasoning.push_str(&thinking);
                if signature.is_some() {
                    thinking_signature = signature;
                }
            }
            AnthropicRespBlock::ToolUse { id, name, input } => {
                tool_calls.push(LlmToolCall {
                    id,
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string()),
                    },
                });
            }
            AnthropicRespBlock::Other => {}
        }
    }

    LlmResponse {
        content: text,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        usage: LlmUsage {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
            cache_read_tokens: resp.usage.cache_read_input_tokens,
            cache_creation_tokens: resp.usage.cache_creation_input_tokens,
        },
        model_used,
        provider_used: "anthropic".to_string(),
        latency_ms,
        finish_reason: map_stop_reason(resp.stop_reason.as_deref()),
        privacy_rerouted: None,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        thinking_signature,
    }
}

/// Mappa lo `stop_reason` Anthropic ai valori canonici del contratto
/// (`finishReasonMap` del TS); valori non noti collassano a `stop`.
fn map_stop_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("end_turn") {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
    .to_string()
}

/// Detection billing specifica di Anthropic (estende [`super::is_billing_error`]
/// con i pattern propri del messaggio Anthropic: "plans & billing", "upgrade or
/// purchase credits", "billing required"). Punto unico (regola L): la detection
/// generica resta in `openai_compat`, qui si aggiunge solo il delta Anthropic.
pub fn is_anthropic_billing_error(msg: &str) -> bool {
    if super::is_billing_error(msg) {
        return true;
    }
    let m = msg.to_lowercase();
    m.contains("plans & billing")
        || m.contains("upgrade or purchase credits")
        || m.contains("billing required")
}

/// Parser SSE dell'API Messages. Gli eventi rilevanti:
///   - `content_block_delta` con `delta.type == "text_delta"` -> delta di testo;
///   - `message_delta` -> porta `usage.output_tokens` cumulativi e lo
///     `stop_reason` finale;
///   - `message_stop` -> chiude lo stream emettendo il chunk finale con usage.
///
/// Gli `input_tokens` arrivano nell'evento iniziale `message_start`; li
/// memorizziamo per riportarli nel chunk finale. Stateful ma autonomo dal
/// trasporto (testabile senza rete).
struct AnthropicSseParser {
    line_buf: String,
    pending: VecDeque<LlmStreamChunk>,
    model_used: String,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: Option<u32>,
    cache_creation_tokens: Option<u32>,
    finish_reason: Option<String>,
    /// Signature opaca del blocco thinking, catturata dai `signature_delta`. Non
    /// viaggia nei chunk (lo stream non porta la signature al client), ma resta
    /// disponibile per usi futuri / asserzioni di test.
    thinking_signature: Option<String>,
}

impl AnthropicSseParser {
    fn new(model_used: String) -> Self {
        Self {
            line_buf: String::new(),
            pending: VecDeque::new(),
            model_used,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            finish_reason: None,
            thinking_signature: None,
        }
    }

    /// Aggiunge byte al buffer ed estrae tutte le righe complete.
    fn push_bytes(&mut self, s: &str) {
        self.line_buf.push_str(s);
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].to_string();
            self.line_buf.drain(..=idx);
            self.parse_line(&line);
        }
    }

    /// Processa l'eventuale residuo nel buffer a fine stream.
    fn flush_leftover(&mut self) {
        let leftover = std::mem::take(&mut self.line_buf);
        for line in leftover.lines() {
            self.parse_line(line);
        }
    }

    /// Parsa una riga SSE. Nell'API Messages le righe utili sono `data: {json}`;
    /// la riga `event:` indica il tipo, ma il campo `type` e' replicato anche nel
    /// JSON del `data:`, quindi ci basiamo su quello (robusto a riordini).
    fn parse_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return,
        };
        if payload.is_empty() {
            return;
        }
        let event: AnthropicStreamEvent = match serde_json::from_str(payload) {
            Ok(e) => e,
            Err(_) => return,
        };
        self.handle_event(event);
    }

    fn handle_event(&mut self, event: AnthropicStreamEvent) {
        match event.kind.as_str() {
            "message_start" => {
                if let Some(msg) = event.message {
                    if let Some(u) = msg.usage {
                        self.input_tokens = u.input_tokens;
                        if u.cache_read_input_tokens.is_some() {
                            self.cache_read_tokens = u.cache_read_input_tokens;
                        }
                        if u.cache_creation_input_tokens.is_some() {
                            self.cache_creation_tokens = u.cache_creation_input_tokens;
                        }
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    match delta.kind.as_deref() {
                        Some("text_delta") => {
                            if let Some(text) = delta.text {
                                if !text.is_empty() {
                                    self.pending.push_back(LlmStreamChunk {
                                        delta: text,
                                        tool_call_delta: None,
                                        finish_reason: None,
                                        usage: None,
                                        provider_used: Some("anthropic".to_string()),
                                        model_used: Some(self.model_used.clone()),
                                        reasoning_delta: None,
                                    });
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            // Extended thinking: il ragionamento viaggia in
                            // `reasoning_delta` (parita' col Python ~712-713).
                            if let Some(thinking) = delta.thinking {
                                if !thinking.is_empty() {
                                    self.pending.push_back(LlmStreamChunk {
                                        delta: String::new(),
                                        tool_call_delta: None,
                                        finish_reason: None,
                                        usage: None,
                                        provider_used: Some("anthropic".to_string()),
                                        model_used: Some(self.model_used.clone()),
                                        reasoning_delta: Some(thinking),
                                    });
                                }
                            }
                        }
                        Some("signature_delta") => {
                            // La signature del blocco thinking arriva a fine
                            // ragionamento: la conserviamo per il chunk finale.
                            if let Some(sig) = delta.signature {
                                if !sig.is_empty() {
                                    self.thinking_signature = Some(sig);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = event.delta {
                    if let Some(reason) = delta.stop_reason {
                        self.finish_reason = Some(map_stop_reason(Some(&reason)));
                    }
                }
                if let Some(u) = event.usage {
                    self.output_tokens = u.output_tokens;
                }
            }
            "message_stop" => {
                self.pending.push_back(LlmStreamChunk {
                    delta: String::new(),
                    tool_call_delta: None,
                    finish_reason: Some(self.finish_reason.clone().unwrap_or_else(|| "stop".to_string())),
                    usage: Some(LlmUsage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        cache_read_tokens: self.cache_read_tokens,
                        cache_creation_tokens: self.cache_creation_tokens,
                    }),
                    provider_used: Some("anthropic".to_string()),
                    model_used: Some(self.model_used.clone()),
                    reasoning_delta: None,
                });
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Tipi wire (formato API Messages Anthropic). Separati dal contratto del
// gateway per non accoppiare il dialetto provider ai tipi pubblici.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    /// Blocco extended thinking (`{type:"enabled", budget_tokens}`). Presente
    /// solo quando il thinking e' attivo per la richiesta.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
}

/// Configurazione thinking nel body Anthropic.
#[derive(Debug, Serialize)]
struct AnthropicThinking {
    #[serde(rename = "type")]
    kind: String,
    budget_tokens: u32,
}

#[derive(Debug, Serialize)]
struct AnthropicMessageParam {
    role: String,
    content: AnthropicContent,
}

/// Content di un messaggio: stringa (caso semplice) o lista di block.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicBlock {
    #[serde(rename = "text")]
    Text { text: String },
    /// Blocco thinking ri-passato in un turno assistant precedente. Anthropic
    /// richiede `thinking` (puo' essere vuoto) + `signature` opaca; senza la
    /// signature l'API ritorna HTTP 400 nei turni con tool.
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    #[serde(default)]
    content: Vec<AnthropicRespBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

/// Block della risposta. `Other` cattura tipi non gestiti (es.
/// `redacted_thinking`) senza far fallire la deserializzazione.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicRespBlock {
    #[serde(rename = "text")]
    Text { text: String },
    /// Blocco di extended thinking: testo del ragionamento + signature opaca
    /// (entrambi necessari per il ri-passaggio nei turni con tool).
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
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

#[derive(Debug, Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    /// Token serviti da prompt cache (presenti solo con prompt caching attivo).
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce).
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

/// Evento SSE generico: `type` discrimina, gli altri campi sono opzionali in
/// base al tipo. Un solo struct tollerante invece di un enum per evento (parsing
/// robusto a campi extra).
#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<AnthropicStreamMessage>,
    #[serde(default)]
    delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamMessage {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamDelta {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    /// Delta del testo di extended thinking (`thinking_delta`).
    #[serde(default)]
    thinking: Option<String>,
    /// Signature opaca del blocco thinking (`signature_delta`).
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, LlmToolDefinition, RequestMetadata, ToolFunctionDef};

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            tenant_id: "t".to_string(),
            user_id: "u".to_string(),
            request_id: "r".to_string(),
            sensitivity_tier: 0,
            feature: "f".to_string(),
        }
    }

    fn msg(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn capacita_dichiarate() {
        let p = AnthropicProvider::new(Client::new(), "key", None);
        assert_eq!(p.name(), "anthropic");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 200_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }

    #[test]
    fn system_estratto_come_campo_separato() {
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![msg("system", "sei un assistente"), msg("user", "ciao")],
            temperature: Some(0.3),
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let body = build_request_body(&req, false, None);
        let json = serde_json::to_value(&body).unwrap();

        // system NON e' tra i messages, ma campo a se'.
        assert_eq!(json["system"], "sei un assistente");
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "ciao");
        // max_tokens obbligatorio: applicato il default quando assente.
        assert_eq!(json["max_tokens"], DEFAULT_MAX_TOKENS);
        // stream non richiesto: campo assente.
        assert!(json.get("stream").is_none());
    }

    #[test]
    fn tool_message_diventa_block_tool_result_ruolo_user() {
        let mut tool_msg = msg("tool", "risultato del tool");
        tool_msg.tool_call_id = Some("call_42".to_string());
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![tool_msg],
            temperature: None,
            max_tokens: Some(100),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, false, None)).unwrap();

        let m = &json["messages"][0];
        assert_eq!(m["role"], "user");
        assert_eq!(m["content"][0]["type"], "tool_result");
        assert_eq!(m["content"][0]["tool_use_id"], "call_42");
        assert_eq!(m["content"][0]["content"], "risultato del tool");
    }

    #[test]
    fn assistant_con_tool_calls_diventa_block_tool_use() {
        let mut a = msg("assistant", "");
        a.tool_calls = Some(vec![LlmToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "do_thing".to_string(),
                arguments: r#"{"a":1}"#.to_string(),
            },
        }]);
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![a],
            temperature: None,
            max_tokens: Some(100),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, false, None)).unwrap();

        let m = &json["messages"][0];
        assert_eq!(m["role"], "assistant");
        // content vuoto saltato -> solo il block tool_use.
        assert_eq!(m["content"][0]["type"], "tool_use");
        assert_eq!(m["content"][0]["id"], "call_1");
        assert_eq!(m["content"][0]["name"], "do_thing");
        assert_eq!(m["content"][0]["input"]["a"], 1);
    }

    #[test]
    fn tools_mappati_su_input_schema() {
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![msg("user", "ciao")],
            temperature: None,
            max_tokens: Some(100),
            tools: Some(vec![LlmToolDefinition {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "search".to_string(),
                    description: Some("cerca".to_string()),
                    parameters: serde_json::json!({"type": "object"}),
                    strict: None,
                },
            }]),
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, false, None)).unwrap();

        let t = &json["tools"][0];
        assert_eq!(t["name"], "search");
        assert_eq!(t["description"], "cerca");
        assert_eq!(t["input_schema"]["type"], "object");
    }

    #[test]
    fn deserializza_response_testuale() {
        let raw = r#"{
            "content": [{"type": "text", "text": "ciao mondo"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 12, "output_tokens": 3}
        }"#;
        let parsed: AnthropicMessage = serde_json::from_str(raw).unwrap();
        let resp = from_anthropic_message(parsed, "claude-x".to_string(), 50);

        assert_eq!(resp.content, "ciao mondo");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 12);
        assert_eq!(resp.usage.output_tokens, 3);
        assert_eq!(resp.provider_used, "anthropic");
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn deserializza_response_con_tool_use() {
        let raw = r#"{
            "content": [
                {"type": "text", "text": "uso un tool"},
                {"type": "tool_use", "id": "tu_1", "name": "calc", "input": {"x": 2}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 9}
        }"#;
        let parsed: AnthropicMessage = serde_json::from_str(raw).unwrap();
        let resp = from_anthropic_message(parsed, "claude-x".to_string(), 1);

        assert_eq!(resp.content, "uso un tool");
        assert_eq!(resp.finish_reason, "tool_calls");
        let calls = resp.tool_calls.expect("tool_calls presenti");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tu_1");
        assert_eq!(calls[0].function.name, "calc");
        assert_eq!(calls[0].function.arguments, r#"{"x":2}"#);
    }

    #[test]
    fn block_sconosciuto_non_rompe_il_parsing() {
        // Un block "thinking" non gestito deve essere ignorato, non far fallire.
        let raw = r#"{
            "content": [
                {"type": "thinking", "thinking": "ragionamento interno"},
                {"type": "text", "text": "risposta"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }"#;
        let parsed: AnthropicMessage = serde_json::from_str(raw).unwrap();
        let resp = from_anthropic_message(parsed, "m".to_string(), 0);
        assert_eq!(resp.content, "risposta");
    }

    #[test]
    fn stop_reason_mappato() {
        assert_eq!(map_stop_reason(Some("end_turn")), "stop");
        assert_eq!(map_stop_reason(Some("max_tokens")), "length");
        assert_eq!(map_stop_reason(Some("tool_use")), "tool_calls");
        assert_eq!(map_stop_reason(Some("boh")), "stop");
        assert_eq!(map_stop_reason(None), "stop");
    }

    #[test]
    fn sse_text_delta_emette_chunk() {
        let mut p = AnthropicSseParser::new("m".to_string());
        p.parse_line(
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#,
        );
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "Hel");
        assert_eq!(p.pending[0].provider_used.as_deref(), Some("anthropic"));
    }

    #[test]
    fn sse_message_stop_riporta_usage_e_finish() {
        let mut p = AnthropicSseParser::new("m".to_string());
        // message_start porta input_tokens.
        p.parse_line(
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":20,"output_tokens":0}}}"#,
        );
        // message_delta porta output_tokens e stop_reason.
        p.parse_line(
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        );
        // message_stop emette il chunk finale.
        p.parse_line(r#"data: {"type":"message_stop"}"#);

        let last = p.pending.pop_back().expect("chunk finale");
        assert_eq!(last.delta, "");
        assert_eq!(last.finish_reason.as_deref(), Some("stop"));
        let usage = last.usage.expect("usage finale");
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 7);
    }

    #[test]
    fn sse_riga_parziale_gestita() {
        let mut p = AnthropicSseParser::new("m".to_string());
        // Prima meta' della riga (senza newline finale): nessun chunk.
        p.push_bytes(r#"data: {"type":"content_block_delta","delta":{"type":"text"#);
        assert_eq!(p.pending.len(), 0);
        // Seconda meta' che completa la riga.
        p.push_bytes("_delta\",\"text\":\"ok\"}}\n");
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "ok");
    }

    #[test]
    fn billing_error_anthropic_specifico() {
        assert!(is_anthropic_billing_error(
            "Your credit balance is too low to access the API"
        ));
        assert!(is_anthropic_billing_error("Please go to Plans & Billing"));
        assert!(is_anthropic_billing_error(
            "Upgrade or purchase credits to continue"
        ));
        assert!(is_anthropic_billing_error("billing required"));
        // Pattern generico ancora riconosciuto via delega.
        assert!(is_anthropic_billing_error("insufficient_quota"));
        assert!(!is_anthropic_billing_error("rate limit exceeded"));
    }

    // --- Extended thinking (passo 1) ---------------------------------------

    fn req_thinking(enabled: bool, budget: Option<u32>, max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![msg("user", "ciao")],
            temperature: None,
            max_tokens,
            tools: None,
            response_format: None,
            stream: None,
            thinking: Some(crate::types::ThinkingConfig {
                enabled,
                budget_tokens: budget,
            }),
            metadata: metadata(),
        }
    }

    #[test]
    fn request_con_thinking_aggiunge_blocco_enabled() {
        // budget esplicito 1024 < max_tokens 8000 -> thinking attivo.
        let req = req_thinking(true, Some(1024), Some(8000));
        let budget = resolve_thinking_budget(&req, 2048);
        assert_eq!(budget, Some(1024));
        let json = serde_json::to_value(build_request_body(&req, false, budget)).unwrap();
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 1024);
    }

    #[test]
    fn request_senza_thinking_non_aggiunge_blocco() {
        let req = req_thinking(false, Some(1024), Some(8000));
        let budget = resolve_thinking_budget(&req, 2048);
        assert_eq!(budget, None);
        let json = serde_json::to_value(build_request_body(&req, false, budget)).unwrap();
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn budget_thinking_oltre_max_tokens_disattiva() {
        // Guardia del Python (max_tokens > thinking_budget): budget >= max_tokens
        // -> thinking disattivato per evitare HTTP 400.
        let req = req_thinking(true, Some(5000), Some(4000));
        assert_eq!(resolve_thinking_budget(&req, 2048), None);
        // Budget configurato usato quando la request non lo specifica.
        let req2 = req_thinking(true, None, Some(8000));
        assert_eq!(resolve_thinking_budget(&req2, 2048), Some(2048));
    }

    #[test]
    fn deserializza_response_con_thinking_e_signature() {
        let raw = r#"{
            "content": [
                {"type": "thinking", "thinking": "rifletto sul problema", "signature": "sig-abc123"},
                {"type": "text", "text": "ecco la risposta"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 4, "cache_read_input_tokens": 6}
        }"#;
        let parsed: AnthropicMessage = serde_json::from_str(raw).unwrap();
        let resp = from_anthropic_message(parsed, "claude-x".to_string(), 7);

        assert_eq!(resp.content, "ecco la risposta");
        assert_eq!(resp.reasoning.as_deref(), Some("rifletto sul problema"));
        assert_eq!(resp.thinking_signature.as_deref(), Some("sig-abc123"));
        assert_eq!(resp.usage.cache_read_tokens, Some(6));
        // I cache_creation non presenti -> None.
        assert_eq!(resp.usage.cache_creation_tokens, None);
    }

    #[test]
    fn response_senza_thinking_ha_reasoning_none() {
        let raw = r#"{
            "content": [{"type": "text", "text": "solo testo"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }"#;
        let parsed: AnthropicMessage = serde_json::from_str(raw).unwrap();
        let resp = from_anthropic_message(parsed, "m".to_string(), 0);
        assert!(resp.reasoning.is_none());
        assert!(resp.thinking_signature.is_none());
    }

    #[test]
    fn sse_thinking_delta_emette_reasoning_delta() {
        let mut p = AnthropicSseParser::new("m".to_string());
        p.parse_line(
            r#"data: {"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"penso..."}}"#,
        );
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].reasoning_delta.as_deref(), Some("penso..."));
        // Il delta testuale resta vuoto sul chunk di reasoning.
        assert_eq!(p.pending[0].delta, "");
    }

    #[test]
    fn sse_signature_delta_catturata_non_emette_chunk() {
        let mut p = AnthropicSseParser::new("m".to_string());
        p.parse_line(
            r#"data: {"type":"content_block_delta","delta":{"type":"signature_delta","signature":"sig-stream"}}"#,
        );
        // La signature non viaggia in un chunk, ma e' conservata nel parser.
        assert_eq!(p.pending.len(), 0);
        assert_eq!(p.thinking_signature.as_deref(), Some("sig-stream"));
    }

    #[test]
    fn round_trip_signature_assistant_la_reinclude() {
        // Un turno assistant con thinking_signature + tool_call deve produrre il
        // block `thinking` (con signature) in TESTA al content, prima del tool_use.
        let mut a = msg("assistant", "");
        a.thinking_signature = Some("sig-round-trip".to_string());
        a.tool_calls = Some(vec![LlmToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "do_thing".to_string(),
                arguments: r#"{"a":1}"#.to_string(),
            },
        }]);
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![a],
            temperature: None,
            max_tokens: Some(100),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, false, None)).unwrap();

        let m = &json["messages"][0];
        assert_eq!(m["role"], "assistant");
        // Primo block: thinking con la signature ri-passata.
        assert_eq!(m["content"][0]["type"], "thinking");
        assert_eq!(m["content"][0]["signature"], "sig-round-trip");
        assert_eq!(m["content"][0]["thinking"], "");
        // Secondo block: il tool_use.
        assert_eq!(m["content"][1]["type"], "tool_use");
        assert_eq!(m["content"][1]["id"], "call_1");
    }

    #[test]
    fn assistant_senza_signature_non_reinclude_thinking() {
        // Round-trip no-op: assente la signature, nessun block thinking spurio.
        let mut a = msg("assistant", "testo");
        a.tool_calls = Some(vec![LlmToolCall {
            id: "c1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "f".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let req = LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![a],
            temperature: None,
            max_tokens: Some(100),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, false, None)).unwrap();
        let m = &json["messages"][0];
        // Primo block e' il testo (non un thinking).
        assert_eq!(m["content"][0]["type"], "text");
        assert_eq!(m["content"][1]["type"], "tool_use");
    }
}
