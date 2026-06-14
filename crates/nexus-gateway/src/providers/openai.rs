//! Provider OpenAI.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts`. Delega tutto al
//! client condiviso [`OpenAiCompatClient`] (composizione, regola L). Le capacita'
//! (tier, context window) replicano il TS.

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint OpenAI di default. La `base_url` resta un parametro del costruttore
/// (override per gateway compatibili); questo valore e' solo il default quando
/// il chiamante non ne passa una.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    client: OpenAiCompatClient,
}

impl OpenAiProvider {
    /// Costruisce il provider. `base_url` opzionale (default OpenAI ufficiale).
    /// La `api_key` e' iniettata dal chiamante (regola G/F: niente segreti nel
    /// codice).
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "openai"),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
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

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(Client::new(), "sk-test", None)
    }

    #[test]
    fn capacita_dichiarate() {
        let p = provider();
        assert_eq!(p.name(), "openai");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 128_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }
}
