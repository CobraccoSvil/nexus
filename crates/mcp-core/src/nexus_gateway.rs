use anyhow::Result;
use serde::{Deserialize, Serialize};

/// HTTP client per il Nexus LLM Gateway (porta 4060).
#[derive(Clone)]
pub struct NexusGatewayClient {
    http: reqwest::Client,
    base_url: String,
    service_token: String,
}

#[derive(Serialize)]
pub struct GwMessage {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct GwMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
    pub sensitivity_tier: u8,
    pub feature: String,
}

#[derive(Serialize)]
pub struct GwRequest {
    pub model: String,
    pub messages: Vec<GwMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<serde_json::Value>,
    pub metadata: GwMetadata,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GwUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
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
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    pub usage: GwUsage,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
    pub finish_reason: String,
    /// Presente se il gateway ha re-instradato automaticamente su provider locale per privacy
    #[serde(default)]
    pub privacy_rerouted: Option<GwPrivacyRerouted>,
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

    pub async fn is_healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
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
