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
use crate::providers::openai_compat::parse_models_response;
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

/// Beta header richiesto dal TTL cache esteso a 1h (parita' col Python
/// `_system_cache_control`, commento ~124). Inviato via `anthropic-beta` solo
/// quando il caching e' attivo con TTL 1h.
const EXTENDED_CACHE_BETA: &str = "extended-cache-ttl-2025-04-11";

/// Chiave settings (regola G/L) del TTL della prompt cache di sistema Anthropic.
/// Unica fonte di verita' condivisa col brain Python (mig 0125): valori `5m`,
/// `1h` (default) o `off` per disattivare il caching. Il gateway Rust segue la
/// regola G stretta: nessun override env, solo DB.
const CACHE_TTL_SETTING: &str = "anthropic_system_cache_ttl";

/// TTL cache usato SOLO se il DB e' irraggiungibile (fallback graceful
/// documentato, regola G). Allineato al default `1h` della mig 0125: il system
/// prompt cambia raramente, 1h massimizza il cache hit fra turni distanti.
const CACHE_TTL_DB_DOWN_FALLBACK: &str = "1h";

/// Numero minimo di messaggi nella history sotto cui non si applica il
/// breakpoint cache sulla history (parita' col Python ~370: `>= 6`). Sotto
/// questa soglia la cache del solo system gia' copre il prefisso stabile.
const CACHE_HISTORY_MIN_MESSAGES: usize = 6;

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
    cache_ttl: TtlCache<(), CacheTtl>,
}

/// Stato della prompt cache di sistema, risolto dal setting
/// `anthropic_system_cache_ttl` (regola G). `Off` = caching disattivato:
/// nessun `cache_control` sui blocchi (parita' col brain quando il setting non
/// abilita il caching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    /// Caching disattivato: niente `cache_control`.
    Off,
    /// Cache ephemeral con TTL default Anthropic (5 minuti).
    FiveMinutes,
    /// Cache ephemeral con TTL esteso 1 ora (richiede il beta header).
    OneHour,
}

impl CacheTtl {
    /// Mappa il valore testuale del setting (`5m`/`1h`/`off`) a [`CacheTtl`].
    /// Valori non riconosciuti collassano su `Off` per non attivare il caching
    /// con parametri ignoti (fail-safe, niente magic default attivante).
    fn parse(raw: &str) -> Self {
        match raw.trim() {
            "5m" => CacheTtl::FiveMinutes,
            "1h" => CacheTtl::OneHour,
            _ => CacheTtl::Off,
        }
    }

    /// `true` se il caching e' attivo (un breakpoint va emesso).
    fn is_active(self) -> bool {
        !matches!(self, CacheTtl::Off)
    }

    /// Blocco `cache_control` per il SYSTEM prompt: il TTL 1h aggiunge il campo
    /// `ttl:"1h"` (parita' col Python `_system_cache_control`). `None` se il
    /// caching e' spento.
    fn system_cache_control(self) -> Option<serde_json::Value> {
        match self {
            CacheTtl::Off => None,
            CacheTtl::FiveMinutes => Some(serde_json::json!({ "type": "ephemeral" })),
            CacheTtl::OneHour => {
                Some(serde_json::json!({ "type": "ephemeral", "ttl": "1h" }))
            }
        }
    }

    /// `cache_control` per i breakpoint sulla HISTORY: sempre ephemeral 5m
    /// (la storia muta ad ogni turno, parita' col Python ~383). `None` se spento.
    fn history_cache_control(self) -> Option<serde_json::Value> {
        if self.is_active() {
            Some(serde_json::json!({ "type": "ephemeral" }))
        } else {
            None
        }
    }
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
            cache_ttl: TtlCache::new(SETTINGS_TTL),
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

    /// TTL della prompt cache di sistema dai settings (cache TTL 60s, regola
    /// G/L). Se il DB e' irraggiungibile ricade sul fallback documentato
    /// (`CACHE_TTL_DB_DOWN_FALLBACK`). Senza accesso DB (test di mappatura) il
    /// caching resta `Off`: i test costruiscono lo stato cache esplicitamente.
    async fn configured_cache_ttl(&self) -> CacheTtl {
        if let Some(c) = self.cache_ttl.get(&()) {
            return c;
        }
        let Some(db) = self.db.as_ref() else {
            return CacheTtl::Off;
        };
        let raw = nexus_auth::get_setting(db, CACHE_TTL_SETTING)
            .await
            .unwrap_or_else(|| CACHE_TTL_DB_DOWN_FALLBACK.to_string());
        let ttl = CacheTtl::parse(&raw);
        self.cache_ttl.insert((), ttl);
        ttl
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
        let cache_ttl = self.configured_cache_ttl().await;
        let body = build_request_body(req, false, thinking_budget, cache_ttl);
        let start = Instant::now();

        let mut builder = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(beta) = beta_header(thinking_budget.is_some(), cache_ttl) {
            builder = builder.header("anthropic-beta", beta);
        }
        let resp = builder.json(&body).send().await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: il body d'errore non contiene prompt utente ma dettagli
            // del provider; lo propaghiamo al caller (il cooldown della Fase 3
            // riconosce il billing via `is_billing_error`), senza loggarlo qui.
            let text = resp.text().await.unwrap_or_default();
            return Err(anthropic_http_error(status.as_u16(), text).into());
        }

        let parsed: AnthropicMessage = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_anthropic_message(parsed, req.model.clone(), latency_ms))
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let configured = self.configured_thinking_budget().await;
        let thinking_budget = resolve_thinking_budget(req, configured);
        let cache_ttl = self.configured_cache_ttl().await;
        let body = build_request_body(req, true, thinking_budget, cache_ttl);

        let mut builder = self
            .http
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(beta) = beta_header(thinking_budget.is_some(), cache_ttl) {
            builder = builder.header("anthropic-beta", beta);
        }
        let resp = builder.json(&body).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anthropic_http_error(status.as_u16(), text).into());
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

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // Anthropic: `GET {base_url}/models` con header `x-api-key` +
        // `anthropic-version` (non Bearer). La risposta ha la stessa forma del
        // dialetto OpenAI (`{ "data": [{ "id": ... }] }`), quindi il parsing
        // delega al punto unico `parse_models_response` (regola L).
        let url = format!("{}/models", self.base_url);
        let resp = self
            .http
            .get(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            // Errore strutturato anche sulla lista modelli (regola M): status +
            // codice, mai testo da classificare.
            let text = resp.text().await.unwrap_or_default();
            return Err(anthropic_http_error(status.as_u16(), text).into());
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(parse_models_response(&body))
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

/// Valore dell'header `anthropic-beta` da inviare: cumula i beta necessari
/// separati da virgola. `None` se non serve alcun beta. Il thinking richiede
/// `interleaved-thinking`; il caching con TTL 1h richiede `extended-cache-ttl`.
fn beta_header(thinking_active: bool, cache_ttl: CacheTtl) -> Option<String> {
    let mut betas: Vec<&str> = Vec::new();
    if thinking_active {
        betas.push(THINKING_BETA);
    }
    if cache_ttl == CacheTtl::OneHour {
        betas.push(EXTENDED_CACHE_BETA);
    }
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

/// Costruisce il corpo JSON della request Messages a partire dal contratto LLM.
/// `thinking_budget` e' il budget effettivo gia' risolto (vedi
/// [`resolve_thinking_budget`]): `Some` => blocco thinking nel body.
/// `cache_ttl` governa i breakpoint di prompt cache (regola G): quando attivo,
/// il system prompt e l'ultimo blocco stabile della history portano
/// `cache_control`.
fn build_request_body(
    req: &LlmRequest,
    stream: bool,
    thinking_budget: Option<u32>,
    cache_ttl: CacheTtl,
) -> AnthropicRequest {
    let (system_text, mut messages) = to_anthropic_messages(req);

    // Breakpoint cache su SYSTEM: blocco text strutturato con cache_control
    // (parita' col Python ~263-270). Guardia HTTP 400: niente cache_control su
    // testo vuoto -> in quel caso il system resta una stringa semplice.
    let system = build_system_field(system_text, cache_ttl);

    // Breakpoint cache su HISTORY: l'ultimo blocco stabile (ultimo messaggio
    // `user`) riceve cache_control ephemeral, solo se la history e' abbastanza
    // lunga da giustificarlo (parita' col Python ~370 `>= 6`).
    if cache_ttl.is_active() {
        apply_history_cache_breakpoint(&mut messages, cache_ttl);
    }

    let tools: Option<Vec<AnthropicTool>> = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.function.name.clone(),
                description: t.function.description.clone().unwrap_or_default(),
                input_schema: t.function.parameters.clone(),
            })
            .collect()
    });

    // tool_choice mappato al dialetto Anthropic (`{type:any|tool|auto}`) via il
    // punto unico (regola L). Inviato solo con tool presenti e vincolo
    // riconosciuto; `none` viene omesso (Anthropic non lo supporta).
    let tool_choice = req
        .tool_choice
        .as_ref()
        .filter(|_| tools.is_some())
        .and_then(super::tool_choice::to_anthropic);

    AnthropicRequest {
        model: req.model.clone(),
        max_tokens: req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        temperature: req.temperature,
        system,
        messages,
        tools,
        tool_choice,
        stream: if stream { Some(true) } else { None },
        thinking: thinking_budget.map(|budget_tokens| AnthropicThinking {
            kind: "enabled".to_string(),
            budget_tokens,
        }),
    }
}

/// Costruisce il campo `system` della request. Quando il caching e' attivo e il
/// system non e' vuoto, lo avvolge in un blocco text con `cache_control` (array
/// di blocchi, formato accettato da Anthropic). Altrimenti resta una stringa
/// semplice (o `None` se assente). Guardia: niente `cache_control` su testo
/// vuoto (Anthropic ritorna HTTP 400).
fn build_system_field(system_text: Option<String>, cache_ttl: CacheTtl) -> Option<AnthropicSystem> {
    let text = system_text?;
    match cache_ttl.system_cache_control() {
        Some(cc) if !text.is_empty() => Some(AnthropicSystem::Blocks(vec![AnthropicSystemBlock {
            kind: "text".to_string(),
            text,
            cache_control: Some(cc),
        }])),
        _ => Some(AnthropicSystem::Text(text)),
    }
}

/// Applica il breakpoint cache all'ultimo blocco STABILE della history. Il
/// breakpoint va su un messaggio `user` che si ripeta identico tra turni
/// successivi: il terzultimo `user` quando disponibile (parita' col Python
/// `_apply_cache_breakpoint`, ~398), con fallback all'ultimo `user` se ce ne
/// sono meno di tre. Mettere il breakpoint sul terzultimo (non sull'ultimo, che
/// e' il turno corrente mutevole) massimizza il cache hit rate.
///
/// Il `cache_control` ephemeral va sull'ULTIMO content block del messaggio. I
/// messaggi a content stringa vengono promossi a blocco text con cache_control
/// (saltando quelli vuoti, guardia HTTP 400).
fn apply_history_cache_breakpoint(messages: &mut [AnthropicMessageParam], cache_ttl: CacheTtl) {
    if messages.len() < CACHE_HISTORY_MIN_MESSAGES {
        return;
    }
    let Some(cc) = cache_ttl.history_cache_control() else {
        return;
    };
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    // Terzultimo user (blocco stabile); fallback all'ultimo se < 3 user.
    let idx = if user_indices.len() >= 3 {
        user_indices[user_indices.len() - 3]
    } else if let Some(&last) = user_indices.last() {
        last
    } else {
        return;
    };
    match &mut messages[idx].content {
        AnthropicContent::Text(s) => {
            if !s.is_empty() {
                let text = std::mem::take(s);
                messages[idx].content = AnthropicContent::Blocks(vec![AnthropicBlock::Text {
                    text,
                    cache_control: Some(cc),
                }]);
            }
        }
        AnthropicContent::Blocks(blocks) => {
            if let Some(last) = blocks.last_mut() {
                last.set_cache_control(cc);
            }
        }
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
                        blocks.push(AnthropicBlock::Text {
                            text,
                            cache_control: None,
                        });
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
                // Messaggi user/altro: se il content porta blocchi immagine, li
                // mappiamo nel formato nativo Anthropic (block `image`), cosi' la
                // capability vision e' preservata. Altrimenti content stringa.
                let content = match &msg.content {
                    MessageContent::Blocks(blocks)
                        if blocks.iter().any(|b| b.kind == "image_url") =>
                    {
                        AnthropicContent::Blocks(blocks_to_anthropic(blocks))
                    }
                    other => AnthropicContent::Text(content_to_string(other)),
                };
                messages.push(AnthropicMessageParam {
                    role: msg.role.clone(),
                    content,
                });
            }
        }
    }

    (system, messages)
}

/// Mappa i blocchi del contratto nei block Anthropic. I blocchi `image_url`
/// diventano block `image` con `source` nativo:
///   - data URI `data:<mime>;base64,<dati>` -> `{type:"base64", media_type, data}`;
///   - URL http/https -> `{type:"url", url}`.
/// I blocchi testuali diventano block `text`. I `tool_result` qui inattesi nel
/// content vengono resi come testo per non perderne il payload.
fn blocks_to_anthropic(blocks: &[crate::types::LlmContentBlock]) -> Vec<AnthropicBlock> {
    let mut out: Vec<AnthropicBlock> = Vec::new();
    for b in blocks {
        match b.kind.as_str() {
            "image_url" => {
                if let Some(url) = b
                    .image_url
                    .as_ref()
                    .and_then(|iu| iu.get("url"))
                    .and_then(|u| u.as_str())
                {
                    out.push(AnthropicBlock::Image {
                        source: image_url_to_source(url),
                        cache_control: None,
                    });
                }
            }
            "text" => out.push(AnthropicBlock::Text {
                text: b.text.clone().unwrap_or_default(),
                cache_control: None,
            }),
            _ => {
                if let Some(c) = &b.content {
                    out.push(AnthropicBlock::Text {
                        text: c.clone(),
                        cache_control: None,
                    });
                }
            }
        }
    }
    out
}

/// Converte la `url` di un blocco immagine nel `source` Anthropic. Data URI
/// base64 -> source `base64`; qualunque altra URL -> source `url` (l'API
/// Anthropic scarica l'immagine). Data URI malformato -> source `url` grezza.
fn image_url_to_source(url: &str) -> serde_json::Value {
    if let Some((media_type, data)) = parse_data_uri(url) {
        serde_json::json!({
            "type": "base64",
            "media_type": media_type,
            "data": data,
        })
    } else {
        serde_json::json!({ "type": "url", "url": url })
    }
}

/// Estrae `(media_type, base64)` da un data URI `data:<mime>;base64,<dati>`.
/// `None` se non e' un data URI base64 ben formato.
fn parse_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let meta = meta.strip_suffix(";base64")?;
    if meta.is_empty() {
        return None;
    }
    Some((meta.to_string(), data.to_string()))
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
                    // Anthropic firma il blocco thinking a livello di messaggio
                    // (`thinking_signature`), non per-call.
                    thought_signature: None,
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
        citations: None,
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

/// Detection billing specifica di Anthropic. Quirk isolato nell'adapter: traduce
/// pattern Anthropic in codice `"billing"` su [`ProviderHttpError`] (regola M).
pub fn is_anthropic_billing_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("insufficient_quota")
        || m.contains("credit balance")
        || m.contains("payment required")
        || m.contains("plans & billing")
        || m.contains("upgrade or purchase credits")
        || m.contains("billing required")
}

/// Costruisce un [`super::ProviderHttpError`] per Anthropic. Anthropic NON espone
/// un codice d'errore strutturato per il credito (il segnale e' solo nel testo del
/// messaggio, es. 400 "Your credit balance is too low"): qui — e SOLO qui, dentro
/// l'adapter che conosce il formato Anthropic — quel quirk viene tradotto in un
/// codice `"billing"`. Cosi' il classificatore generico
/// (`classify_provider_error`) resta DETERMINISTICO su status+codice (regola H):
/// il match testuale e' confinato al provider, non sparso nel punto di decisione.
fn anthropic_http_error(status: u16, body: String) -> super::ProviderHttpError {
    let mut e = super::ProviderHttpError::from_response("anthropic", status, body);
    if e.code.is_none() && is_anthropic_billing_error(&e.message) {
        e.code = Some("billing".to_string());
    }
    e
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
    system: Option<AnthropicSystem>,
    messages: Vec<AnthropicMessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    /// Vincolo di scelta tool nel formato Anthropic (`{type:any|tool|auto}`).
    /// Omesso quando assente o quando il vincolo originale era `none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
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

/// Campo `system` della request: stringa semplice o array di blocchi text con
/// `cache_control` (prompt caching). L'enum untagged serializza al valore JSON
/// atteso (stringa o array).
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicSystem {
    Text(String),
    Blocks(Vec<AnthropicSystemBlock>),
}

/// Blocco text del campo `system` con eventuale `cache_control` (breakpoint
/// prompt cache sul system prompt).
#[derive(Debug, Serialize)]
struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<serde_json::Value>,
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
    Text {
        text: String,
        /// Breakpoint prompt cache su questo blocco (ephemeral). Omesso quando
        /// assente.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<serde_json::Value>,
    },
    /// Blocco immagine nel formato nativo Anthropic. `source` e' base64
    /// (`{type:"base64", media_type, data}`) o url (`{type:"url", url}`).
    #[serde(rename = "image")]
    Image {
        source: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<serde_json::Value>,
    },
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

impl AnthropicBlock {
    /// Imposta il `cache_control` sui block che lo supportano (text/image). Sui
    /// block thinking/tool_use/tool_result e' un no-op (Anthropic non vi accetta
    /// breakpoint cache).
    fn set_cache_control(&mut self, cc: serde_json::Value) {
        match self {
            AnthropicBlock::Text { cache_control, .. }
            | AnthropicBlock::Image { cache_control, .. } => *cache_control = Some(cc),
            _ => {}
        }
    }
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
            reasoning: None,
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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let body = build_request_body(&req, false, None, CacheTtl::Off);
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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();

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
            thought_signature: None,
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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();

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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();

        let t = &json["tools"][0];
        assert_eq!(t["name"], "search");
        assert_eq!(t["description"], "cerca");
        assert_eq!(t["input_schema"]["type"], "object");
    }

    fn search_tool() -> LlmToolDefinition {
        LlmToolDefinition {
            kind: "function".to_string(),
            function: ToolFunctionDef {
                name: "search".to_string(),
                description: Some("cerca".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }
    }

    fn req_tool_choice(choice: serde_json::Value, with_tools: bool) -> LlmRequest {
        LlmRequest {
            model: "claude-x".to_string(),
            messages: vec![msg("user", "modifica il file")],
            temperature: None,
            max_tokens: Some(100),
            tools: if with_tools {
                Some(vec![search_tool()])
            } else {
                None
            },
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: Some(choice),
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        }
    }

    #[test]
    fn tool_choice_required_diventa_type_any() {
        // "required" lato contratto -> {"type":"any"} lato Anthropic: e' questo
        // che OBBLIGA il modello a chiamare un tool (fix del bug tool_choice
        // droppato dal gateway).
        let req = req_tool_choice(serde_json::json!("required"), true);
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        assert_eq!(json["tool_choice"]["type"], "any");

        // Oggetto funzione -> {"type":"tool","name":X}.
        let req2 = req_tool_choice(
            serde_json::json!({"type": "function", "function": {"name": "search"}}),
            true,
        );
        let json2 =
            serde_json::to_value(build_request_body(&req2, false, None, CacheTtl::Off)).unwrap();
        assert_eq!(json2["tool_choice"]["type"], "tool");
        assert_eq!(json2["tool_choice"]["name"], "search");

        // "auto" -> {"type":"auto"}.
        let req3 = req_tool_choice(serde_json::json!("auto"), true);
        let json3 =
            serde_json::to_value(build_request_body(&req3, false, None, CacheTtl::Off)).unwrap();
        assert_eq!(json3["tool_choice"]["type"], "auto");
    }

    #[test]
    fn tool_choice_none_e_senza_tools_omettono_il_campo() {
        // "none" non esiste lato Anthropic: campo omesso.
        let req = req_tool_choice(serde_json::json!("none"), true);
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        assert!(json.get("tool_choice").is_none());

        // tool_choice senza tools: campo omesso.
        let req2 = req_tool_choice(serde_json::json!("required"), false);
        let json2 =
            serde_json::to_value(build_request_body(&req2, false, None, CacheTtl::Off)).unwrap();
        assert!(json2.get("tool_choice").is_none());
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
                mandatory: false,
            }),
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        }
    }

    #[test]
    fn request_con_thinking_aggiunge_blocco_enabled() {
        // budget esplicito 1024 < max_tokens 8000 -> thinking attivo.
        let req = req_thinking(true, Some(1024), Some(8000));
        let budget = resolve_thinking_budget(&req, 2048);
        assert_eq!(budget, Some(1024));
        let json = serde_json::to_value(build_request_body(&req, false, budget, CacheTtl::Off)).unwrap();
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 1024);
    }

    #[test]
    fn request_senza_thinking_non_aggiunge_blocco() {
        let req = req_thinking(false, Some(1024), Some(8000));
        let budget = resolve_thinking_budget(&req, 2048);
        assert_eq!(budget, None);
        let json = serde_json::to_value(build_request_body(&req, false, budget, CacheTtl::Off)).unwrap();
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
            thought_signature: None,
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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();

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
            thought_signature: None,
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
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        };
        let json = serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        let m = &json["messages"][0];
        // Primo block e' il testo (non un thinking).
        assert_eq!(m["content"][0]["type"], "text");
        assert_eq!(m["content"][1]["type"], "tool_use");
    }

    // --- Vision: block immagine nativo (passo 3) ---------------------------

    fn user_image(url: &str) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![
                crate::types::LlmContentBlock {
                    kind: "text".to_string(),
                    text: Some("descrivi".to_string()),
                    image_url: None,
                    tool_use_id: None,
                    content: None,
                },
                crate::types::LlmContentBlock {
                    kind: "image_url".to_string(),
                    text: None,
                    image_url: Some(serde_json::json!({ "url": url })),
                    tool_use_id: None,
                    content: None,
                },
            ]),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }
    }

    fn req_msgs(messages: Vec<LlmMessage>) -> LlmRequest {
        LlmRequest {
            model: "claude-x".to_string(),
            messages,
            temperature: None,
            max_tokens: Some(1024),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
            run_timeout_secs: None,
        }
    }

    #[test]
    fn vision_data_uri_diventa_source_base64() {
        let req = req_msgs(vec![user_image("data:image/png;base64,QUJD")]);
        let json =
            serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        let blocks = json["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "descrivi");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "QUJD");
    }

    #[test]
    fn vision_url_http_diventa_source_url() {
        let req = req_msgs(vec![user_image("https://example.com/x.webp")]);
        let json =
            serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        let blocks = json["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "url");
        assert_eq!(blocks[1]["source"]["url"], "https://example.com/x.webp");
    }

    #[test]
    fn parse_data_uri_anthropic() {
        assert_eq!(
            parse_data_uri("data:image/jpeg;base64,ZZZ"),
            Some(("image/jpeg".to_string(), "ZZZ".to_string()))
        );
        assert!(parse_data_uri("https://x/y").is_none());
    }

    // --- Prompt cache: cache_control sui breakpoint (passo 3) --------------

    #[test]
    fn cache_off_nessun_cache_control_sul_system() {
        let req = req_msgs(vec![msg("system", "istruzioni di sistema"), msg("user", "ciao")]);
        let json =
            serde_json::to_value(build_request_body(&req, false, None, CacheTtl::Off)).unwrap();
        // Caching spento: system resta una stringa semplice, niente cache_control.
        assert_eq!(json["system"], "istruzioni di sistema");
    }

    #[test]
    fn cache_5m_system_blocco_con_cache_control() {
        let req = req_msgs(vec![msg("system", "istruzioni di sistema"), msg("user", "ciao")]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            None,
            CacheTtl::FiveMinutes,
        ))
        .unwrap();
        // System promosso a array di blocchi text con cache_control ephemeral.
        let sys = json["system"].as_array().expect("system come array di blocchi");
        assert_eq!(sys[0]["type"], "text");
        assert_eq!(sys[0]["text"], "istruzioni di sistema");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        // TTL 5m: nessun campo ttl.
        assert!(sys[0]["cache_control"].get("ttl").is_none());
    }

    #[test]
    fn cache_1h_system_aggiunge_ttl() {
        let req = req_msgs(vec![msg("system", "istruzioni"), msg("user", "ciao")]);
        let json =
            serde_json::to_value(build_request_body(&req, false, None, CacheTtl::OneHour)).unwrap();
        let sys = json["system"].as_array().unwrap();
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(sys[0]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn cache_system_vuoto_non_aggiunge_cache_control() {
        // Guardia HTTP 400: cache_control su testo vuoto e' vietato.
        let req = req_msgs(vec![msg("system", ""), msg("user", "ciao")]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            None,
            CacheTtl::FiveMinutes,
        ))
        .unwrap();
        // System vuoto: resta stringa, nessun blocco con cache_control.
        assert_eq!(json["system"], "");
    }

    #[test]
    fn cache_breakpoint_history_su_terzultimo_user() {
        // History >= 6 messaggi, 4 user: il breakpoint va sul TERZULTIMO user
        // (m2), il blocco stabile che si ripete fra turni, non sull'ultimo
        // (turno corrente mutevole).
        let req = req_msgs(vec![
            msg("user", "m1"),
            msg("assistant", "r1"),
            msg("user", "m2"),
            msg("assistant", "r2"),
            msg("user", "m3"),
            msg("assistant", "r3"),
            msg("user", "turno corrente"),
        ]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            None,
            CacheTtl::FiveMinutes,
        ))
        .unwrap();
        let messages = json["messages"].as_array().unwrap();
        // Il terzultimo user ("m2") ha il cache_control.
        let m2 = &messages[2];
        assert_eq!(m2["role"], "user");
        let blocks = m2["content"].as_array().expect("m2 a blocchi");
        assert_eq!(blocks[0]["text"], "m2");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        // L'ultimo user (turno corrente) resta una stringa semplice.
        let last_user = messages.last().unwrap();
        assert_eq!(last_user["content"], "turno corrente");
    }

    #[test]
    fn cache_history_breakpoint_assente_se_pochi_messaggi() {
        // Sotto la soglia minima: nessun breakpoint sulla history.
        let req = req_msgs(vec![msg("user", "ciao"), msg("assistant", "ehi")]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            None,
            CacheTtl::FiveMinutes,
        ))
        .unwrap();
        // Pochi messaggi: l'user resta una stringa semplice.
        assert_eq!(json["messages"][0]["content"], "ciao");
    }

    #[test]
    fn beta_header_cumula_thinking_e_cache_estesa() {
        // Solo cache 1h.
        assert_eq!(
            beta_header(false, CacheTtl::OneHour).as_deref(),
            Some(EXTENDED_CACHE_BETA)
        );
        // Solo thinking.
        assert_eq!(
            beta_header(true, CacheTtl::FiveMinutes).as_deref(),
            Some(THINKING_BETA)
        );
        // Entrambi: cumulati con virgola.
        let both = beta_header(true, CacheTtl::OneHour).unwrap();
        assert!(both.contains(THINKING_BETA));
        assert!(both.contains(EXTENDED_CACHE_BETA));
        // Niente beta: cache 5m senza thinking.
        assert!(beta_header(false, CacheTtl::FiveMinutes).is_none());
        assert!(beta_header(false, CacheTtl::Off).is_none());
    }

    #[test]
    fn cache_ttl_parse() {
        assert_eq!(CacheTtl::parse("5m"), CacheTtl::FiveMinutes);
        assert_eq!(CacheTtl::parse("1h"), CacheTtl::OneHour);
        assert_eq!(CacheTtl::parse("off"), CacheTtl::Off);
        assert_eq!(CacheTtl::parse("boh"), CacheTtl::Off);
    }
}
