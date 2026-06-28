use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// HTTP client per il Nexus LLM Gateway (porta 4060).
#[derive(Clone)]
pub struct NexusGatewayClient {
    http: reqwest::Client,
    base_url: String,
    service_token: String,
}

/// Messaggio della conversazione inviato al gateway.
///
/// `content` e' un [`Value`] perche' il contratto del server (`MessageContent`
/// untagged in `nexus-gateway::types`) accetta SIA una stringa semplice (turno
/// testuale) SIA una lista di blocchi `{type, ...}` (tool_use/tool_result/image).
/// L'agent graph adapter (executor) deve poter trasportare i blocchi per la
/// continuita' tool_use/tool_result, quindi il campo non puo' essere un `String`
/// rigido. I call site testuali costruiscono `content: json!("...")`.
///
/// CONTINUITA' TOOL MULTI-TURN (regola L, allineato a `LlmMessage` del server in
/// `nexus-gateway::types`): un turno `assistant` che ha chiamato tool porta i
/// `tool_use` in [`GwMessage::tool_calls`] (NON appiattiti in `content`); un turno
/// `tool` (risultato) ha `role="tool"` + [`GwMessage::tool_call_id`] valorizzato.
/// Il server (`to_anthropic_messages`) riconosce la coppia tool_use/tool_result
/// SOLO da questi campi: senza di essi Anthropic risponde HTTP 400 (`tool_use ids
/// without tool_result`). Campi `Option` additivi: omessi (`skip_serializing_if`)
/// sui messaggi testuali, retrocompatibili coi call site esistenti.
#[derive(Serialize, Clone, Debug)]
pub struct GwMessage {
    pub role: String,
    pub content: Value,
    /// Tool-call emesse da un turno `assistant` (continuita' tool_use). Gli id qui
    /// DEVONO combaciare col [`GwMessage::tool_call_id`] del messaggio `tool` che ne
    /// porta il risultato. Omesso quando `None` (turno testuale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<GwToolCall>>,
    /// Id della tool-call a cui un messaggio `role="tool"` (risultato) risponde.
    /// Omesso quando `None` (qualunque ruolo != tool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct GwMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
    pub sensitivity_tier: u8,
    pub feature: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct GwRequest {
    pub model: String,
    pub messages: Vec<GwMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Tool dichiarati al modello. Il contratto del server e' lo schema OpenAI
    /// (`[{type:"function", function:{name, description?, parameters}}]`): chi
    /// passa tool Anthropic-style (`{name, description, input_schema}`) li
    /// converte PRIMA di valorizzare questo campo (vedi adapter LlmGateway).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Vincolo di scelta tool in stile OpenAI (`"auto"` | `"required"` | `"none"`
    /// | `{"type":"function","function":{"name":"X"}}`). DEVE arrivare al gateway
    /// per non neutralizzare il force-action anti-loop (memoria progetto "Gateway
    /// droppava tool_choice"): omesso quando `None` (equivale ad `auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Pin esplicito del provider (bypass routing nel gateway). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider col `model` indicato, senza
    /// `policy.decide` ne' fallback cross-provider. Il chiamante (mcp-core) che ha
    /// gia' deciso provider+modello via routing matrix DB lo valorizza per evitare
    /// un secondo routing divergente (regola G).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: GwMetadata,
}

/// Richiesta di generazione immagine al gateway (`POST /v1/images/generations`).
/// Speculare a [`GwRequest`] ma per il task image-gen: solo un `prompt` testuale.
/// Regola G: il `model` arriva dal chiamante (nessun default hardcoded).
///
/// API client pronta per il wiring: i tool agente che la consumeranno arrivano
/// nella PR successiva (PR6b-2), quindi qui e' ancora senza call site.
#[allow(dead_code)]
#[derive(Serialize, Clone, Debug, Default)]
pub struct GwImageRequest {
    pub model: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Pin esplicito del provider (bypass routing nel gateway). Stessa semantica
    /// di [`GwRequest::pin_provider`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: GwMetadata,
}

/// Una immagine generata, come la riporta il gateway. Almeno uno tra `b64_json`
/// (base64 inline) e `url` (URL temporanea) e' valorizzato; `mime` quando il
/// provider lo dichiara (Vertex `mimeType`). Vedi nota su [`GwImageRequest`].
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwGeneratedImage {
    #[serde(default)]
    pub b64_json: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
}

/// Risposta di `POST /v1/images/generations` del gateway. Speculare a
/// [`GwResponse`] per i campi di tracciamento. Vedi nota su [`GwImageRequest`].
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwImageResponse {
    pub images: Vec<GwGeneratedImage>,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di trascrizione audio al gateway (`POST /v1/audio/transcriptions`).
/// Speculare a [`GwImageRequest`] ma per il task audio-in: l'audio arriva come
/// base64 inline + il modello. Regola G: il `model` arriva dal chiamante.
///
/// API client pronta per il wiring: il tool agente che la consuma vive in
/// `nexus-agent-tools` (audio_tools), che NON puo' dipendere da questo crate
/// (ciclo) e re-implementa il trasporto wire-compatibile; qui e' senza call site.
#[allow(dead_code)]
#[derive(Serialize, Clone, Debug, Default)]
pub struct GwTranscribeRequest {
    pub model: String,
    pub audio_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Pin esplicito del provider (bypass routing nel gateway). Stessa semantica
    /// di [`GwImageRequest::pin_provider`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: GwMetadata,
}

/// Risposta di `POST /v1/audio/transcriptions` del gateway. Speculare a
/// [`GwImageResponse`]. Vedi nota su [`GwTranscribeRequest`].
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwTranscribeResponse {
    pub text: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token serviti da prompt cache (Anthropic `cache_read_input_tokens`).
    /// `None` se il provider non li riporta.
    #[serde(default)]
    pub cache_read_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce). Vedi sopra.
    #[serde(default)]
    pub cache_creation_tokens: Option<u32>,
}

/// Funzione chiamata in una tool-call (forma OpenAI Chat Completions): `arguments`
/// e' una STRINGA JSON. `Serialize`+`Deserialize`: la stessa forma serve sia in
/// RISPOSTA (tool_calls emesse dal modello) sia in RICHIESTA (tool_calls di un
/// turno assistant precedente re-inviato per la continuita' multi-turn).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool-call emessa dal modello, come la riporta il gateway e come la rispedisce
/// il chiamante nei turni successivi (`LlmToolCall` del contratto: `{id, type,
/// function:{name, arguments}}`). Bidirezionale (`Serialize`+`Deserialize`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwToolCall {
    pub id: String,
    /// Discriminante OpenAI (`"function"`). In risposta e' deserializzato per
    /// tolleranza; in richiesta DEVE valere `"function"`: lo costruiamo
    /// esplicitamente nell'adapter. `default` -> stringa vuota in deserializzazione
    /// se assente (tollerante).
    #[serde(rename = "type", default)]
    pub kind: String,
    pub function: GwToolFunctionCall,
}

/// Informazioni sul re-routing automatico per motivi di privacy.
/// Presente nella risposta quando la richiesta è stata instradata su provider locale
/// al posto del provider cloud originalmente richiesto.
#[derive(Deserialize, Debug, Clone)]
pub struct GwPrivacyRerouted {
    pub provider: String,
    pub blocked_tier: u8,
    pub reason: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GwResponse {
    pub content: String,
    /// Tool-call emesse dal modello (vuoto/`None` per un turno testuale). DEVE
    /// essere popolato dal gateway quando il modello chiama un tool, altrimenti il
    /// force-action e' inutile (memoria progetto "google tool monco"): l'adapter
    /// le mappa in `ToolUse` per il `Message::Ai`.
    #[serde(default)]
    pub tool_calls: Option<Vec<GwToolCall>>,
    pub usage: GwUsage,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
    pub finish_reason: String,
    /// Presente se il gateway ha re-instradato automaticamente su provider locale per privacy
    #[serde(default)]
    pub privacy_rerouted: Option<GwPrivacyRerouted>,
    /// Testo del ragionamento (extended thinking) quando il provider lo emette.
    /// Parte del contratto wire (deserializzazione tollerante). La porta
    /// [`nexus_agent_graph::runtime::ports::LlmResponse`] non espone ancora il
    /// reasoning: il campo e' qui pronto per il wiring futuro, non ancora letto.
    #[serde(default)]
    #[allow(dead_code)]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` da ri-passare nei turni con tool
    /// (Anthropic). `None` per gli altri provider. Vedi nota su `reasoning`.
    #[serde(default)]
    #[allow(dead_code)]
    pub thinking_signature: Option<String>,
}

impl NexusGatewayClient {
    pub fn new(base_url: String, service_token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            base_url,
            service_token,
        }
    }

    /// Costruisce il client risolvendo la porta del gateway dal DB (regola G:
    /// niente porta hardcoded; il solo segreto del token resta in env, come in
    /// `main.rs`). PUNTO UNICO (regola L) del cablaggio gateway riusato da
    /// `build_native_deps` (run principale) e dall'orchestrazione sub-agente
    /// nativa (`agent_tools::subagent_native`): prima la sequenza
    /// `resolve_port -> format url -> NexusGatewayClient::new` era duplicata.
    pub async fn from_db(db: &sqlx::PgPool) -> Self {
        let gw_port = nexus_auth::resolve_port(db, "nexus_gateway_port").await;
        let gw_url = format!("http://127.0.0.1:{gw_port}");
        let gw_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
            .unwrap_or_else(|_| "dev-internal-token".to_string());
        Self::new(gw_url, gw_token)
    }

    pub async fn complete(&self, req: GwRequest) -> Result<GwResponse> {
        let resp = self
            .http
            .post(format!("{}/v1/complete", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .json(&req)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway HTTP error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nexus Gateway {status}: {body}");
        }

        resp.json::<GwResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway response parse: {e}"))
    }

    /// Genera immagini via gateway (`POST /v1/images/generations`). Speculare a
    /// [`Self::complete`]: stesso Bearer service token, stesso status-check con
    /// propagazione del body d'errore al caller (regola H: errore esplicito se il
    /// provider non genera immagini -> il gateway ritorna 500 col motivo).
    ///
    /// Pronta per il wiring: i tool agente che la consumeranno arrivano nella PR
    /// successiva (PR6b-2), quindi e' ancora senza call site.
    #[allow(dead_code)]
    pub async fn generate_image(&self, req: GwImageRequest) -> Result<GwImageResponse> {
        let resp = self
            .http
            .post(format!("{}/v1/images/generations", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .json(&req)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway HTTP error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nexus Gateway {status}: {body}");
        }

        resp.json::<GwImageResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway image response parse: {e}"))
    }

    /// Trascrive audio via gateway (`POST /v1/audio/transcriptions`). Speculare a
    /// [`Self::generate_image`]: stesso Bearer service token, stesso status-check
    /// con propagazione del body d'errore al caller (regola H: errore esplicito se
    /// il provider non trascrive -> il gateway ritorna 500 col motivo).
    ///
    /// Pronta per il wiring: vedi nota su [`GwTranscribeRequest`].
    #[allow(dead_code)]
    pub async fn transcribe_audio(
        &self,
        req: GwTranscribeRequest,
    ) -> Result<GwTranscribeResponse> {
        let resp = self
            .http
            .post(format!("{}/v1/audio/transcriptions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .json(&req)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway HTTP error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nexus Gateway {status}: {body}");
        }

        resp.json::<GwTranscribeResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway transcribe response parse: {e}"))
    }

    pub async fn is_healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Autodiscovery dei modelli live di un provider via gateway
    /// (`GET /v1/models/{provider}`). Il gateway e' la via UNICA per la
    /// discovery: incapsula l'auth di ogni provider (incluso Vertex con
    /// Service Account), cosi' il worker catalog non deve replicare le
    /// chiamate dirette ne' delegare al brain per Google (regola L).
    /// Ritorna la lista dei model id esposti dal provider.
    pub async fn list_models(&self, provider: &str) -> Result<Vec<String>> {
        let resp = self
            .http
            .get(format!("{}/v1/models/{provider}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway HTTP error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nexus Gateway {status}: {body}");
        }

        resp.json::<GwModelsResponse>()
            .await
            .map(|r| r.models)
            .map_err(|e| anyhow::anyhow!("Nexus Gateway models parse: {e}"))
    }
}

/// Risposta di `GET /v1/models/{provider}` del gateway.
#[derive(Deserialize, Debug)]
struct GwModelsResponse {
    #[allow(dead_code)]
    provider: String,
    models: Vec<String>,
}

/// Mappa intent + behavior_mode all'alias definito in config/model-aliases.yaml.
pub fn intent_to_alias(intent: &str, behavior_mode: &str, forced_model: Option<&str>) -> String {
    if let Some(m) = forced_model {
        return m.to_string();
    }
    match (intent, behavior_mode) {
        ("architecture" | "design", _) => "reasoning-heavy",
        ("fix" | "refactor", _) => "coder-large",
        ("test" | "docs", "approfondita") => "coder-large",
        ("test" | "docs", _) => "coder-small",
        (_, "approfondita" | "dinamico") => "coder-large",
        _ => "coder-small",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_models_response_per_provider() {
        // Forma di GET /v1/models/{provider}: {"provider":"...","models":[...]}.
        let raw = r#"{"provider":"openai","models":["gpt-4o","gpt-4o-mini","o3"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse models response");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.models, vec!["gpt-4o", "gpt-4o-mini", "o3"]);
    }

    #[test]
    fn parse_models_response_lista_vuota() {
        // Provider configurato ma senza modelli: lista vuota valida (il worker
        // tratta poi la lista vuota come skip-per-safety).
        let raw = r#"{"provider":"deepseek","models":[]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse empty models");
        assert_eq!(parsed.provider, "deepseek");
        assert!(parsed.models.is_empty());
    }

    #[test]
    fn parse_image_response_dal_gateway() {
        // Forma di POST /v1/images/generations: images[] + tracciamento.
        let raw = r#"{
            "images": [
                {"b64_json": "AAAA", "mime": "image/png"},
                {"url": "https://example.com/x.png"}
            ],
            "model_used": "gpt-image-1",
            "provider_used": "openai",
            "latency_ms": 1234
        }"#;
        let parsed: GwImageResponse = serde_json::from_str(raw).expect("parse image response");
        assert_eq!(parsed.provider_used, "openai");
        assert_eq!(parsed.model_used, "gpt-image-1");
        assert_eq!(parsed.latency_ms, 1234);
        assert_eq!(parsed.images.len(), 2);
        assert_eq!(parsed.images[0].b64_json.as_deref(), Some("AAAA"));
        assert_eq!(parsed.images[0].mime.as_deref(), Some("image/png"));
        assert_eq!(parsed.images[1].url.as_deref(), Some("https://example.com/x.png"));
    }

    #[test]
    fn image_request_serializza_campi_e_omette_opzionali() {
        let req = GwImageRequest {
            model: "gpt-image-1".into(),
            prompt: "un gatto".into(),
            n: Some(1),
            size: None,
            pin_provider: Some("openai".into()),
            metadata: GwMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "image".into(),
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "gpt-image-1");
        assert_eq!(json["prompt"], "un gatto");
        assert_eq!(json["n"], 1);
        // size None -> campo omesso.
        assert!(json.get("size").is_none());
        assert_eq!(json["pin_provider"], "openai");
    }

    #[test]
    fn parse_transcribe_response_dal_gateway() {
        // Forma di POST /v1/audio/transcriptions: text + tracciamento.
        let raw = r#"{
            "text": "ciao mondo",
            "model_used": "whisper-1",
            "provider_used": "openai",
            "latency_ms": 850
        }"#;
        let parsed: GwTranscribeResponse =
            serde_json::from_str(raw).expect("parse transcribe response");
        assert_eq!(parsed.text, "ciao mondo");
        assert_eq!(parsed.model_used, "whisper-1");
        assert_eq!(parsed.provider_used, "openai");
        assert_eq!(parsed.latency_ms, 850);
    }

    #[test]
    fn transcribe_request_serializza_campi_e_omette_opzionali() {
        let req = GwTranscribeRequest {
            model: "whisper-1".into(),
            audio_base64: "AAAA".into(),
            mime: Some("audio/mpeg".into()),
            language: None,
            pin_provider: Some("openai".into()),
            metadata: GwMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "audio".into(),
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "whisper-1");
        assert_eq!(json["audio_base64"], "AAAA");
        assert_eq!(json["mime"], "audio/mpeg");
        // language None -> campo omesso.
        assert!(json.get("language").is_none());
        assert_eq!(json["pin_provider"], "openai");
    }

    #[test]
    fn parse_models_response_google_via_gateway() {
        // Google passa per il gateway come tutti gli altri (auth Vertex inclusa
        // nel gateway): nessuna forma speciale lato mcp-core.
        let raw = r#"{"provider":"google","models":["gemini-2.5-pro","gemini-2.5-flash"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse google models");
        assert_eq!(parsed.models.len(), 2);
        assert!(parsed.models.contains(&"gemini-2.5-pro".to_string()));
    }
}
