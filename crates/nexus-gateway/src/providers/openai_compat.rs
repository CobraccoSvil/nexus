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
    GeneratedImage, ImageGenResponse, LlmRequest, LlmResponse, LlmStreamChunk, LlmToolCall,
    LlmUsage, ToolCallDelta, ToolCallDeltaFunction, ToolFunctionCall, TranscribeResponse,
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
        let mut body = build_request_body(req, false, reasoning);
        if provider_requires_user_or_tool_last(&self.provider_name) {
            strip_trailing_assistant(&mut body.messages);
        }
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
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        // Body come testo + parse esplicito: il generico `resp.json()` di
        // reqwest appiattiva OGNI mismatch in "error decoding response body"
        // (incidente mistral 2026-07-06: 18 errori in history senza causa
        // diagnosticabile). Il parse separato distingue "body troncato dalla
        // rete" (fallisce `text()`, transitorio vero) da "schema inatteso"
        // (fallisce serde con campo/posizione precisi, senza payload nel
        // messaggio: regola F).
        let body = resp.text().await?;
        let parsed = parse_chat_completion(&self.provider_name, &body)?;
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
        let mut body = build_request_body(req, true, reasoning);
        if provider_requires_user_or_tool_last(&self.provider_name) {
            strip_trailing_assistant(&mut body.messages);
        }

        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
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

    /// Autodiscovery live: `GET {base_url}/models` (Bearer) ed estrae `data[].id`.
    /// Dialetto OpenAI condiviso da OpenAI/Mistral/DeepSeek/vLLM (punto unico,
    /// regola L). Il parsing della risposta e' delegato a [`parse_models_response`]
    /// (puro, testabile senza rete).
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .list_models_meta()
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect())
    }

    /// Autodiscovery live CON METADATI: id + finestra di contesto dichiarata
    /// dal provider quando il dialetto la espone (Mistral: `max_context_length`
    /// in `data[]`; OpenAI/DeepSeek non la espongono -> `None`). Un solo fetch
    /// (regola L): [`Self::list_models`] delega qui e proietta i soli id.
    pub async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        let url = format!("{}/models", self.base_url);
        let resp = self.http.get(url).bearer_auth(&self.api_key).send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Errore strutturato anche sulla lista modelli (regola M): status +
            // codice, mai testo da classificare. Il caller aggrega best-effort.
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(parse_models_meta_response(&body))
    }

    /// Genera immagini via `POST {base_url}/images/generations` (dialetto OpenAI
    /// Images). Punto unico del trasporto image-gen OpenAI-compatibile (regola L):
    /// stesso `http` client e `bearer_auth(api_key)` di [`Self::complete`], stesso
    /// status-check propagato al caller (che applica `is_billing_error` + cooldown).
    ///
    /// Richiesta: `{model, prompt, n?, size?, response_format:"b64_json"}`.
    /// Risposta: `{data:[{b64_json|url}], ...}` -> [`GeneratedImage`]. Regola G:
    /// `model` arriva dal chiamante. Regola F: il body d'errore (che non contiene
    /// prompt utente) e' propagato al caller, non loggato qui in chiaro.
    pub async fn images_generations(
        &self,
        model: &str,
        prompt: &str,
        n: Option<u32>,
        size: Option<&str>,
    ) -> anyhow::Result<ImageGenResponse> {
        let body = ImageGenWireRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            n,
            size: size.map(|s| s.to_string()),
            // base64 inline: il gateway non dipende da URL temporanee del provider.
            response_format: "b64_json".to_string(),
        };
        let start = Instant::now();

        let resp = self
            .http
            .post(format!("{}/images/generations", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        let parsed: ImagesResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_images_response(
            parsed,
            model.to_string(),
            &self.provider_name,
            latency_ms,
        ))
    }

    /// Trascrive audio via `POST {base_url}/audio/transcriptions` (dialetto OpenAI
    /// Audio, MULTIPART/form-data). Punto unico del trasporto audio-in OpenAI-
    /// compatibile (regola L): stesso `http` client e `bearer_auth(api_key)` di
    /// [`Self::complete`], stesso status-check propagato al caller (che applica
    /// `is_billing_error` + cooldown).
    ///
    /// Form: `file=<bytes>` (con `file_name` + mime), `model`, `response_format=json`,
    /// `language` se presente. Risposta: `{"text":"..."}` -> [`TranscribeResponse`].
    /// Regola G: `model` arriva dal chiamante. Regola F: il body d'errore (che non
    /// contiene il payload audio) e' propagato al caller, non loggato qui.
    pub async fn transcribe(
        &self,
        model: &str,
        audio_bytes: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> anyhow::Result<TranscribeResponse> {
        let mut part = reqwest::multipart::Part::bytes(audio_bytes).file_name(filename.to_string());
        // MIME inferito dall'estensione del filename (gia' risolta dal chiamante in
        // base al mime dichiarato). Se non riconosciuto, lasciamo che reqwest usi
        // application/octet-stream: OpenAI inferisce comunque dal file_name.
        if let Some(mime) = mime_from_filename(filename) {
            part = part.mime_str(mime)?;
        }
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string())
            .text("response_format", "json");
        if let Some(lang) = language.filter(|l| !l.trim().is_empty()) {
            form = form.text("language", lang.trim().to_string());
        }

        let start = Instant::now();
        let resp = self
            .http
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        let parsed: TranscriptionResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(TranscribeResponse {
            text: parsed.text,
            model_used: model.to_string(),
            provider_used: self.provider_name.clone(),
            latency_ms,
        })
    }

    /// Sintetizza audio via `POST {base_url}/audio/speech` (dialetto OpenAI Audio,
    /// JSON in -> BYTES binari out). Punto unico del trasporto audio-out OpenAI-
    /// compatibile (regola L): stesso `http` client e `bearer_auth(api_key)` di
    /// [`Self::complete`], stesso status-check propagato al caller (che applica
    /// `is_billing_error` + cooldown).
    ///
    /// Body JSON: `model`, `input`, `voice` (se presente), `response_format`.
    /// Risposta: BYTES audio (NON JSON) + il Content-Type per il MIME reale.
    /// Regola G: `model` arriva dal chiamante. Regola F: il body d'errore (che non
    /// contiene il testo sintetizzato) e' propagato al caller, non loggato qui.
    pub async fn speech(
        &self,
        model: &str,
        input: &str,
        voice: Option<&str>,
        response_format: Option<&str>,
    ) -> anyhow::Result<(Vec<u8>, String)> {
        let mut body = serde_json::json!({
            "model": model,
            "input": input,
        });
        if let Some(v) = voice.filter(|v| !v.trim().is_empty()) {
            body["voice"] = serde_json::Value::String(v.trim().to_string());
        }
        if let Some(fmt) = response_format.filter(|f| !f.trim().is_empty()) {
            body["response_format"] = serde_json::Value::String(fmt.trim().to_string());
        }

        let resp = self
            .http
            .post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        // Content-Type per il MIME reale; se assente lo deriviamo dal formato
        // richiesto (default mp3 -> audio/mpeg).
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| mime_from_audio_format(response_format).to_string());

        // La risposta e' BINARIA: NON json(). Leggiamo i bytes.
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, mime))
    }
}

/// MIME audio dal `response_format` richiesto al TTS. Copre i formati emessi
/// dall'API OpenAI Audio Speech. Default `audio/mpeg` (formato `mp3`, il default
/// del provider). Funzione PURA (testabile).
fn mime_from_audio_format(format: Option<&str>) -> &'static str {
    match format.map(|f| f.trim().to_lowercase()).as_deref() {
        Some("wav") => "audio/wav",
        Some("opus") => "audio/opus",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("pcm") => "audio/pcm",
        // mp3 o assente -> default mp3.
        _ => "audio/mpeg",
    }
}

/// MIME audio dall'estensione del filename multipart. Copre i formati accettati
/// dall'API OpenAI Audio. `None` per estensioni non riconosciute (reqwest usa il
/// default; OpenAI inferisce dal file_name). Funzione PURA (testabile).
fn mime_from_filename(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_lowercase())?;
    let mime = match ext.as_str() {
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => return None,
    };
    Some(mime)
}

/// Mappa una [`ImagesResponse`] del dialetto OpenAI Images nel contratto
/// [`ImageGenResponse`]. Funzione PURA (testabile senza rete).
fn from_images_response(
    resp: ImagesResponse,
    model_used: String,
    provider_name: &str,
    latency_ms: u64,
) -> ImageGenResponse {
    let images = resp
        .data
        .into_iter()
        .map(|d| GeneratedImage {
            b64_json: d.b64_json.filter(|s| !s.is_empty()),
            url: d.url.filter(|s| !s.is_empty()),
            // OpenAI Images non dichiara il mime: e' sempre PNG inline; lasciamo
            // None per non inventare un valore (regola G/H).
            mime: None,
        })
        .collect();
    ImageGenResponse {
        images,
        model_used,
        provider_used: provider_name.to_string(),
        latency_ms,
    }
}

/// Estrae i nomi modello dalla risposta `GET /models` del dialetto OpenAI:
/// `{ "data": [{ "id": "..." }, ...] }`. Funzione PURA (regola L, testabile
/// senza rete): salta gli elementi senza `id` non-vuoto, deduplica e ordina per
/// output deterministico (parita' col brain `list_models_live`).
pub fn parse_models_response(body: &serde_json::Value) -> Vec<String> {
    parse_models_meta_response(body)
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// Variante CON METADATI di [`parse_models_response`] (punto unico del parsing,
/// regola L: la versione nomi-soli vi delega). Oltre all'`id`, estrae la
/// finestra di contesto DICHIARATA dal provider quando il dialetto la espone:
/// Mistral usa `max_context_length` in `data[]` (OpenAI/DeepSeek non hanno il
/// campo -> `None`). Valori non positivi sono trattati come non dichiarati:
/// meglio "ignota" di una finestra inventata (regola H, incidente 2026-07-06).
/// Ordinamento/dedup per id come la versione nomi-soli (output deterministico).
pub fn parse_models_meta_response(body: &serde_json::Value) -> Vec<crate::provider::ModelMeta> {
    let items = body.get("data").and_then(|d| d.as_array());
    let mut metas: Vec<crate::provider::ModelMeta> = items
        .map(|arr| arr.iter().filter_map(openai_model_meta_of).collect())
        .unwrap_or_default();
    metas.sort_by(|a, b| a.id.cmp(&b.id));
    metas.dedup_by(|a, b| a.id == b.id);
    metas
}

/// Mappa UN elemento di `data[]` (dialetto OpenAI) in [`ModelMeta`]: `id`
/// trimmato non-vuoto obbligatorio; `max_context_length` (Mistral) come
/// finestra dichiarata solo se positiva.
fn openai_model_meta_of(m: &serde_json::Value) -> Option<crate::provider::ModelMeta> {
    let id = m
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let context_window = m
        .get("max_context_length")
        .and_then(serde_json::Value::as_i64)
        .filter(|w| *w > 0);
    Some(crate::provider::ModelMeta { id, context_window })
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
    let mut messages: Vec<WireMessage> = req.messages.iter().map(to_wire_message).collect();

    // ROUND-TRIP reasoning_content (DeepSeek): per gli assistant message generati
    // in thinking mode l'API DeepSeek IMPONE che il `reasoning_content` venga
    // ri-passato nelle richieste successive, altrimenti HTTP 400. Lo facciamo
    // viaggiare SOLO verso DeepSeek: per ogni coppia (wire, sorgente) con
    // role=="assistant" e `reasoning` non vuoto, copiamo il reasoning della
    // history nel campo wire. Gli altri dialetti non vedono mai il campo (resta
    // None -> omesso). Speculare al round-trip `thinking_signature` di Anthropic.
    if reasoning.dialect == ReasoningDialect::DeepSeek {
        for (wire, src) in messages.iter_mut().zip(req.messages.iter()) {
            if wire.role == "assistant" {
                if let Some(r) = src.reasoning.as_ref().filter(|r| !r.is_empty()) {
                    wire.reasoning_content = Some(r.clone());
                }
            }
        }
    }

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

    // tool_choice: dialetto OpenAI nativo, inoltrato tale e quale (canonicalizzato)
    // via il punto unico di mapping (regola L). Inviato solo quando c'e' un
    // vincolo riconosciuto E ci sono tool da scegliere (senza tools sarebbe
    // ignorato/rifiutato dall'API).
    let tool_choice = req
        .tool_choice
        .as_ref()
        .filter(|_| tools.is_some())
        .and_then(super::tool_choice::to_openai);

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        temperature,
        max_tokens,
        max_completion_tokens,
        reasoning_effort,
        extra_body,
        tools,
        tool_choice,
        response_format: req.response_format.clone(),
        stream: if stream { Some(true) } else { None },
        stream_options: if stream {
            Some(StreamOptions { include_usage: true })
        } else {
            None
        },
    }
}

/// Alcuni provider OpenAI-compat stretti (es. Mistral) RIFIUTANO con HTTP 400
/// ("Expected last role User or Tool (or Assistant with prefix True) ... but got
/// assistant") una richiesta il cui ULTIMO messaggio ha role "assistant" senza
/// tool-call pendenti. Nei run agentici la cronologia puo' terminare con un
/// assistant interlocutorio o, in cascade/fallback, con la risposta di un altro
/// provider. Rimuoviamo i trailing assistant SENZA tool_calls cosi' l'ultimo role
/// e' user/tool; gli assistant CON tool_calls pendenti restano (parte valida del
/// flusso tool). Porting del fix Python `_strip_trailing_assistant` perso nel
/// cutover a Rust. Mantiene sempre almeno un messaggio.
fn strip_trailing_assistant(messages: &mut Vec<WireMessage>) {
    while messages.len() > 1 {
        let drop_last = matches!(
            messages.last(),
            Some(m) if m.role == "assistant" && m.tool_calls.is_none()
        );
        if drop_last {
            messages.pop();
        } else {
            break;
        }
    }
}

/// True per i provider che esigono come ULTIMO messaggio role `user` o `tool`
/// (assistant trailing rifiutato dall'API). E' una proprieta' del PROVIDER, non
/// del modello (regola G: nessun model_id hardcoded).
fn provider_requires_user_or_tool_last(provider: &str) -> bool {
    provider == "mistral"
}

/// Converte un [`crate::types::LlmMessage`] nel formato wire OpenAI.
///
/// Il content e' una stringa nel caso semplice. Quando e' una lista di blocchi
/// (`MessageContent::Blocks`):
///   - se contiene blocchi immagine (`image_url`) si emette un content ARRAY
///     nativo OpenAI (`[{type:"text",...}, {type:"image_url", image_url:{url}}]`)
///     cosi' la capability vision e' preservata (regola: il gateway non deve
///     perdere le immagini quando elimineremo `brain/providers`);
///   - altrimenti (solo testo / tool_result) si ricade sulla serializzazione a
///     stringa (parita' col TS che fa `JSON.stringify`).
/// Per i messaggi `assistant` con tool-call il content puo' essere `null`.
fn to_wire_message(msg: &crate::types::LlmMessage) -> WireMessage {
    use crate::types::MessageContent;

    let content_value = match &msg.content {
        MessageContent::Text(s) => Some(WireContent::Text(s.clone())),
        MessageContent::Blocks(blocks) => {
            if blocks.iter().any(|b| b.kind == "image_url") {
                Some(WireContent::Parts(blocks_to_openai_parts(blocks)))
            } else {
                // Nessuna immagine: parita' col TS (JSON.stringify dei blocchi).
                serde_json::to_string(blocks)
                    .ok()
                    .map(WireContent::Text)
                    .or(Some(WireContent::Text(String::new())))
            }
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
            MessageContent::Text(s) if !s.is_empty() => Some(WireContent::Text(s.clone())),
            _ => None,
        }
    } else {
        content_value
    };

    WireMessage {
        role: msg.role.clone(),
        content,
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls,
        name: msg.name.clone(),
        // Popolato a valle in `build_request_body` SOLO per il dialetto DeepSeek
        // (round-trip del reasoning_content): qui resta None, neutro per gli altri.
        reasoning_content: None,
    }
}

/// Mappa i blocchi del contratto nel content array nativo OpenAI. I blocchi
/// `image_url` mantengono il formato OpenAI nativo (`{type:"image_url",
/// image_url:{url, detail?}}`, dove `url` puo' essere `http(s)` o
/// `data:<mime>;base64,<...>`). I blocchi testuali diventano
/// `{type:"text", text}`. I blocchi `tool_result` (qui inattesi nel content
/// array) sono serializzati come testo per non perderne il payload.
fn blocks_to_openai_parts(blocks: &[crate::types::LlmContentBlock]) -> Vec<serde_json::Value> {
    blocks
        .iter()
        .filter_map(|b| match b.kind.as_str() {
            "image_url" => b
                .image_url
                .as_ref()
                .map(|iu| serde_json::json!({"type": "image_url", "image_url": iu})),
            "text" => Some(serde_json::json!({
                "type": "text",
                "text": b.text.clone().unwrap_or_default(),
            })),
            _ => b.content.as_ref().map(|c| {
                serde_json::json!({"type": "text", "text": c})
            }),
        })
        .collect()
}

/// Parsa il body 200 di `/chat/completions` in [`ChatCompletion`] con errore
/// CONTESTUALIZZATO: provider + causa serde (campo mancante/tipo inatteso, con
/// riga e colonna), mai il contenuto del body (regola F). Funzione PURA
/// (testabile senza rete); punto unico del parse non-streaming (regola L).
fn parse_chat_completion(provider: &str, body: &str) -> anyhow::Result<ChatCompletion> {
    serde_json::from_str(body).map_err(|e| {
        // Causa serde troncata: per gli invalid-type su stringhe serde include
        // il valore nel messaggio; il taglio evita di trascinare contenuto di
        // risposta nel canale d'errore (regola F) mantenendo campo/riga/colonna.
        let cause: String = e.to_string().chars().take(200).collect();
        anyhow::anyhow!("{provider}: risposta 200 non decodificabile come ChatCompletion ({cause})")
    })
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
                // Firma per-call specifica di Gemini: assente sui provider
                // OpenAI-compatibili.
                thought_signature: None,
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

/// Classe di errore provider ai fini della strategia retry/cooldown.
/// Punto unico (regola L): i call site (`run_fallback`, streaming) NON
/// reimplementano il match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Crediti/quota/fatturazione: il provider e' inutilizzabile finche' non si
    /// ricarica. NIENTE retry; cooldown lungo (mark_billing).
    Billing,
    /// Errore lato richiesta o configurazione (4xx client): 400 invalid_request,
    /// 401/403 auth o modello non abilitato, 404 model not found, 422. Ritentare
    /// NON aiuta e mettere in cooldown il PROVIDER e' sbagliato (il problema e' la
    /// singola richiesta o il singolo modello). NIENTE retry, NIENTE cooldown.
    ClientError,
    /// Transitorio (429 rate-limit, 5xx, timeout, connessione) o ignoto:
    /// ritentabile con backoff. Solo dopo l'esaurimento dei retry si applica un
    /// cooldown breve (transient).
    Transient,
}

/// Errore HTTP di un provider, con lo status NUMERICO (segnale CERTO) e il
/// codice d'errore STRUTTURATO estratto dal JSON (`error.code`/`error.type`/
/// `error.status`, identificatore macchina STABILE). Sostituisce la
/// classificazione fragile sul testo del messaggio (regola H): il testo puo'
/// cambiare per provider/versione/lingua, lo status e il codice no.
///
/// `Display` e' IDENTICO al vecchio `bail!("{provider} HTTP {status}: {body}")`
/// cosi' i chiamanti legacy che leggono `to_string()` (es. `is_billing_error`
/// in `fallback.rs`) non cambiano comportamento, mentre il codice nuovo fa
/// `downcast` per accedere ai campi strutturati.
#[derive(Debug)]
pub struct ProviderHttpError {
    pub provider: String,
    pub status: u16,
    /// Codice d'errore strutturato dal body JSON (lowercase), se presente.
    pub code: Option<String>,
    /// Secondi indicati dall'header `Retry-After` (RFC 9457/7231), se il provider
    /// lo fornisce (es. Mistral/OpenAI su 429). Segnale AUTORITATIVO di quanto
    /// attendere prima di ritentare: ha precedenza sul backoff calcolato.
    pub retry_after_seconds: Option<u64>,
    /// Body grezzo, SOLO per logging/display: mai usato per classificare.
    pub message: String,
}

impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} HTTP {}: {}", self.provider, self.status, self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

impl ProviderHttpError {
    /// Costruisce dall'HTTP status + body grezzo, estraendo il codice d'errore
    /// STRUTTURATO dal JSON (non dalla prosa).
    pub fn from_response(provider: &str, status: u16, body: String) -> Self {
        let code = extract_structured_error_code(&body);
        Self {
            provider: provider.to_string(),
            status,
            code,
            retry_after_seconds: None,
            message: body,
        }
    }

    /// Imposta i secondi di `Retry-After` (builder). `None` lascia il default.
    pub fn with_retry_after(mut self, secs: Option<u64>) -> Self {
        self.retry_after_seconds = secs;
        self
    }
}

/// Parsa l'header `Retry-After` in secondi. RFC 7231: gestiamo il formato
/// "delta-seconds" (intero); una data HTTP ritorna `None` (ripiego sul backoff
/// calcolato). Punto unico (regola L): i provider lo leggono da qui.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    // Clamp difensivo: un Retry-After assurdo non deve bloccare la richiesta per
    // ore; il caller applica comunque il proprio tetto.
    raw.trim().parse::<u64>().ok().map(|s| s.min(3600))
}

/// Costruisce un [`ProviderHttpError`] da una `Response` non-2xx catturando
/// status, header `Retry-After` e body (async: consuma la response). Punto unico
/// della costruzione errore HTTP dei provider OpenAI-compat (regola L).
pub async fn provider_http_error(provider: &str, resp: reqwest::Response) -> ProviderHttpError {
    let status = resp.status().as_u16();
    let retry_after = parse_retry_after(resp.headers());
    let body = resp.text().await.unwrap_or_default();
    ProviderHttpError::from_response(provider, status, body).with_retry_after(retry_after)
}

/// Estrae il codice d'errore STRUTTURATO da un body JSON di errore provider.
/// Cerca (in ordine) `error.type`, `error.code` (se stringa), `error.status`
/// (enum Google), e i corrispettivi top-level. Ritorna il valore in lowercase.
/// NB: parsa CAMPI JSON del contratto macchina del provider, non testo libero.
fn extract_structured_error_code(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let candidates = [
        v.pointer("/error/type"),
        v.pointer("/error/code"),
        v.pointer("/error/status"),
        v.get("type"),
        v.get("code"),
        v.get("status"),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(s) = c.as_str() {
            if !s.is_empty() {
                return Some(s.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Classifica in modo DETERMINISTICO da status HTTP + codice strutturato.
/// Lo status e' il segnale primario; il codice ESCALA a Billing solo un 429/402
/// quando e' un identificatore di credito inequivocabile (non prosa).
fn classify_by_status_code(status: u16, code: Option<&str>) -> ProviderErrorKind {
    // Codice STRUTTURATO di credito/fatturazione (identificatore macchina).
    // Conservativo: solo codici inequivocabili, cosi' non si scambia un
    // rate-limit (429) per un provider "down per credito".
    if let Some(c) = code {
        if c.contains("insufficient_quota")
            || c.contains("billing")
            || c.contains("payment_required")
            || c.contains("account_deactivated")
        {
            return ProviderErrorKind::Billing;
        }
    }
    // Mappatura verificata sulle tabelle ufficiali (Anthropic/OpenAI, 2026):
    //   402 billing_error (Anthropic) -> Billing;
    //   400/401/403/404/413/422 (+ altri 4xx non ritentabili) -> ClientError:
    //     ritentare NON aiuta (413 request_too_large: la richiesta e' troppo
    //     grande, un retro identico rifallisce);
    //   408/425/429 (timeout/too-early/rate-limit), 5xx e 529 overloaded (Anthropic)
    //     -> Transient (ritentabili).
    match status {
        402 => ProviderErrorKind::Billing,
        400 | 401 | 403 | 404 | 405 | 406 | 409 | 410 | 413 | 415 | 422 => {
            ProviderErrorKind::ClientError
        }
        _ => ProviderErrorKind::Transient,
    }
}

/// Classifica un errore provider in modo DETERMINISTICO (regola H, punto unico
/// regola L). Ordine dei segnali CERTI:
///   1. [`ProviderHttpError`] nella catena -> status + codice strutturato;
///   2. `reqwest::Error` -> `status()` se presente, altrimenti timeout/connessione
///      (predicati tipizzati) -> transitorio;
///   3. sconosciuto (es. parse di un body 200) -> transitorio (default sicuro:
///      ritentare e' innocuo, non penalizza un provider sano).
/// NESSUNA classificazione basata sul testo del messaggio.
pub fn classify_provider_error(err: &anyhow::Error) -> ProviderErrorKind {
    for cause in err.chain() {
        if let Some(http) = cause.downcast_ref::<ProviderHttpError>() {
            return classify_by_status_code(http.status, http.code.as_deref());
        }
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = re.status() {
                return classify_by_status_code(status.as_u16(), None);
            }
            // Nessuno status = errore di trasporto (timeout/connessione/body):
            // transitorio CERTO (predicati tipizzati, non testo).
            return ProviderErrorKind::Transient;
        }
    }
    ProviderErrorKind::Transient
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
    /// Vincolo di scelta tool in formato OpenAI nativo (stringa o oggetto).
    /// Inoltrato tale e quale; omesso quando assente.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
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

/// Corpo della richiesta `POST /images/generations` (dialetto OpenAI Images).
#[derive(Debug, Serialize)]
struct ImageGenWireRequest {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    response_format: String,
}

/// Risposta di `POST /images/generations`: `{ "data": [{ "b64_json"|"url" }] }`.
#[derive(Debug, Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    data: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// Risposta di `POST /audio/transcriptions` con `response_format=json`:
/// `{ "text": "..." }`.
#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: String,
    // Serializziamo sempre `content` (anche null) per i messaggi assistant con
    // tool-call, dove l'API richiede esplicitamente `content: null`.
    content: Option<WireContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    // Reasoning del turno assistant prodotto in thinking mode da DeepSeek, da
    // RI-PASSARE all'API nelle richieste successive (vincolo HTTP 400: "The
    // reasoning_content in the thinking mode must be passed back to the API").
    // Valorizzato SOLO per il dialetto DeepSeek in `build_request_body`; per gli
    // altri provider resta `None` (omesso) cosi' il campo non viaggia mai verso
    // chi non lo conosce.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

/// Content di un messaggio nel wire OpenAI: stringa (caso semplice) o array di
/// parti tipizzate (testo + immagini, per le richieste vision). L'enum untagged
/// serializza direttamente al valore JSON (stringa o array) atteso dall'API.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<serde_json::Value>),
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
    /// Content della risposta. Il dialetto OpenAI classico e' una STRINGA, ma
    /// Mistral (contratto ufficiale: `content: string | ContentChunk[]`) puo'
    /// rispondere con un ARRAY di chunk (`{type:"text", text}`, reference,
    /// thinking). Un `Option<String>` rigido faceva fallire l'intero parse
    /// ("error decoding response body", classificato transitorio e ritentato a
    /// vuoto): il deserializzatore tollerante estrae il testo dai chunk `text`
    /// e ignora gli altri.
    #[serde(default, deserialize_with = "deserialize_lenient_content")]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<RespToolCall>>,
    /// Reasoning DeepSeek (campo separato dal content). Assente sugli altri
    /// provider OpenAI-compat.
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Deserializza un content wire tollerante: stringa as-is, array di chunk
/// concatenando i soli `{type:"text"}` (il resto — reference, thinking — non e'
/// testo di risposta), `null`/assente -> `None`. Punto unico riusato da
/// response e delta streaming (regola L).
fn deserialize_lenient_content<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(v.and_then(|v| match v {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        // null gia' mappato a None da Option; altri tipi inattesi -> None
        // (il chiamante degrada a content vuoto, non a parse fallito).
        _ => None,
    }))
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
    /// Stesso contratto tollerante del content non-streaming (stringa o array
    /// di chunk): un delta array altrimenti veniva scartato in silenzio dal
    /// parser SSE (risposta troncata senza errore).
    #[serde(default, deserialize_with = "deserialize_lenient_content")]
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
                reasoning: None,
            }],
            temperature: Some(0.5),
            max_tokens: Some(256),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
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
    fn parse_models_estrae_id_ordina_e_deduplica() {
        // Forma canonica della risposta `GET /models` (OpenAI/Mistral/DeepSeek/vLLM).
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "gpt-4o", "object": "model" },
                { "id": "gpt-4o-mini", "object": "model" },
                { "id": "gpt-4o", "object": "model" }, // duplicato
            ]
        });
        let models = parse_models_response(&body);
        // Ordinato e deduplicato.
        assert_eq!(models, vec!["gpt-4o", "gpt-4o-mini"]);
    }

    #[test]
    fn parse_models_meta_estrae_finestra_dichiarata() {
        // Dialetto Mistral: `max_context_length` in data[]. OpenAI/DeepSeek non
        // hanno il campo -> None (finestra IGNOTA, mai inventata: regola H).
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "mistral-medium-3", "max_context_length": 131072 },
                { "id": "mistral-ocr-latest" },                     // senza campo
                { "id": "mistral-rotto", "max_context_length": 0 }, // non positivo
            ]
        });
        let metas = parse_models_meta_response(&body);
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].id, "mistral-medium-3");
        assert_eq!(metas[0].context_window, Some(131072));
        assert_eq!(metas[1].id, "mistral-ocr-latest");
        assert_eq!(metas[1].context_window, None);
        // Valore non positivo = non dichiarato (mai una finestra inventata).
        assert_eq!(metas[2].context_window, None);
    }

    #[test]
    fn parse_models_salta_id_assenti_o_vuoti_e_gestisce_data_mancante() {
        let body = serde_json::json!({
            "data": [
                { "id": "deepseek-chat" },
                { "object": "model" },          // niente id
                { "id": "" },                    // id vuoto
                { "id": "  mistral-small  " },   // trimmato
            ]
        });
        let models = parse_models_response(&body);
        assert_eq!(models, vec!["deepseek-chat", "mistral-small"]);

        // Risposta senza `data`: lista vuota, non panico.
        let vuoto = serde_json::json!({ "object": "list" });
        assert!(parse_models_response(&vuoto).is_empty());
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

    /// Round-trip reasoning_content (DeepSeek): un assistant message con
    /// `reasoning=Some(...)` DEVE comparire come `messages[i].reasoning_content`
    /// nel body SOLO per il dialetto DeepSeek (vincolo HTTP 400). Per i dialetti
    /// non-DeepSeek il campo NON deve viaggiare (assente).
    #[test]
    fn reasoning_content_round_trip_solo_deepseek() {
        // Richiesta con un assistant in thinking mode (porta reasoning) seguito da
        // un turno user: speculare a una history agentica multi-turno DeepSeek.
        let mut req = sample_request();
        req.messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("rispondo".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: Some("ho ragionato cosi'".to_string()),
            },
            LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("continua".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                // L'utente non porta reasoning: non deve mai comparire reasoning_content.
                reasoning: Some("spurio-da-ignorare".to_string()),
            },
        ];

        // Dialetto DeepSeek: il reasoning dell'assistant e' ri-passato.
        let deepseek = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: true,
            effort: None,
        };
        let body = build_request_body(&req, false, &deepseek);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json["messages"][0]["reasoning_content"], "ho ragionato cosi'",
            "l'assistant DeepSeek deve ri-passare il reasoning_content"
        );
        // Lo user NON deve portare reasoning_content (solo i ruoli assistant).
        assert!(
            json["messages"][1].get("reasoning_content").is_none(),
            "lo user non deve mai esporre reasoning_content"
        );

        // Dialetto non-DeepSeek (base): il campo non viaggia mai.
        let body_base = build_request_body(&req, false, &ResolvedReasoning::none());
        let json_base = serde_json::to_value(&body_base).unwrap();
        assert!(
            json_base["messages"][0].get("reasoning_content").is_none(),
            "fuori dal dialetto DeepSeek il reasoning_content non deve essere inviato"
        );
    }

    fn edit_tool() -> crate::types::LlmToolDefinition {
        crate::types::LlmToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ToolFunctionDef {
                name: "edit_file".to_string(),
                description: Some("modifica un file".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }
    }

    #[test]
    fn tool_choice_required_passthrough_nativo_openai() {
        // Con tools presenti il vincolo "required" e' inoltrato tale e quale
        // (dialetto OpenAI nativo): e' questo che FORZA il modello a chiamare il
        // tool invece di descrivere (fix del bug tool_choice droppato).
        let mut req = sample_request();
        req.tools = Some(vec![edit_tool()]);
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();
        assert_eq!(json["tool_choice"], "required");
        // Oggetto funzione: passthrough nella forma OpenAI canonica.
        req.tool_choice = Some(serde_json::json!({"type": "function", "function": {"name": "edit_file"}}));
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();
        assert_eq!(json["tool_choice"]["type"], "function");
        assert_eq!(json["tool_choice"]["function"]["name"], "edit_file");
    }

    #[test]
    fn tool_choice_omesso_senza_tools() {
        // tool_choice senza tools non ha senso: il campo non viene inviato.
        let mut req = sample_request();
        req.tools = None;
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();
        assert!(json.get("tool_choice").is_none());
        // Senza tool_choice (caso storico): campo assente.
        let mut req2 = sample_request();
        req2.tools = Some(vec![edit_tool()]);
        let json2 = serde_json::to_value(build_request_body(&req2, false, &ResolvedReasoning::none()))
            .unwrap();
        assert!(json2.get("tool_choice").is_none());
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

    // ── Content tollerante (contratto Mistral: string | ContentChunk[]) ─────

    #[test]
    fn content_array_di_chunk_estrae_il_testo() {
        // Mistral puo' rispondere content come array di chunk: i `text` vanno
        // concatenati, reference/thinking ignorati. Prima falliva l'INTERO
        // parse -> "error decoding response body" (18 occorrenze in history il
        // 2026-07-06) ritentato a vuoto come transitorio.
        let raw = r#"{
            "choices": [{
                "message": {"content": [
                    {"type": "text", "text": "Ciao "},
                    {"type": "reference", "reference_ids": [1]},
                    {"type": "text", "text": "mondo"}
                ]},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed = parse_chat_completion("mistral", raw).expect("parse tollerante");
        let resp = from_chat_completion(parsed, "m".to_string(), "mistral", 1).unwrap();
        assert_eq!(resp.content, "Ciao mondo");
    }

    #[test]
    fn content_stringa_resta_invariato() {
        let raw = r#"{
            "choices": [{"message": {"content": "semplice"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed = parse_chat_completion("mistral", raw).expect("parse stringa");
        let resp = from_chat_completion(parsed, "m".to_string(), "mistral", 1).unwrap();
        assert_eq!(resp.content, "semplice");
    }

    #[test]
    fn sse_delta_content_array_non_scartato() {
        // Delta streaming con content array: prima il parser SSE scartava la
        // riga in silenzio (risposta troncata); ora estrae il testo.
        let raw = r#"{
            "choices": [{"delta": {"content": [{"type": "text", "text": "pezzo"}]}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "mistral", "m").expect("chunk emesso");
        assert_eq!(out.delta, "pezzo");
    }

    #[test]
    fn parse_fallito_ha_errore_contestualizzato() {
        // Il messaggio deve dire provider + causa serde (diagnostico), MAI il
        // generico "error decoding response body" ne' il contenuto del body.
        let err = parse_chat_completion("mistral", "<html>proxy error</html>").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mistral"), "manca il provider: {msg}");
        assert!(
            msg.contains("non decodificabile come ChatCompletion"),
            "manca il contesto: {msg}"
        );
        assert!(!msg.contains("proxy error"), "il body non va nel messaggio: {msg}");
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

    // --- Vision: blocchi immagine nel content array (passo 3) --------------

    fn image_block(url: &str) -> crate::types::LlmContentBlock {
        crate::types::LlmContentBlock {
            kind: "image_url".to_string(),
            text: None,
            image_url: Some(serde_json::json!({ "url": url })),
            tool_use_id: None,
            content: None,
        }
    }

    fn text_block(text: &str) -> crate::types::LlmContentBlock {
        crate::types::LlmContentBlock {
            kind: "text".to_string(),
            text: Some(text.to_string()),
            image_url: None,
            tool_use_id: None,
            content: None,
        }
    }

    #[test]
    fn vision_blocco_immagine_diventa_content_array_nativo() {
        let mut req = sample_request();
        req.messages[0].content = MessageContent::Blocks(vec![
            text_block("descrivi"),
            image_block("data:image/png;base64,AAAA"),
        ]);
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();

        let content = &json["messages"][0]["content"];
        // Il content e' un ARRAY (formato OpenAI vision), non una stringa.
        let arr = content.as_array().expect("content array per vision");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "descrivi");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(arr[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn vision_url_http_preservato() {
        let mut req = sample_request();
        req.messages[0].content =
            MessageContent::Blocks(vec![image_block("https://example.com/x.png")]);
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();
        let arr = json["messages"][0]["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
        assert_eq!(arr[0]["image_url"]["url"], "https://example.com/x.png");
    }

    #[test]
    fn blocchi_senza_immagine_restano_stringa() {
        // Nessuna immagine -> parita' col TS (content serializzato a stringa).
        let mut req = sample_request();
        req.messages[0].content = MessageContent::Blocks(vec![text_block("solo testo")]);
        let json = serde_json::to_value(build_request_body(&req, false, &ResolvedReasoning::none()))
            .unwrap();
        assert!(json["messages"][0]["content"].is_string());
    }

    // --- Image generation (dialetto OpenAI Images) ------------------------

    #[test]
    fn images_response_mappa_b64_e_filtra_vuoti() {
        let raw = r#"{
            "data": [
                {"b64_json": "AAAA"},
                {"b64_json": ""},
                {"url": "https://example.com/x.png"}
            ]
        }"#;
        let parsed: ImagesResponse = serde_json::from_str(raw).unwrap();
        let out = from_images_response(parsed, "gpt-image-1".to_string(), "openai", 7);
        assert_eq!(out.model_used, "gpt-image-1");
        assert_eq!(out.provider_used, "openai");
        assert_eq!(out.latency_ms, 7);
        assert_eq!(out.images.len(), 3);
        assert_eq!(out.images[0].b64_json.as_deref(), Some("AAAA"));
        // base64 vuoto -> None (non si propaga una stringa vuota).
        assert!(out.images[1].b64_json.is_none());
        assert!(out.images[1].url.is_none());
        assert_eq!(out.images[2].url.as_deref(), Some("https://example.com/x.png"));
        // OpenAI Images non dichiara il mime.
        assert!(out.images[0].mime.is_none());
    }

    #[test]
    fn images_request_body_imposta_response_format_b64() {
        let body = ImageGenWireRequest {
            model: "gpt-image-1".to_string(),
            prompt: "un gatto".to_string(),
            n: Some(2),
            size: Some("1024x1024".to_string()),
            response_format: "b64_json".to_string(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "gpt-image-1");
        assert_eq!(json["prompt"], "un gatto");
        assert_eq!(json["n"], 2);
        assert_eq!(json["size"], "1024x1024");
        assert_eq!(json["response_format"], "b64_json");
    }

    // --- Audio transcription (dialetto OpenAI Audio) ----------------------

    #[test]
    fn transcription_response_estrae_text() {
        let raw = r#"{ "text": "ciao mondo", "language": "it" }"#;
        let parsed: TranscriptionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.text, "ciao mondo");
        // Risposta senza text -> stringa vuota (tollerante, non panico).
        let vuoto: TranscriptionResponse = serde_json::from_str("{}").unwrap();
        assert!(vuoto.text.is_empty());
    }

    #[test]
    fn mime_from_filename_mappa_estensioni_audio() {
        assert_eq!(mime_from_filename("audio.mp3"), Some("audio/mpeg"));
        assert_eq!(mime_from_filename("a.WAV"), Some("audio/wav"));
        assert_eq!(mime_from_filename("nota.m4a"), Some("audio/mp4"));
        assert_eq!(mime_from_filename("voce.ogg"), Some("audio/ogg"));
        assert_eq!(mime_from_filename("x.flac"), Some("audio/flac"));
        // Estensione non audio o assente -> None (default reqwest).
        assert_eq!(mime_from_filename("file.bin"), None);
        assert_eq!(mime_from_filename("senza_estensione"), None);
    }

    #[test]
    fn mime_from_audio_format_mappa_formati_tts() {
        assert_eq!(mime_from_audio_format(Some("mp3")), "audio/mpeg");
        assert_eq!(mime_from_audio_format(Some("WAV")), "audio/wav");
        assert_eq!(mime_from_audio_format(Some("opus")), "audio/opus");
        assert_eq!(mime_from_audio_format(Some("aac")), "audio/aac");
        assert_eq!(mime_from_audio_format(Some("flac")), "audio/flac");
        assert_eq!(mime_from_audio_format(Some("pcm")), "audio/pcm");
        // Formato assente o sconosciuto -> default mp3.
        assert_eq!(mime_from_audio_format(None), "audio/mpeg");
        assert_eq!(mime_from_audio_format(Some("xyz")), "audio/mpeg");
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

    #[test]
    fn extract_structured_error_code_da_json() {
        // OpenAI/DeepSeek/Mistral: error.code / error.type.
        assert_eq!(
            extract_structured_error_code(
                r#"{"error":{"code":"insufficient_quota","type":"insufficient_quota"}}"#
            )
            .as_deref(),
            Some("insufficient_quota")
        );
        assert_eq!(
            extract_structured_error_code(
                r#"{"error":{"type":"invalid_request_error","message":"bad"}}"#
            )
            .as_deref(),
            Some("invalid_request_error")
        );
        // Google: error.code e' NUMERICO -> si usa error.status (enum).
        assert_eq!(
            extract_structured_error_code(
                r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"x"}}"#
            )
            .as_deref(),
            Some("invalid_argument")
        );
        // Body non-JSON o senza campi: None.
        assert_eq!(extract_structured_error_code("502 Bad Gateway (html)"), None);
    }

    #[test]
    fn parse_retry_after_delta_seconds_e_clamp() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None); // header assente
        h.insert(RETRY_AFTER, HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&h), Some(5));
        // Valore assurdo: clamp a 3600.
        h.insert(RETRY_AFTER, HeaderValue::from_static("999999"));
        assert_eq!(parse_retry_after(&h), Some(3600));
        // Formato data HTTP (non delta-seconds): non gestito -> None.
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn classify_deterministica_da_status_e_codice() {
        let http = |status, code: Option<&str>| {
            anyhow::Error::new(ProviderHttpError {
                provider: "p".into(),
                status,
                code: code.map(|c| c.to_string()),
                retry_after_seconds: None,
                message: "body".into(),
            })
        };
        // Codice di credito strutturato -> Billing anche su 429 (OpenAI usa 429).
        assert_eq!(
            classify_provider_error(&http(429, Some("insufficient_quota"))),
            ProviderErrorKind::Billing
        );
        assert_eq!(
            classify_provider_error(&http(402, None)),
            ProviderErrorKind::Billing
        );
        // 4xx client -> ClientError (niente retry, niente cooldown).
        assert_eq!(
            classify_provider_error(&http(400, Some("invalid_request_error"))),
            ProviderErrorKind::ClientError
        );
        assert_eq!(
            classify_provider_error(&http(403, Some("permission_denied"))),
            ProviderErrorKind::ClientError
        );
        assert_eq!(
            classify_provider_error(&http(404, None)),
            ProviderErrorKind::ClientError
        );
        // 413 request_too_large (OpenAI/Anthropic): non ritentabile -> ClientError.
        assert_eq!(
            classify_provider_error(&http(413, None)),
            ProviderErrorKind::ClientError
        );
        // 429 rate-limit puro (senza codice credito) -> Transient (ritentabile).
        assert_eq!(
            classify_provider_error(&http(429, Some("rate_limit_exceeded"))),
            ProviderErrorKind::Transient
        );
        assert_eq!(
            classify_provider_error(&http(429, None)),
            ProviderErrorKind::Transient
        );
        // 5xx server e 529 overloaded (Anthropic) -> Transient.
        assert_eq!(
            classify_provider_error(&http(503, None)),
            ProviderErrorKind::Transient
        );
        assert_eq!(
            classify_provider_error(&http(529, Some("overloaded_error"))),
            ProviderErrorKind::Transient
        );
        // Errore non-HTTP (es. parse) -> default sicuro Transient.
        assert_eq!(
            classify_provider_error(&anyhow::anyhow!("json parse fallito")),
            ProviderErrorKind::Transient
        );
    }
}
