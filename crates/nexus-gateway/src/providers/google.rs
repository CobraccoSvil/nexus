//! Provider Google (Generative Language REST nativo).
//!
//! Il TS (`providers/google.ts`, 34 righe) instrada Gemini attraverso l'endpoint
//! OpenAI-compatibile di Google delegando a `OpenAIProvider`. Qui implementiamo
//! invece il formato REST NATIVO `generateContent`/`streamGenerateContent`, piu'
//! fedele all'API e testabile in isolamento:
//!   - i messaggi diventano `contents[]` con `role` (`user`/`model`) e `parts[]`;
//!   - il `system` prompt e' un campo separato `systemInstruction`;
//!   - la API key viaggia come query param `?key=...` (convenzione Google);
//!   - lo streaming usa `?alt=sse` con eventi `data: {GenerateContentResponse}`.
//!
//! Regola G: nessun modello hardcoded (arriva da `req.model`, finisce nel path
//! URL). Regola F: mai loggare prompt/response in chiaro.

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
    LlmRequest, LlmResponse, LlmStreamChunk, LlmUsage, MessageContent, SensitivityTier,
};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint REST nativo di Generative Language (override via costruttore).
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Chiave settings (regola G) del budget thinking Gemini. La stessa letta dal
/// brain Python (`providers.google.thinking_budget`, mig 0407): unica fonte di
/// verita' condivisa tra i due porting.
const THINKING_BUDGET_SETTING: &str = "providers.google.thinking_budget";

/// Budget thinking usato SOLO se il DB e' irraggiungibile e la richiesta ha
/// thinking abilitato (fallback graceful documentato, regola G). Allineato al
/// default 8192 della mig 0407; non e' un "magic default" di routing.
const THINKING_BUDGET_DB_DOWN_FALLBACK: u32 = 8192;

/// Soglia minima di `max_tokens` sotto la quale il thinking resta disattivo
/// (parita' col Python ~489: `if max_tokens >= 256`). Sotto questo valore non
/// c'e' spazio nemmeno per la sola risposta.
const THINKING_MIN_MAX_TOKENS: u32 = 256;

/// Pavimento del budget thinking effettivo (parita' col Python ~490:
/// `max(128, min(_tb_base, max_tokens))`).
const THINKING_BUDGET_FLOOR: u32 = 128;

/// TTL della cache settings (60s, come gli altri provider).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

pub struct GoogleProvider {
    http: Client,
    base_url: String,
    api_key: String,
    db: Option<PgPool>,
    thinking_budget: TtlCache<(), u32>,
}

impl GoogleProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). Il budget
    /// thinking non sara' leggibile dai settings: il thinking resta disattivo a
    /// meno che la request non porti un `budget_tokens` esplicito.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere il budget thinking dai
    /// settings (regola G). `base_url` opzionale (default Google ufficiale);
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

    /// Budget thinking di base dai settings (cache TTL 60s). Se il DB e'
    /// irraggiungibile o la chiave assente, ricade sul fallback documentato.
    /// Il valore viene poi validato in [`resolve_thinking`] (clamp + guardia
    /// `max_tokens`).
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

    /// URL dell'azione per il modello richiesto. `stream=true` usa
    /// `streamGenerateContent?alt=sse`, altrimenti `generateContent`.
    fn endpoint(&self, model: &str, stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let mut url = format!("{}/models/{}:{}", self.base_url, model, action);
        if stream {
            url.push_str("?alt=sse");
        }
        url
    }
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    fn name(&self) -> &str {
        "google"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        1_000_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let configured = self.configured_thinking_budget().await;
        let thinking = resolve_thinking(req, configured);
        let body = build_request_body(req, thinking);
        let start = Instant::now();

        let resp = self
            .http
            .post(self.endpoint(&req.model, false))
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: body d'errore propagato al caller (cooldown Fase 3 lo
            // classifica via is_billing_error), non loggato qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }

        let parsed: GenerateContentResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_generate_response(parsed, req.model.clone(), latency_ms))
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let configured = self.configured_thinking_budget().await;
        let thinking = resolve_thinking(req, configured);
        let body = build_request_body(req, thinking);

        let resp = self
            .http
            .post(self.endpoint(&req.model, true))
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }

        let model_used = req.model.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut parser = GoogleSseParser::new(model_used);

            loop {
                match bytes.next().await {
                    Some(Ok(buf)) => parser.push_bytes(&String::from_utf8_lossy(&buf)),
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
        // GET /models: 2xx => raggiungibile. Usato anche dal re-probe del cooldown.
        let url = format!("{}/models", self.base_url);
        match self
            .http
            .get(url)
            .query(&[("key", &self.api_key)])
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

/// Esito della risoluzione del thinking Gemini per una richiesta. `None` =
/// thinking disattivo; `Some(budget)` = `thinkingConfig` con quel budget e tetto
/// di output alzato di conseguenza (fix hollow completion).
type GoogleThinking = Option<u32>;

/// Budget thinking effettivo per la richiesta (parita' col Python ~470-503).
///
/// Replica le guardie del brain:
///   - thinking attivo solo se `req.thinking.enabled`;
///   - budget esplicito nella request ha priorita' su quello configurato;
///   - se `max_tokens` < soglia minima (256), thinking disattivato (troppo poco
///     spazio anche solo per la risposta);
///   - clamp del budget a `max(128, min(budget, max_tokens))`.
fn resolve_thinking(req: &LlmRequest, configured_budget: u32) -> GoogleThinking {
    let enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
    if !enabled {
        return None;
    }
    // Senza un tetto di output esplicito non sappiamo dimensionare il budget
    // (il Python alza max_output_tokens partendo da max_tokens richiesto): in
    // assenza, evitiamo di attivare il thinking per non rischiare hollow.
    let max_tokens = req.max_tokens?;
    if max_tokens < THINKING_MIN_MAX_TOKENS {
        return None;
    }
    let base = req
        .thinking
        .as_ref()
        .and_then(|t| t.budget_tokens)
        .unwrap_or(configured_budget);
    if base == 0 {
        return None;
    }
    let budget = base.min(max_tokens).max(THINKING_BUDGET_FLOOR);
    Some(budget)
}

/// Costruisce il corpo `GenerateContentRequest`: separa il system come
/// `systemInstruction`, mappa i ruoli (`assistant`->`model`) e impacchetta i
/// parametri di generazione in `generationConfig`.
///
/// `thinking` (gia' risolto da [`resolve_thinking`]): `Some(budget)` attiva il
/// `thinkingConfig` con `includeThoughts=true` e ALZA `maxOutputTokens` di
/// `budget` (fix hollow completion: i token di reasoning sono conteggiati dentro
/// il tetto di output, parita' col Python ~494 `_effective_output_tokens`).
fn build_request_body(req: &LlmRequest, thinking: GoogleThinking) -> GenerateContentRequest {
    let mut system_instruction: Option<GoogleContent> = None;
    let mut contents: Vec<GoogleContent> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                system_instruction = Some(GoogleContent {
                    role: None,
                    parts: vec![GooglePart::text(content_to_string(&msg.content))],
                });
            }
            role => {
                // RI-PASSAGGIO thought_signature: se il turno assistant la porta,
                // va riattaccata alla PRIMA part del turno (parita' col Python
                // `_convert_messages_to_google` ~776-798). Obbligatoria su
                // Gemini 3, raccomandata su 2.5. Solo sui turni `model`.
                let signature = if map_role(role) == "model" {
                    msg.thinking_signature.clone()
                } else {
                    None
                };
                let mut parts = content_to_parts(&msg.content);
                // La signature si attacca alla PRIMA part del turno (vuota se
                // assente). `content_to_parts` garantisce almeno una part.
                if let Some(first) = parts.first_mut() {
                    first.thought_signature = signature;
                }
                contents.push(GoogleContent {
                    role: Some(map_role(role).to_string()),
                    parts,
                });
            }
        }
    }

    // Fix hollow completion: alza il tetto di output del budget thinking cosi'
    // i max_tokens richiesti restano interi per la risposta utente.
    let max_output_tokens = match (req.max_tokens, thinking) {
        (Some(mt), Some(budget)) => Some(mt.saturating_add(budget)),
        (mt, _) => mt,
    };

    let thinking_config = thinking.map(|budget| ThinkingConfigWire {
        include_thoughts: true,
        thinking_budget: budget,
    });

    let generation_config =
        if req.temperature.is_some() || max_output_tokens.is_some() || thinking_config.is_some() {
            Some(GenerationConfig {
                temperature: req.temperature,
                max_output_tokens,
                thinking_config,
            })
        } else {
            None
        };

    GenerateContentRequest {
        contents,
        system_instruction,
        generation_config,
    }
}

/// Mappa il ruolo del contratto al ruolo Google: `assistant` -> `model`, tutto
/// il resto (`user`, `tool`) -> `user` (Google non distingue il tool come ruolo
/// separato nel formato base).
fn map_role(role: &str) -> &str {
    match role {
        "assistant" | "model" => "model",
        _ => "user",
    }
}

fn content_to_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => serde_json::to_string(blocks).unwrap_or_default(),
    }
}

/// Mappa il content di un messaggio nelle `parts[]` Google. Caso semplice:
/// una sola part di testo. Con blocchi immagine (`image_url`) emette una part
/// `inlineData` (per i data URI base64) o `fileData` (per le URL http), cosi'
/// la capability vision e' preservata (parita' col formato nativo che il brain
/// usa via `Part.from_bytes`). I blocchi non-immagine restano testo.
///
/// Garantisce SEMPRE almeno una part (eventualmente testo vuoto): la signature
/// del thinking va riattaccata alla prima part del turno.
fn content_to_parts(content: &MessageContent) -> Vec<GooglePart> {
    match content {
        MessageContent::Text(s) => vec![GooglePart::text(s.clone())],
        MessageContent::Blocks(blocks) => {
            let has_image = blocks.iter().any(|b| b.kind == "image_url");
            if !has_image {
                // Nessuna immagine: testo serializzato (parita' col TS).
                return vec![GooglePart::text(content_to_string(content))];
            }
            let mut parts: Vec<GooglePart> = Vec::new();
            for b in blocks {
                match b.kind.as_str() {
                    "image_url" => {
                        if let Some(url) = b
                            .image_url
                            .as_ref()
                            .and_then(|iu| iu.get("url"))
                            .and_then(|u| u.as_str())
                        {
                            parts.push(image_url_to_part(url));
                        }
                    }
                    "text" => {
                        if let Some(t) = &b.text {
                            parts.push(GooglePart::text(t.clone()));
                        }
                    }
                    _ => {
                        if let Some(c) = &b.content {
                            parts.push(GooglePart::text(c.clone()));
                        }
                    }
                }
            }
            if parts.is_empty() {
                parts.push(GooglePart::text(String::new()));
            }
            parts
        }
    }
}

/// Converte una `url` di un blocco immagine in una part Google:
///   - `data:<mime>;base64,<dati>` -> `inlineData{mimeType, data}` (base64);
///   - qualunque altra URL (http/https/gs) -> `fileData{mimeType, fileUri}`.
/// Per i data URI malformati ricade su `fileData` con la URL grezza, senza
/// rompere la richiesta.
fn image_url_to_part(url: &str) -> GooglePart {
    if let Some((mime, data)) = parse_data_uri(url) {
        GooglePart::inline_data(mime, data)
    } else {
        // URL remota: Google la scarica via fileData. Il mimeType non e'
        // sempre deducibile dalla URL; quando ignoto si omette (l'API lo
        // inferisce dal contenuto scaricato).
        GooglePart::file_data(mime_from_url(url), url.to_string())
    }
}

/// Estrae `(mime, base64)` da un data URI `data:<mime>;base64,<dati>`. Ritorna
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

/// Best-effort del mime da estensione URL (solo per `fileData`). `None` se non
/// riconosciuto: l'API Google inferisce comunque dal contenuto.
fn mime_from_url(url: &str) -> Option<String> {
    let lower = url.split('?').next().unwrap_or(url).to_lowercase();
    let mime = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        return None;
    };
    Some(mime.to_string())
}

/// Mappa una `GenerateContentResponse` nel contratto [`LlmResponse`]: concatena
/// le `parts[].text` del primo candidate e normalizza il `finishReason`.
fn from_generate_response(
    resp: GenerateContentResponse,
    model_used: String,
    latency_ms: u64,
) -> LlmResponse {
    let candidate = resp.candidates.into_iter().next();

    // Separa il testo utente dai "thoughts" (part con `thought=true`): il
    // reasoning interno va in `reasoning`, non nel content (parita' col Python
    // ~575-583). La `thoughtSignature` (gia' base64 nell'API REST) si cattura
    // una sola volta, dovunque appaia (parita' col Python ~567-574).
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut thinking_signature: Option<String> = None;

    if let Some(c) = candidate.as_ref() {
        for part in &c.content.parts {
            if thinking_signature.is_none() {
                if let Some(sig) = &part.thought_signature {
                    if !sig.is_empty() {
                        thinking_signature = Some(sig.clone());
                    }
                }
            }
            if let Some(text) = &part.text {
                if part.thought.unwrap_or(false) {
                    reasoning.push_str(text);
                } else {
                    content.push_str(text);
                }
            }
        }
    }

    let finish_reason = map_finish_reason(candidate.as_ref().and_then(|c| c.finish_reason.as_deref()));

    let usage = resp
        .usage_metadata
        .map(|u| LlmUsage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cache_read_tokens: u.cached_content_token_count,
            cache_creation_tokens: None,
        })
        .unwrap_or(LlmUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        });

    LlmResponse {
        content,
        // Il formato base mappato non emette function-call: tool su Google
        // richiederebbe il blocco functionDeclarations, fuori scope di questa
        // implementazione REST minimale (parita' funzionale col TS, che a sua
        // volta delega senza supporto tool nativo nel formato Google).
        tool_calls: None,
        usage,
        model_used,
        provider_used: "google".to_string(),
        latency_ms,
        finish_reason,
        privacy_rerouted: None,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        thinking_signature,
    }
}

/// Mappa il `finishReason` Google ai valori canonici del contratto. `STOP` e i
/// valori non noti collassano a `stop`.
fn map_finish_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("STOP") {
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" => "content_filter",
        _ => "stop",
    }
    .to_string()
}

/// Parser SSE Google (`?alt=sse`): ogni riga `data: {GenerateContentResponse}`
/// porta un delta incrementale; l'ultimo evento contiene `usageMetadata` e il
/// `finishReason`. Stateful, testabile senza rete.
struct GoogleSseParser {
    line_buf: String,
    pending: VecDeque<LlmStreamChunk>,
    model_used: String,
}

impl GoogleSseParser {
    fn new(model_used: String) -> Self {
        Self {
            line_buf: String::new(),
            pending: VecDeque::new(),
            model_used,
        }
    }

    fn push_bytes(&mut self, s: &str) {
        self.line_buf.push_str(s);
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].to_string();
            self.line_buf.drain(..=idx);
            self.parse_line(&line);
        }
    }

    fn flush_leftover(&mut self) {
        let leftover = std::mem::take(&mut self.line_buf);
        for line in leftover.lines() {
            self.parse_line(line);
        }
    }

    fn parse_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return,
        };
        if payload.is_empty() {
            return;
        }
        let resp: GenerateContentResponse = match serde_json::from_str(payload) {
            Ok(r) => r,
            Err(_) => return,
        };
        if let Some(chunk) = self.chunk_from_response(resp) {
            self.pending.push_back(chunk);
        }
    }

    fn chunk_from_response(&self, resp: GenerateContentResponse) -> Option<LlmStreamChunk> {
        let candidate = resp.candidates.into_iter().next();

        // Separa testo utente da reasoning (part `thought=true`): il primo va in
        // `delta`, il secondo in `reasoning_delta` (parita' col Python streaming).
        let mut delta = String::new();
        let mut reasoning_delta = String::new();
        if let Some(c) = candidate.as_ref() {
            for part in &c.content.parts {
                if let Some(text) = &part.text {
                    if part.thought.unwrap_or(false) {
                        reasoning_delta.push_str(text);
                    } else {
                        delta.push_str(text);
                    }
                }
            }
        }

        let finish_reason = candidate
            .as_ref()
            .and_then(|c| c.finish_reason.as_deref())
            .map(|r| map_finish_reason(Some(r)));

        let usage = resp.usage_metadata.as_ref().map(|u| LlmUsage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cache_read_tokens: u.cached_content_token_count,
            cache_creation_tokens: None,
        });

        // Chunk vuoto (nessun delta, nessun reasoning, nessun finish, nessun
        // usage): salta.
        if delta.is_empty() && reasoning_delta.is_empty() && finish_reason.is_none() && usage.is_none()
        {
            return None;
        }

        // L'usage va riportato solo sul chunk finale (quando c'e' finish).
        let usage = if finish_reason.is_some() { usage } else { None };

        Some(LlmStreamChunk {
            delta,
            tool_call_delta: None,
            finish_reason,
            usage,
            provider_used: Some("google".to_string()),
            model_used: Some(self.model_used.clone()),
            reasoning_delta: if reasoning_delta.is_empty() {
                None
            } else {
                Some(reasoning_delta)
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tipi wire (formato Generative Language). Separati dal contratto del gateway.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GenerateContentRequest {
    contents: Vec<GoogleContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GoogleContent>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Serialize)]
struct GoogleContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GooglePart>,
}

/// Part di un messaggio Google. Esattamente uno tra `text`, `inline_data`,
/// `file_data` e' valorizzato (gli altri sono omessi dal wire). La
/// `thought_signature` si attacca alla prima part del turno `model`.
#[derive(Debug, Serialize)]
struct GooglePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Immagine inline base64 (`{mimeType, data}`), per i data URI.
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    inline_data: Option<GoogleInlineData>,
    /// Riferimento a file remoto (`{mimeType?, fileUri}`), per le URL http.
    #[serde(rename = "fileData", skip_serializing_if = "Option::is_none")]
    file_data: Option<GoogleFileData>,
    /// Firma opaca del thinking (base64) ri-passata nei turni successivi. Sul
    /// wire e' `thoughtSignature`; assente quando il turno non la porta.
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
}

impl GooglePart {
    fn text(text: String) -> Self {
        Self {
            text: Some(text),
            inline_data: None,
            file_data: None,
            thought_signature: None,
        }
    }

    fn inline_data(mime_type: String, data: String) -> Self {
        Self {
            text: None,
            inline_data: Some(GoogleInlineData { mime_type, data }),
            file_data: None,
            thought_signature: None,
        }
    }

    fn file_data(mime_type: Option<String>, file_uri: String) -> Self {
        Self {
            text: None,
            inline_data: None,
            file_data: Some(GoogleFileData {
                mime_type,
                file_uri,
            }),
            thought_signature: None,
        }
    }
}

/// Immagine inline base64 nel formato Gemini (`inlineData`).
#[derive(Debug, Serialize)]
struct GoogleInlineData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    data: String,
}

/// Riferimento a file remoto nel formato Gemini (`fileData`). Il `mimeType` e'
/// opzionale: l'API lo inferisce dal contenuto quando assente.
#[derive(Debug, Serialize)]
struct GoogleFileData {
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(rename = "fileUri")]
    file_uri: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    /// Configurazione thinking (`includeThoughts` + `thinkingBudget`). Presente
    /// solo quando il thinking e' attivo per la richiesta.
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<ThinkingConfigWire>,
}

/// `thinkingConfig` del body Gemini. `includeThoughts=true` espone i thoughts
/// nella risposta cosi' il reasoning e' visibile (parita' col Python ~491-493).
#[derive(Debug, Serialize)]
struct ThinkingConfigWire {
    #[serde(rename = "includeThoughts")]
    include_thoughts: bool,
    #[serde(rename = "thinkingBudget")]
    thinking_budget: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GoogleCandidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<GoogleUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct GoogleCandidate {
    #[serde(default)]
    content: GoogleRespContent,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct GoogleRespContent {
    #[serde(default)]
    parts: Vec<GoogleRespPart>,
}

#[derive(Debug, Deserialize)]
struct GoogleRespPart {
    #[serde(default)]
    text: Option<String>,
    /// `true` se la part e' un "thought" (reasoning interno dei modelli 2.5/3),
    /// da separare dal testo utente.
    #[serde(default)]
    thought: Option<bool>,
    /// Firma opaca del thinking (base64) emessa da Gemini: va catturata e
    /// rispedita identica nei turni successivi.
    #[serde(rename = "thoughtSignature", default)]
    thought_signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
    /// Token serviti dall'implicit caching Gemini 2.5+ (sottoinsieme di
    /// `promptTokenCount`). Presente solo a cache hit.
    #[serde(rename = "cachedContentTokenCount", default)]
    cached_content_token_count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, RequestMetadata};

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
        let p = GoogleProvider::new(Client::new(), "key", None);
        assert_eq!(p.name(), "google");
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 1_000_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }

    #[test]
    fn endpoint_streaming_aggiunge_alt_sse() {
        let p = GoogleProvider::new(Client::new(), "key", None);
        let url = p.endpoint("gemini-x", true);
        assert!(url.ends_with("/models/gemini-x:streamGenerateContent?alt=sse"));
        let url2 = p.endpoint("gemini-x", false);
        assert!(url2.ends_with("/models/gemini-x:generateContent"));
    }

    #[test]
    fn system_estratto_in_system_instruction() {
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("system", "istruzione"), msg("user", "domanda")],
            temperature: Some(0.5),
            max_tokens: Some(500),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        assert_eq!(json["systemInstruction"]["parts"][0]["text"], "istruzione");
        // Solo lo user finisce in contents.
        assert_eq!(json["contents"].as_array().unwrap().len(), 1);
        assert_eq!(json["contents"][0]["role"], "user");
        assert_eq!(json["contents"][0]["parts"][0]["text"], "domanda");
        assert_eq!(json["generationConfig"]["temperature"], 0.5);
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 500);
    }

    #[test]
    fn assistant_mappato_su_model() {
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("assistant", "risposta precedente")],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        // Nessun parametro di generazione -> generationConfig assente.
        assert!(json.get("generationConfig").is_none());
    }

    #[test]
    fn deserializza_response() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [{"text": "Ciao "}, {"text": "mondo"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 4}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-x".to_string(), 33);

        assert_eq!(resp.content, "Ciao mondo");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 11);
        assert_eq!(resp.usage.output_tokens, 4);
        assert_eq!(resp.provider_used, "google");
    }

    #[test]
    fn finish_reason_mappato() {
        assert_eq!(map_finish_reason(Some("STOP")), "stop");
        assert_eq!(map_finish_reason(Some("MAX_TOKENS")), "length");
        assert_eq!(map_finish_reason(Some("SAFETY")), "content_filter");
        assert_eq!(map_finish_reason(Some("boh")), "stop");
        assert_eq!(map_finish_reason(None), "stop");
    }

    #[test]
    fn sse_delta_emette_chunk() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(r#"data: {"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#);
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "Hel");
        assert!(p.pending[0].finish_reason.is_none());
        assert_eq!(p.pending[0].provider_used.as_deref(), Some("google"));
    }

    #[test]
    fn sse_chunk_finale_riporta_usage() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2}}"#,
        );
        let chunk = p.pending.pop_back().expect("chunk finale");
        assert_eq!(chunk.delta, ".");
        assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        let usage = chunk.usage.expect("usage finale");
        assert_eq!(usage.input_tokens, 9);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn sse_riga_parziale_gestita() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.push_bytes(r#"data: {"candidates":[{"content":{"parts":[{"te"#);
        assert_eq!(p.pending.len(), 0);
        p.push_bytes("xt\":\"ok\"}]}}]}\n");
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "ok");
    }

    // --- Extended thinking (passo 2) ---------------------------------------

    fn req_thinking(
        enabled: bool,
        budget: Option<u32>,
        max_tokens: Option<u32>,
        messages: Vec<LlmMessage>,
    ) -> LlmRequest {
        LlmRequest {
            model: "gemini-x".to_string(),
            messages,
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
    fn thinking_attivo_aggiunge_config_e_alza_output() {
        // budget esplicito 2048, max_tokens 8000 -> thinking attivo, output alzato.
        let req = req_thinking(true, Some(2048), Some(8000), vec![msg("user", "ciao")]);
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, Some(2048));
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert_eq!(json["generationConfig"]["thinkingConfig"]["includeThoughts"], true);
        assert_eq!(json["generationConfig"]["thinkingConfig"]["thinkingBudget"], 2048);
        // Fix hollow: maxOutputTokens = max_tokens + budget.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000 + 2048);
    }

    #[test]
    fn thinking_disattivo_non_aggiunge_config() {
        let req = req_thinking(false, Some(2048), Some(8000), vec![msg("user", "ciao")]);
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, None);
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert!(json["generationConfig"].get("thinkingConfig").is_none());
        // Output non alzato: resta il max_tokens richiesto.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000);
    }

    #[test]
    fn thinking_budget_usa_configurato_e_clampa() {
        // Budget configurato 50000 > max_tokens 4000 -> clamp a max_tokens.
        let req = req_thinking(true, None, Some(4000), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req, 50_000), Some(4000));
        // max_tokens sotto soglia minima -> thinking disattivato.
        let req2 = req_thinking(true, Some(1024), Some(100), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req2, 8192), None);
        // Nessun max_tokens -> non dimensionabile -> disattivato.
        let req3 = req_thinking(true, Some(1024), None, vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req3, 8192), None);
    }

    #[test]
    fn round_trip_thought_signature_su_part_model() {
        // Un turno assistant con thinking_signature deve produrre la
        // thoughtSignature sulla part del messaggio `model`.
        let mut a = msg("assistant", "ho ragionato");
        a.thinking_signature = Some("c2lnLWdlbWluaQ==".to_string());
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![a],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        assert_eq!(
            json["contents"][0]["parts"][0]["thoughtSignature"],
            "c2lnLWdlbWluaQ=="
        );
    }

    #[test]
    fn signature_su_user_non_viene_ripassata() {
        // La signature appartiene ai turni `model`: su uno user e' ignorata
        // (mai inviata su una part `user`, non avrebbe senso lato API).
        let mut u = msg("user", "domanda");
        u.thinking_signature = Some("spuria".to_string());
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![u],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert!(json["contents"][0]["parts"][0]
            .get("thoughtSignature")
            .is_none());
    }

    #[test]
    fn deserializza_response_con_thought_e_signature() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [
                    {"text": "rifletto", "thought": true},
                    {"text": "risposta utente", "thoughtSignature": "c2lnLXJlc3A="}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 5, "cachedContentTokenCount": 8}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-x".to_string(), 9);

        // Il thought NON entra nel content utente.
        assert_eq!(resp.content, "risposta utente");
        assert_eq!(resp.reasoning.as_deref(), Some("rifletto"));
        assert_eq!(resp.thinking_signature.as_deref(), Some("c2lnLXJlc3A="));
        assert_eq!(resp.usage.cache_read_tokens, Some(8));
    }

    #[test]
    fn response_senza_thought_ha_reasoning_none() {
        let raw = r#"{
            "candidates": [{"content": {"parts": [{"text": "solo risposta"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "m".to_string(), 0);
        assert!(resp.reasoning.is_none());
        assert!(resp.thinking_signature.is_none());
        assert!(resp.usage.cache_read_tokens.is_none());
    }

    #[test]
    fn sse_thought_emette_reasoning_delta() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"penso...","thought":true}]}}]}"#,
        );
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].reasoning_delta.as_deref(), Some("penso..."));
        // Il delta testuale resta vuoto sul chunk di reasoning.
        assert_eq!(p.pending[0].delta, "");
    }

    // --- Vision: parts inlineData / fileData (passo 3) ---------------------

    fn image_block(url: &str) -> LlmMessage {
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
        }
    }

    fn req_with(msg: LlmMessage) -> LlmRequest {
        LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn vision_data_uri_diventa_inline_data() {
        let req = req_with(image_block("data:image/png;base64,QUJD"));
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        // Prima part: testo; seconda: inlineData con mimeType+data.
        assert_eq!(parts[0]["text"], "descrivi");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "QUJD");
        // Niente text spurio sulla part immagine.
        assert!(parts[1].get("text").is_none());
    }

    #[test]
    fn vision_url_http_diventa_file_data() {
        let req = req_with(image_block("https://example.com/foto.jpg"));
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[1]["fileData"]["fileUri"], "https://example.com/foto.jpg");
        assert_eq!(parts[1]["fileData"]["mimeType"], "image/jpeg");
        assert!(parts[1].get("inlineData").is_none());
    }

    #[test]
    fn parse_data_uri_estrae_mime_e_dati() {
        assert_eq!(
            parse_data_uri("data:image/webp;base64,XYZ"),
            Some(("image/webp".to_string(), "XYZ".to_string()))
        );
        // Non base64 / non data URI -> None.
        assert!(parse_data_uri("https://x/y.png").is_none());
        assert!(parse_data_uri("data:image/png,raw").is_none());
    }

    #[test]
    fn vision_signature_su_prima_part_con_immagine() {
        // La signature del thinking si attacca alla PRIMA part anche quando il
        // turno e' multimodale (testo + immagine).
        let mut msg = image_block("data:image/png;base64,QUJD");
        msg.role = "assistant".to_string();
        msg.thinking_signature = Some("c2ln".to_string());
        let req = req_with(msg);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        assert_eq!(parts[0]["thoughtSignature"], "c2ln");
        assert!(parts[1].get("thoughtSignature").is_none());
    }
}
