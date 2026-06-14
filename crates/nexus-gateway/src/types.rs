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
