//! Tipi del contratto LLM del gateway.
//!
//! Fedeli a `packages/shared/src/llm-types.ts` (lingua franca: OpenAI Chat
//! Completions). Il client esistente in `crates/mcp-core/src/nexus_gateway.rs`
//! usa una versione ridotta (`GwRequest`/`GwResponse`); qui modelliamo il
//! contratto COMPLETO che il server deve deserializzare. Alla Fase 6 il client
//! mcp-core verra' allineato a riusare questi tipi (regola L: punto unico).

use serde::{Deserialize, Serialize};

/// Tier di sensibilita' del dato (0 = pubblico ... 3 = massimo riservato).
pub type SensitivityTier = u8;

/// Blocco di contenuto strutturato di un messaggio (testo, immagine, risultato
/// di tool). Corrisponde a `LLMContentBlock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Chiamata a tool emessa dal modello (`LLMToolCall`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionCall,
    /// Firma opaca di reasoning (`thoughtSignature`) che Gemini 3 emette PER
    /// OGNI `functionCall` e IMPONE di ri-passare sulla rispettiva part nei
    /// turni con tool, altrimenti HTTP 400 INVALID_ARGUMENT ("Function call is
    /// missing a thought_signature in functionCall parts"). A differenza di
    /// Anthropic (una firma per blocco thinking, a livello di messaggio via
    /// [`LlmMessage::thinking_signature`]) qui la firma e' PER-CALL.
    /// Retrocompatibile: assente/`None` per tutti gli altri provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Definizione di un tool offerto al modello (`LLMToolDefinition`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Contenuto di un messaggio: stringa semplice oppure lista di blocchi.
/// Modella `string | LLMContentBlock[]` con un enum untagged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<LlmContentBlock>),
}

/// Messaggio della conversazione (`LLMMessage`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Firma opaca del blocco `thinking` di un turno assistant precedente
    /// (extended thinking Anthropic). Quando presente su un messaggio
    /// `assistant`, il provider la re-include come block `thinking` con
    /// `signature` nei turni con tool (l'API Anthropic la richiede, altrimenti
    /// HTTP 400). Retrocompatibile: assente/`None` per tutti gli altri provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// Testo del ragionamento (`reasoning_content`) di un turno assistant
    /// precedente generato in thinking mode da DeepSeek. Vincolo analogo al
    /// `thinking_signature` Anthropic: l'API DeepSeek IMPONE che, per gli
    /// assistant message prodotti in thinking mode, il `reasoning_content` venga
    /// RI-PASSATO nelle richieste successive, altrimenti HTTP 400 ("The
    /// reasoning_content in the thinking mode must be passed back to the API").
    /// Il chiamante lo rispedisce da [`LlmResponse::reasoning`] del turno
    /// precedente; il provider OpenAI-compat lo re-include nel wire SOLO per il
    /// dialetto DeepSeek (vedi `build_request_body`). Retrocompatibile:
    /// assente/`None` per tutti gli altri provider, che non vedono il campo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Metadati di tracciamento e tenancy della richiesta (`RequestMetadata`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
    #[serde(default)]
    pub sensitivity_tier: SensitivityTier,
    pub feature: String,
}

/// Richiesta di completion (`LLMRequest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<LlmToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Configurazione extended thinking richiesta dal chiamante. Quando
    /// `enabled` e' true il provider (oggi solo Anthropic) attiva la modalita'
    /// thinking. Retrocompatibile: `None` = nessuna richiesta di thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Vincolo di scelta del tool, in stile OpenAI Chat Completions (lingua
    /// franca del gateway). Governa quanto il modello e' OBBLIGATO a chiamare un
    /// tool: il brain lo imposta a `"required"` quando il force-action anti-loop
    /// / `progress_controller` devono costringere l'agente ad AGIRE invece di
    /// descrivere. Formati accettati (identici all'API OpenAI):
    ///   - stringa `"auto"`   -> il modello sceglie se chiamare un tool;
    ///   - stringa `"required"` -> il modello DEVE chiamare almeno un tool;
    ///   - stringa `"none"`   -> il modello NON deve chiamare tool;
    ///   - oggetto `{"type":"function","function":{"name":"X"}}` -> forza il tool `X`.
    /// Ogni provider lo rimappa al proprio dialetto nel rispettivo
    /// `build_request_body` (OpenAI-compat passthrough nativo; Anthropic
    /// `tool_choice` con `{type:any|tool|auto}`; Google
    /// `tool_config.function_calling_config.mode`). Retrocompatibile: `None` =
    /// nessun vincolo inviato (comportamento storico, equivalente ad `auto`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`,
    /// il gateway esegue ESATTAMENTE quel provider col `model` indicato
    /// (strippato dell'eventuale prefisso `provider/`), SENZA `policy.decide` e
    /// SENZA fallback cross-provider: se il provider e' in cooldown o non e'
    /// configurato, la richiesta fallisce (nessun ripiego su un altro provider).
    /// Serve al chiamante (mcp-core) che ha gia' deciso provider+modello via
    /// routing matrix DB, per evitare un secondo routing divergente nel gateway.
    /// Retrocompatibile: `None` = routing per tier + fallback (comportamento
    /// storico invariato).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Configurazione extended thinking (`thinking` di `LLMRequest`). `budget_tokens`
/// opzionale: se assente il provider usa il budget dai settings DB (regola G).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

/// Conteggio token consumati.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LlmUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token serviti da cache (prompt caching). Valorizzati nel passo cache;
    /// retrocompatibile: `None` finche' il provider non li riporta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce cache). Vedi sopra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
}

/// Informazioni sul re-routing per privacy (`privacy_rerouted`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyRerouted {
    pub provider: String,
    pub blocked_tier: u8,
    pub reason: String,
}

/// Risposta non-streaming (`LLMResponse`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    pub usage: LlmUsage,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
    pub finish_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy_rerouted: Option<PrivacyRerouted>,
    /// Testo del ragionamento (extended thinking) visibile, quando il provider
    /// lo emette. Retrocompatibile: `None` se non disponibile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` da ri-passare nei turni successivi con
    /// tool (Anthropic). Il chiamante la rispedisce via
    /// [`LlmMessage::thinking_signature`]. Retrocompatibile: `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
}

/// Delta di tool-call durante lo streaming.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallDeltaFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallDeltaFunction>,
}

/// Chunk di streaming (`LLMStreamChunk`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStreamChunk {
    pub delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_delta: Option<ToolCallDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    /// Delta del testo di reasoning (extended thinking) durante lo streaming.
    /// Retrocompatibile: `None` sui chunk che non portano thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_delta: Option<String>,
}

/// Stato di salute di un provider (`ProviderStatus`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub healthy: bool,
    pub last_check: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Messaggio di errore di billing (crediti esauriti). Presente solo se rilevato.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_error: Option<String>,
}

/// Richiesta di generazione immagine (`ImageGenRequest`). Speculare a
/// [`LlmRequest`] ma per il task image-generation: niente messaggi/tool, solo un
/// `prompt` testuale. Regola G: il `model` arriva sempre dal chiamante (nessun
/// default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    pub model: String,
    pub prompt: String,
    /// Numero di immagini da generare (default lato provider se assente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Dimensione richiesta (es. "1024x1024"); il formato dipende dal provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_image_gen()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Una immagine generata. I provider espongono o il base64 inline (OpenAI
/// `b64_json`, Vertex `bytesBase64Encoded`) o una URL temporanea (OpenAI
/// `response_format=url`): entrambi opzionali, almeno uno valorizzato. `mime`
/// presente quando il provider lo dichiara (Vertex `mimeType`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// Risposta di generazione immagine (`ImageGenResponse`). Speculare a
/// [`LlmResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResponse {
    pub images: Vec<GeneratedImage>,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di generazione video (`VideoGenRequest`, text-to-video). Speculare a
/// [`ImageGenRequest`] ma per il task video-gen: niente messaggi/tool, solo un
/// `prompt` testuale + la durata opzionale. Regola G: il `model` arriva sempre
/// dal chiamante (nessun default hardcoded). `pin_provider` ha la stessa
/// semantica di [`ImageGenRequest::pin_provider`] (bypass routing, esecuzione di
/// QUEL provider).
///
/// DIFFERENZA CHIAVE rispetto a image-gen: il backend (Vertex Veo) e' ASYNC
/// long-running (`:predictLongRunning` -> operation -> poll). Per l'MVP il polling
/// e' incapsulato DENTRO il gateway (richiesta/risposta sincrona per il client):
/// l'handler fa start + poll-loop con timeout DB-driven, poi ritorna il video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenRequest {
    pub model: String,
    pub prompt: String,
    /// Durata richiesta del video in secondi (default lato provider se assente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_video_gen()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di generazione video (`VideoGenResponse`). Il provider Veo puo'
/// restituire i byte del video inline (base64) oppure una `gcsUri` (URL Google
/// Cloud Storage). Entrambi opzionali, almeno uno valorizzato: il chiamante che
/// puo' salvare path-safe usa `video_base64`, altrimenti riporta la `url` con una
/// nota (regola H: niente fetch nascosto di una URL esterna). Speculare a
/// [`ImageGenResponse`]/[`TtsResponse`] per i campi di tracciamento.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenResponse {
    /// Video codificato base64 inline (Veo `bytesBase64Encoded`). `None` quando il
    /// provider risponde solo con una `gcsUri`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_base64: Option<String>,
    /// URL del video (Veo `gcsUri`), quando il provider non emette i byte inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// MIME del video prodotto (es. `video/mp4`): dal campo `mimeType` del
    /// provider quando presente, altrimenti un default coerente.
    pub mime: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di trascrizione audio (`TranscribeRequest`, speech-to-text).
/// Speculare a [`ImageGenRequest`] ma per il task audio-in: niente messaggi/tool,
/// solo l'audio (base64) + il modello. Regola G: il `model` arriva sempre dal
/// chiamante (nessun default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeRequest {
    pub model: String,
    /// Audio sorgente codificato base64 (il gateway lo decodifica e lo invia come
    /// multipart `file` al provider). Niente URL: il gateway non fa fetch esterni.
    pub audio_base64: String,
    /// MIME dell'audio (es. `audio/mpeg`, `audio/wav`): usato per nominare la part
    /// multipart con l'estensione corretta. `None` => estensione generica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Lingua dell'audio in ISO-639-1 (es. `it`, `en`), opzionale: migliora
    /// accuratezza/latency. `None` => il provider la rileva da solo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_audio_in()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di trascrizione audio (`TranscribeResponse`). Speculare a
/// [`ImageGenResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResponse {
    /// Testo trascritto dall'audio.
    pub text: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di sintesi vocale (`TtsRequest`, text-to-speech). Speculare a
/// [`ImageGenRequest`] ma per il task audio-out: niente messaggi/tool, solo il
/// testo da pronunciare + il modello. Regola G: il `model` arriva sempre dal
/// chiamante (nessun default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub model: String,
    /// Testo da convertire in audio.
    pub input: String,
    /// Voce del modello TTS (es. `alloy`, `nova`): opzionale, default lato
    /// provider se assente. Non e' un nome modello (regola G): e' un timbro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Formato audio richiesto (es. `mp3`, `wav`, `opus`, `flac`): opzionale,
    /// default lato provider (`mp3`) se assente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_audio_out()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di sintesi vocale (`TtsResponse`). Il provider risponde con BYTES
/// binari (Content-Type `audio/mpeg`): il gateway li legge e li ritorna in base64
/// al client, coerente con il resto del contratto JSON. Speculare a
/// [`ImageGenResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsResponse {
    /// Audio sintetizzato codificato base64 (il client lo decodifica e lo salva).
    pub audio_base64: String,
    /// MIME dell'audio prodotto (es. `audio/mpeg`): dal Content-Type della risposta
    /// del provider, o derivato dal `response_format` richiesto.
    pub mime: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Voce della tabella di alias modello (`ModelAliasEntry`, da model-aliases.yaml).
///
/// I tre campi modello sono `Option` perche' nello YAML possono valere `null`
/// (es. alias solo-onprem o alias di fallback senza on-premise). `#[serde(default)]`
/// li rende anche assenti-tolleranti: una chiave mancante equivale a `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAliasEntry {
    #[serde(default)]
    pub cloud_primary: Option<String>,
    #[serde(default)]
    pub cloud_secondary: Option<String>,
    #[serde(default)]
    pub onprem: Option<String>,
    pub min_tier: SensitivityTier,
    pub max_tier: SensitivityTier,
}
