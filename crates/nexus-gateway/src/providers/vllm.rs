//! Provider vLLM (endpoint OpenAI-compatibile self-hosted).
//!
//! Porting di `packages/llm-gateway/src/providers/vllm-local.ts`. Pronto dalla
//! Fase 0, attivato in Fase 7. A differenza dei provider cloud ammette anche il
//! tier 3 (dato che gira on-premise) e ha context window e api_key configurabili
//! (alcuni deployment vLLM non richiedono chiave: si passa un placeholder).

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

/// vLLM gira on-premise: ammette tutti i tier, incluso 3 (massima riservatezza).
const TIERS: &[SensitivityTier] = &[0, 1, 2, 3];

/// Context window di default quando il deployment non ne dichiara uno.
const DEFAULT_MAX_CONTEXT: u32 = 32_768;

/// Placeholder usato quando il deployment vLLM non richiede autenticazione.
const NO_KEY_PLACEHOLDER: &str = "no-key";

pub struct VllmProvider {
    client: OpenAiCompatClient,
    max_context_tokens: u32,
}

impl VllmProvider {
    /// `base_url` e' obbligatoria (non c'e' un default cloud). `api_key` e
    /// `max_context_tokens` sono opzionali.
    pub fn new(
        http: Client,
        base_url: impl Into<String>,
        api_key: Option<String>,
        max_context_tokens: Option<u32>,
    ) -> Self {
        let api_key = api_key.unwrap_or_else(|| NO_KEY_PLACEHOLDER.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "vllm"),
            max_context_tokens: max_context_tokens.unwrap_or(DEFAULT_MAX_CONTEXT),
        }
    }
}

#[async_trait]
impl LlmProvider for VllmProvider {
    fn name(&self) -> &str {
        "vllm"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        self.max_context_tokens
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
    fn ammette_tier_3_e_context_default() {
        let p = VllmProvider::new(Client::new(), "http://localhost:8000/v1", None, None);
        assert_eq!(p.name(), "vllm");
        assert_eq!(p.tier_compatibility(), &[0, 1, 2, 3]);
        assert_eq!(p.max_context_tokens(), 32_768);
    }

    #[test]
    fn context_override() {
        let p = VllmProvider::new(
            Client::new(),
            "http://localhost:8000/v1",
            Some("k".to_string()),
            Some(8_192),
        );
        assert_eq!(p.max_context_tokens(), 8_192);
    }
}
