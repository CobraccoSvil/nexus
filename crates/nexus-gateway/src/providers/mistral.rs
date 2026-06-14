//! Provider Mistral.
//!
//! Porting di `packages/llm-gateway/src/providers/mistral.ts`: thin wrapper
//! OpenAI-compatibile con `base_url` Mistral. Il `provider_used` riportato dal
//! client e' "mistral" (passato al costruttore del client condiviso), quindi
//! non serve rimappare la risposta come fa il TS.

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint OpenAI-compatibile di Mistral (default; override via costruttore).
const DEFAULT_BASE_URL: &str = "https://api.mistral.ai/v1";

pub struct MistralProvider {
    client: OpenAiCompatClient,
}

impl MistralProvider {
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "mistral"),
        }
    }
}

#[async_trait]
impl LlmProvider for MistralProvider {
    fn name(&self) -> &str {
        "mistral"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        128_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        self.client.complete(req).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        self.client.stream(req).await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacita_dichiarate() {
        let p = MistralProvider::new(Client::new(), "key", None);
        assert_eq!(p.name(), "mistral");
        assert!(p.supports_tools());
        assert_eq!(p.max_context_tokens(), 128_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }
}
