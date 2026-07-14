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
#[derive(Serialize, Clone, Debug, Default)]
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
    /// Reasoning (`reasoning_content`) di un turno `assistant` precedente generato
    /// in thinking mode (DeepSeek), da RI-PASSARE al gateway: il server lo inoltra
    /// SOLO al dialetto DeepSeek (vincolo HTTP 400 "The reasoning_content in the
    /// thinking mode must be passed back to the API"). Allineato a
    /// `LlmMessage::reasoning` del server (`nexus-gateway::types`). Omesso quando
    /// `None` (turno senza reasoning / altri ruoli o provider).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` (Anthropic) di un turno `assistant`
    /// precedente, da RI-PASSARE al gateway: il server la inoltra SOLO ad Anthropic
    /// (vincolo HTTP 400 sui turni con tool). Allineata a
    /// `LlmMessage::thinking_signature` del server (`nexus-gateway::types`). Omessa
    /// quando `None` (turno senza thinking / altri ruoli o provider).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
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
pub struct GwThinkingConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    /// Thinking OBBLIGATORIO per il modello (gemini-3, policy DB
    /// `agentic_thinking_policy='native'`): quando true il gateway emette un thinking
    /// budget bounded (Enabled) invece di DisabledForTools (che gemini-3 rifiuta ->
    /// thinking illimitato -> risposta vuota). Popolato in `complete()` dal catalog
    /// (self.db, regola G). Serializzato verso il gateway (default false).
    #[serde(default)]
    pub mandatory: bool,
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
    /// Vincolo sul formato della risposta nel contratto gateway completo
    /// (`response_format` OpenAI-style). Il gateway lo inoltra solo ai provider
    /// che lo supportano o lo traducono nel proprio dialetto.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    /// Configurazione thinking esplicita del chiamante. `None` conserva il
    /// comportamento storico DB-driven/provider-specifico del gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<GwThinkingConfig>,
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
    /// Firma opaca di reasoning (`thoughtSignature`) di Gemini 3, PER-CALL.
    /// Combacia col campo omonimo di `LlmToolCall` (contratto gateway): il
    /// gateway la emette in RISPOSTA su ogni tool-call e la esige di ritorno in
    /// RICHIESTA sulla stessa `functionCall`, pena HTTP 400 INVALID_ARGUMENT.
    /// Additivo/tollerante (`default` + skip se `None`): assente per gli altri
    /// provider e retrocompatibile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
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
    /// (Anthropic). `None` per gli altri provider. Ora LETTA dall'adapter
    /// (`map_gw_response`) e trasportata nel round-trip via
    /// `Message::Ai::thinking_signature`.
    #[serde(default)]
    pub thinking_signature: Option<String>,
    /// Citazioni (URL fonti) dei provider di ricerca (Perplexity `citations`).
    /// Parte del contratto wire (default tollerante); propagata nel `metadata` del
    /// messaggio assistant per il pannello "Fonti consultate". `None` per gli altri
    /// provider (regola M: campo strutturato, mai estratto dal testo).
    #[serde(default)]
    pub citations: Option<Vec<String>>,
}

/// Errore HTTP del Nexus Gateway coi segnali STRUTTURATI del body JSON estratti
/// al punto di costruzione (regola M): `code` (`PROVIDER_ERROR`,
/// `POLICY_TIER_EXCLUDED`, `TIER_BLOCKED`, ...) e `details` (fallimenti
/// per-provider con classe, tier rilevato, provider ammessi — vedi
/// `PipelineError` in `nexus-gateway::server::routes`). I decisori fanno
/// `downcast_ref::<GatewayHttpError>()`, mai match sul testo. `Display` e'
/// IDENTICO al vecchio `bail!("Nexus Gateway {status}: {body}")` cosi' i
/// consumatori legacy della stringa non cambiano comportamento.
#[derive(Debug)]
pub struct GatewayHttpError {
    pub status: u16,
    /// Riga di status per il display (es. "500 Internal Server Error").
    status_text: String,
    /// Codice d'errore strutturato dal body JSON del gateway, se presente.
    pub code: Option<String>,
    /// Blocco `details` del body (failures classificate, tier, ammessi).
    pub details: Option<Value>,
    /// Body grezzo: solo display/log, mai per decidere.
    pub body: String,
}

impl GatewayHttpError {
    pub fn from_response(status: reqwest::StatusCode, body: String) -> Self {
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let code = parsed
            .as_ref()
            .and_then(|v| v.get("code"))
            .and_then(|c| c.as_str())
            .map(str::to_string);
        let details = parsed.as_ref().and_then(|v| v.get("details")).cloned();
        Self {
            status: status.as_u16(),
            status_text: status.to_string(),
            code,
            details,
            body,
        }
    }
}

impl std::fmt::Display for GatewayHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nexus Gateway {}: {}", self.status_text, self.body)
    }
}

impl std::error::Error for GatewayHttpError {}

/// Default mig 0421 (allineato a `nexus_gateway::http_timeouts`).
const DEFAULT_COMPLETE_TIMEOUT_SECS: u64 = 120;
const DEFAULT_RETRY_MAX_ATTEMPTS: u64 = 3;
const DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S: i64 = 45;
/// Margine sopra il budget gateway (retry + cooldown) per non chiudere la
/// connessione mcp-core->gateway prima che il gateway risponda.
const CLIENT_TIMEOUT_MARGIN_SECS: u64 = 30;

fn parse_positive_u64(raw: Option<String>, default: u64) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

fn parse_positive_i64(raw: Option<String>, default: i64) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Budget HTTP mcp-core -> gateway per `/v1/complete`: copre timeout provider
/// (complete) x retry + attesa cooldown breve + margine (regola G).
async fn resolve_client_timeout_secs(db: &sqlx::PgPool) -> u64 {
    let complete = parse_positive_u64(
        nexus_auth::get_setting(db, "gateway.complete_timeout_seconds").await,
        DEFAULT_COMPLETE_TIMEOUT_SECS,
    );
    let max_attempts = parse_positive_u64(
        nexus_auth::get_setting(db, "gateway.retry.max_attempts").await,
        DEFAULT_RETRY_MAX_ATTEMPTS,
    );
    let cooldown_cap = parse_positive_i64(
        nexus_auth::get_setting(db, "gateway.retry.wait_short_cooldown_cap_s").await,
        DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S,
    ) as u64;
    complete
        .saturating_mul(max_attempts)
        .saturating_add(cooldown_cap)
        .saturating_add(CLIENT_TIMEOUT_MARGIN_SECS)
}

impl NexusGatewayClient {
    pub fn new(base_url: String, service_token: String) -> Self {
        let timeout_secs = DEFAULT_COMPLETE_TIMEOUT_SECS
            .saturating_mul(DEFAULT_RETRY_MAX_ATTEMPTS)
            .saturating_add(DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S as u64)
            .saturating_add(CLIENT_TIMEOUT_MARGIN_SECS);
        Self::with_timeout(base_url, service_token, timeout_secs)
    }

    fn with_timeout(base_url: String, service_token: String, timeout_secs: u64) -> Self {
        Self {
            // Resilienza connessioni morte post-sleep (regola H): niente riuso di
            // socket idle dal pool + keepalive. Il default keep-alive faceva fallire
            // le chiamate mcp-core -> gateway con "error sending request" dopo che la
            // macchina si risvegliava dallo sleep (connessioni TCP morte riusate).
            // Vedi il gemello in nexus-gateway bootstrap (client gateway -> provider).
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .pool_max_idle_per_host(0)
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
        let timeout_secs = resolve_client_timeout_secs(db).await;
        Self::with_timeout(gw_url, gw_token, timeout_secs)
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
            // Errore TIPIZZATO (regola M): il punto di costruzione — che conosce
            // il contratto del body del gateway — estrae `code` e `details`; il
            // punto di decisione (adapter agent-graph) fa downcast, non parsing
            // della stringa. Display identico al vecchio bail! per i log.
            return Err(GatewayHttpError::from_response(status, body).into());
        }

        resp.json::<GwResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway response parse: {e}"))
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
    /// Ritorna id + finestra di contesto dichiarata dal provider (dal campo
    /// additivo `models_meta`; gateway senza il campo -> finestre `None`,
    /// retro-compatibile).
    pub async fn list_models(&self, provider: &str) -> Result<Vec<GwModelMeta>> {
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
            .map(GwModelsResponse::into_metas)
            .map_err(|e| anyhow::anyhow!("Nexus Gateway models parse: {e}"))
    }
}

/// Metadati di un modello dal gateway (`models_meta` di
/// `GET /v1/models/{provider}`): id + finestra dichiarata dal provider.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GwModelMeta {
    pub id: String,
    /// Finestra di contesto in token dichiarata dal provider; `None` se l'API
    /// del provider non la espone (il catalogo scrive 0 = ignota, regola H).
    #[serde(default)]
    pub context_window: Option<i64>,
}

/// Risposta di `GET /v1/models/{provider}` del gateway.
#[derive(Deserialize, Debug)]
struct GwModelsResponse {
    #[allow(dead_code)]
    provider: String,
    models: Vec<String>,
    /// Campo additivo del gateway aggiornato; assente su gateway vecchio.
    #[serde(default)]
    models_meta: Vec<GwModelMeta>,
}

impl GwModelsResponse {
    /// Proietta la risposta in metadati: usa `models_meta` quando presente,
    /// altrimenti degrada agli id di `models` con finestra ignota (`None`).
    fn into_metas(self) -> Vec<GwModelMeta> {
        if !self.models_meta.is_empty() {
            return self.models_meta;
        }
        self.models
            .into_iter()
            .map(|id| GwModelMeta {
                id,
                context_window: None,
            })
            .collect()
    }
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
        // Gateway VECCHIO senza models_meta: degrada agli id con finestra None.
        let raw = r#"{"provider":"openai","models":["gpt-4o","gpt-4o-mini","o3"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse models response");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.models, vec!["gpt-4o", "gpt-4o-mini", "o3"]);
        let metas = parsed.into_metas();
        assert_eq!(metas.len(), 3);
        assert!(metas.iter().all(|m| m.context_window.is_none()));
    }

    #[test]
    fn parse_models_meta_con_finestra_dichiarata() {
        // Gateway aggiornato: models_meta porta la finestra dichiarata dal
        // provider (Mistral max_context_length); id senza finestra -> None.
        let raw = r#"{"provider":"mistral",
            "models":["mistral-medium-3","mistral-ocr-latest"],
            "models_meta":[
                {"id":"mistral-medium-3","context_window":131072},
                {"id":"mistral-ocr-latest"}
            ]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse meta");
        let metas = parsed.into_metas();
        assert_eq!(metas.len(), 2);
        assert_eq!(
            metas[0],
            GwModelMeta {
                id: "mistral-medium-3".into(),
                context_window: Some(131072)
            }
        );
        assert_eq!(metas[1].context_window, None);
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
    fn parse_models_response_google_via_gateway() {
        // Google passa per il gateway come tutti gli altri (auth Vertex inclusa
        // nel gateway): nessuna forma speciale lato mcp-core.
        let raw = r#"{"provider":"google","models":["gemini-2.5-pro","gemini-2.5-flash"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse google models");
        assert_eq!(parsed.models.len(), 2);
        assert!(parsed.models.contains(&"gemini-2.5-pro".to_string()));
    }

    #[test]
    fn client_timeout_budget_default() {
        assert_eq!(
            DEFAULT_COMPLETE_TIMEOUT_SECS
                .saturating_mul(DEFAULT_RETRY_MAX_ATTEMPTS)
                .saturating_add(DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S as u64)
                .saturating_add(CLIENT_TIMEOUT_MARGIN_SECS),
            435
        );
    }

    #[test]
    fn parse_positive_u64_reject_zero() {
        assert_eq!(parse_positive_u64(Some("0".into()), 120), 120);
    }
}
