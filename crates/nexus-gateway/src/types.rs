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
    /// Durata del RUN che ha originato questa richiesta, in secondi.
    ///
    /// Il gateway deriva i propri budget (`request_budget`, `per_attempt`) dal
    /// run: `budget = run / min_turns`. Ma li derivava una volta sola all'avvio,
    /// dal default globale `orchestrator.subagent_default_timeout_s` (300s),
    /// mentre il run vero e' PER FIGURA (`nexus_subagent_definitions.timeout_s`:
    /// `review` 240, `implement` 600). Una figura da 240s riceveva tentativi
    /// dimensionati su 300: il gateway prometteva turni che il cronometro del
    /// chiamante non poteva mantenere.
    ///
    /// Il chiamante e' l'unico a conoscere questo numero, quindi lo porta con se'.
    /// Retrocompatibile in entrambi i versi: assente (client vecchio) = i timeout
    /// per-processo di sempre; ignoto (gateway vecchio) = serde lo scarta.
    /// Puo' solo STRINGERE i budget, mai allungarli oltre il default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_timeout_secs: Option<u64>,
    pub metadata: RequestMetadata,
}

#[cfg(test)]
mod test_wire_run_timeout {
    use super::*;

    /// Client VECCHIO -> gateway NUOVO.
    ///
    /// Il corpo e' lo stesso sottoinsieme di campi che `scripts/onprem-smoke.sh`
    /// invia oggi in produzione: se questo test diventa rosso, quel corpo smette
    /// di essere accettato e lo smoke test on-prem si rompe in silenzio.
    #[test]
    fn un_corpo_senza_il_campo_resta_valido() {
        let corpo = r#"{
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "ciao"}],
            "metadata": {"tenant_id":"t","user_id":"u","request_id":"r","sensitivity_tier":0,"feature":"smoke"}
        }"#;
        let req: LlmRequest = serde_json::from_str(corpo).expect("corpo storico valido");
        assert_eq!(req.run_timeout_secs, None);
    }

    /// Client NUOVO -> gateway NUOVO: il valore arriva.
    #[test]
    fn il_campo_arriva_quando_e_presente() {
        let corpo = r#"{
            "model": "gpt-4o-mini",
            "messages": [],
            "run_timeout_secs": 240,
            "metadata": {"tenant_id":"t","user_id":"u","request_id":"r","sensitivity_tier":0,"feature":"agent"}
        }"#;
        let req: LlmRequest = serde_json::from_str(corpo).expect("corpo nuovo valido");
        assert_eq!(req.run_timeout_secs, Some(240));
    }

    /// Client NUOVO -> gateway VECCHIO: il campo non deve comparire quando e'
    /// assente, o un deserializzatore piu' rigido a valle lo vedrebbe come
    /// `null` inatteso.
    #[test]
    fn il_campo_assente_non_viene_serializzato() {
        let req = LlmRequest {
            model: "m".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            run_timeout_secs: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "f".into(),
            },
        };
        let json = serde_json::to_string(&req).expect("serializza");
        assert!(
            !json.contains("run_timeout_secs"),
            "campo assente non deve finire sul wire: {json}"
        );
    }
}

/// Configurazione extended thinking (`thinking` di `LLMRequest`). `budget_tokens`
/// opzionale: se assente il provider usa il budget dai settings DB (regola G).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    /// Thinking OBBLIGATORIO per il modello (es. gemini-3): il modello RIFIUTA
    /// `thinkingBudget=0` (HTTP 400) e, senza `thinkingConfig`, applica il suo
    /// thinking DEFAULT ILLIMITATO che divora il tetto di output -> risposta vuota
    /// (`finish_reason=length`). Quando `true`, `resolve_thinking` emette un budget
    /// bounded ESPLICITO anche sui turni con tool (invece di `DisabledForTools`),
    /// e `build_generation_config` alza `maxOutputTokens` del budget cosi' la
    /// risposta ha spazio. Popolato dall'adapter mcp-core dalla policy del catalog
    /// (`agentic_thinking_policy='native'`, regola G): default `false` -> nessuna
    /// regressione per i modelli non-obbligatori. Solo trasporto, non serializzato
    /// verso i provider (usato in `resolve_thinking`).
    #[serde(default)]
    pub mandatory: bool,
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
    /// Citazioni (URL fonti) dei provider di ricerca: Perplexity espone un array
    /// top-level `citations` nella risposta (non standard OpenAI). Retrocompatibile:
    /// `None` per i provider che non le emettono. Regola M: campo strutturato,
    /// mai estratto dal testo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<String>>,
}

impl LlmResponse {
    /// Risposta DEGENERE: HTTP 200 senza alcun output utile (regola M, solo
    /// segnali strutturati, mai parsing di prosa). Vero quando il turno non
    /// produce ne' testo ne' tool-call E il `finish_reason` non e' una chiusura
    /// legittima. Caso tipico: Gemini consuma l'intero budget nel thinking e
    /// ritorna `content=""`, `tool_calls=None`, `finish_reason="length"`
    /// (google.rs `map_finish_reason` MAX_TOKENS -> "length"). Senza questo
    /// predicato il gateway tratterebbe il 200 come successo e il motore non
    /// ripiegherebbe mai su un provider alternativo.
    ///
    /// Condizioni (tutte necessarie):
    /// - `content` vuoto o solo whitespace;
    /// - nessuna tool-call (`None` oppure `Vec` vuoto);
    /// - `finish_reason` NON e' un blocco di safety deliberato (`"content_filter"`),
    ///   l'unico esito con output vuoto che NON va aggirato con un failover.
    ///
    /// NB (regola M): il segnale PRIMARIO e strutturale e' "nessun output utile"
    /// (content vuoto + zero tool-call). NON si esclude `"stop"`: Google
    /// (`map_finish_reason`) collassa a `"stop"` ogni finishReason anomalo non
    /// mappato — `MALFORMED_FUNCTION_CALL`, `OTHER`, `BLOCKLIST`,
    /// `FINISH_REASON_UNSPECIFIED` — e `MALFORMED_FUNCTION_CALL` con output vuoto e'
    /// il caso Gemini PIU' frequente di hollow sul tool-forcing (agent_run.rs:3169).
    /// Un turno senza output e' inservibile qualunque sia il `finish_reason`, e
    /// ripiegare su un altro provider e' sempre preferibile a restituire un 200
    /// vuoto; la sola eccezione e' `content_filter`, dove il vuoto e' una scelta di
    /// safety intenzionale da non aggirare.
    ///
    /// Non e' degenere una risposta con SOLE tool-call (content vuoto ma
    /// `tool_calls` non vuoto): e' il normale comportamento agentico.
    pub fn is_degenerate_completion(&self) -> bool {
        let no_content = self.content.trim().is_empty();
        let no_tool_calls = self
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty());
        // Solo il blocco di safety (`content_filter`) e' una chiusura legittima con
        // output vuoto; `"stop"` NON e' escluso (Google vi collassa anche
        // MALFORMED_FUNCTION_CALL, output vuoto reale che deve ripiegare).
        let safety_block = self.finish_reason == "content_filter";
        if safety_block {
            return false;
        }
        // (1) Storico: nessun output utile (content vuoto E nessuna tool-call).
        if no_content && no_tool_calls {
            return true;
        }
        // (2) CONTRATTO STRUTTURATO (regola M, fix gemini hollow-ma-non-vuoto): il
        // modello e' stato TRONCATO dal limite di token (finish=length: il budget e'
        // stato speso nel thinking) senza una tool-call e con output VISIBILE
        // trascurabile. Il segnale e' l'`usage.output_tokens` STRUTTURATO (per Gemini
        // = candidatesTokenCount, che ESCLUDE i thinking token, google.rs
        // usage_from_metadata): ~0 significa "nessun output reale" = fallito, anche se
        // resta un frammento di content. Cattura il caso che (1) mancava (RC-2: una
        // functionCall vuota/malformata o un frammento non-degenere sfuggiva al gate).
        // Gating STRETTO (finish=length + no tool-call + output <= floor) per non
        // flaggare un troncamento legittimo con output reale.
        let cut_off_by_length = matches!(self.finish_reason.as_str(), "length" | "max_tokens");
        cut_off_by_length && no_tool_calls && self.usage.output_tokens <= HOLLOW_OUTPUT_TOKENS_FLOOR
    }
}

/// Soglia (token di output VISIBILE) sotto la quale un turno troncato da `length`
/// senza tool-call e' considerato hollow (contratto strutturato, regola M). Piccola:
/// distingue un frammento trascurabile da una risposta parziale reale.
const HOLLOW_OUTPUT_TOKENS_FLOOR: u32 = 32;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Costruisce una `LlmResponse` minimale variando i soli campi rilevanti per
    /// il predicato di degenerazione.
    fn resp(content: &str, tool_calls: Option<Vec<LlmToolCall>>, finish_reason: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            tool_calls,
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "m".to_string(),
            provider_used: "p".to_string(),
            latency_ms: 0,
            finish_reason: finish_reason.to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
        }
    }

    fn a_tool_call() -> LlmToolCall {
        LlmToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            },
            thought_signature: None,
        }
    }

    #[test]
    fn empty_content_length_finish_is_degenerate() {
        // Caso Gemini: budget consumato dal thinking, content vuoto,
        // finish_reason="length", nessuna tool-call -> degenere.
        assert!(resp("", None, "length").is_degenerate_completion());
        assert!(resp("   \n\t ", None, "length").is_degenerate_completion());
        assert!(resp("", Some(vec![]), "length").is_degenerate_completion());
    }

    #[test]
    fn only_tool_calls_is_not_degenerate() {
        // Comportamento agentico legittimo: content vuoto ma tool-call presenti.
        let r = resp("", Some(vec![a_tool_call()]), "tool_calls");
        assert!(!r.is_degenerate_completion());
        // Anche con content vuoto e finish_reason non-stop: le tool-call salvano.
        let r2 = resp("", Some(vec![a_tool_call()]), "length");
        assert!(!r2.is_degenerate_completion());
    }

    #[test]
    fn empty_stop_is_degenerate_but_safety_block_is_not() {
        // "stop" con output vuoto e' DEGENERE: Google collassa a "stop" anche
        // MALFORMED_FUNCTION_CALL (il caso Gemini piu' frequente di hollow sul
        // tool-forcing). Il turno non ha output -> deve ripiegare su un altro provider.
        assert!(resp("", None, "stop").is_degenerate_completion());
        // Blocco di safety (content_filter): esito deliberato, NON aggirabile via failover.
        assert!(!resp("", None, "content_filter").is_degenerate_completion());
    }

    #[test]
    fn risposta_reale_troncata_non_e_degenere() {
        // Una risposta REALE troncata da length (output_tokens sostanziosi, > floor)
        // NON e' degenere: e' un output valido tagliato, non un hollow da thinking.
        let mut r = resp("una risposta lunga e utile del modello", None, "length");
        r.usage.output_tokens = 500;
        assert!(!r.is_degenerate_completion());
        // finish "stop" con content: risposta completa, mai degenere (a prescindere
        // dagli output_tokens).
        assert!(!resp("parziale", None, "stop").is_degenerate_completion());
        let mut r3 = resp("breve ma completa", None, "stop");
        r3.usage.output_tokens = 3;
        assert!(!r3.is_degenerate_completion());
    }

    #[test]
    fn frammento_hollow_troncato_da_length_e_degenere() {
        // CONTRATTO STRUTTURATO (regola M, RC-2): un frammento trascurabile con
        // finish=length e output_tokens ~0 (budget speso nel thinking) e' hollow, anche
        // se il content non e' strettamente vuoto. Il segnale e' l'usage STRUTTURATO
        // (candidatesTokenCount, esclude i thinking token), non il parsing del testo.
        let mut r = resp("x", None, "length");
        r.usage.output_tokens = 1;
        assert!(r.is_degenerate_completion());
        // Con una tool-call NON e' degenere (progresso agentico), a prescindere.
        let mut r2 = resp("x", Some(vec![a_tool_call()]), "length");
        r2.usage.output_tokens = 1;
        assert!(!r2.is_degenerate_completion());
        // "max_tokens" (alcuni provider) e' trattato come "length".
        let mut r4 = resp("", None, "max_tokens");
        r4.usage.output_tokens = 0;
        assert!(r4.is_degenerate_completion());
    }
}
