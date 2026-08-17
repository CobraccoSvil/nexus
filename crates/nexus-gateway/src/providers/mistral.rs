//! Provider Mistral.
//!
//! Porting di `packages/llm-gateway/src/providers/mistral.ts` + parita' con
//! `brain/providers/mistral_provider.py`: thin wrapper OpenAI-compatibile con
//! `base_url` Mistral. Il `provider_used` riportato dal client e' "mistral"
//! (passato al costruttore del client condiviso), quindi non serve rimappare la
//! risposta come fa il TS.
//!
//! REASONING: Mistral NON riceve alcun parametro reasoning. I `magistral`
//! restituiscono HTTP 400 se ricevono `reasoning_effort` (storia repo, mig 0381),
//! e il provider Python non invia nulla di reasoning. Per questo Mistral delega a
//! `OpenAiCompatClient::complete`/`stream` standard (dialetto reasoning `None`):
//! niente `reasoning_effort`, niente `extra_body.thinking`. Eventuale reasoning
//! inline nel content viene lasciato passthrough senza rompere il flusso.

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
use crate::types::{LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier};

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
            // CACHE: Mistral non riusa il prefisso se la richiesta non porta un
            // `prompt_cache_key`. Non e' una preferenza di tuning: senza quella
            // chiave il riuso non avviene MAI, e il prompt viene ricalcolato per
            // intero a ogni chiamata. Vedi [`PromptCacheKeying`] per la misura.
            client: OpenAiCompatClient::new(http, base_url, api_key, "mistral")
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey),
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

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    /// Il dialetto Mistral dichiara la finestra (`max_context_length` in
    /// `data[]`): la propaga cosi' il catalog sync scrive il valore REALE
    /// invece di lasciare la finestra ignota (regola G/H).
    async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        self.client.list_models_meta().await
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

    /// Mistral non riusa il prefisso se la richiesta non porta la chiave:
    /// misurato sul provider reale il 29/07/2026 (stesso prefisso di 11.918
    /// token ripetuto tre volte, `cached_tokens` fermo a 0 senza chiave e a
    /// 11.904 con). Se questa dichiarazione torna al default, il gateway smette
    /// di cacheare in silenzio — nessun errore, solo il conto che raddoppia.
    #[test]
    fn dichiara_di_avere_bisogno_della_chiave_di_cache() {
        let p = MistralProvider::new(Client::new(), "key", None);
        assert_eq!(p.client.cache_keying(), PromptCacheKeying::RequiresKey);
    }

    /// Mistral NON ha deprecato `max_tokens`: il tetto resta sul campo
    /// standard del dialetto condiviso. E' il verso opposto del test gemello
    /// in openai.rs (`anche_un_modello_chat_porta_max_completion_tokens`):
    /// insieme fissano che la dichiarazione e' PER FORNITORE, non un default
    /// del client condiviso — `max_completion_tokens` verso chi non lo
    /// documenta sarebbe un campo sconosciuto.
    ///
    /// Attraversa `corpo_della_richiesta` reale col client che il costruttore
    /// compone (regola O). MUTAZIONE: accendere il default nel costruttore di
    /// `OpenAiCompatClient`, o aggiungere `.with_tetto_su_completion()` al
    /// costruttore mistral -> il body porta `max_completion_tokens`: rosso.
    #[tokio::test]
    async fn il_tetto_di_output_resta_su_max_tokens() {
        use crate::providers::openai_compat::ResolvedReasoning;
        use crate::types::{LlmMessage, MessageContent, RequestMetadata};

        let p = MistralProvider::new(Client::new(), "key", None);
        let req = LlmRequest {
            model: "mistral-small-latest".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error: None,
            }],
            temperature: Some(0.5),
            max_tokens: Some(64),
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
            run_timeout_secs: None,
            service_tier: None,
            seed: None,
            stop: None,
            user: None,
            parallel_tool_calls: None,
            deferrable: false,
        };
        let corpo = serde_json::to_value(
            p.client
                .corpo_della_richiesta(&req, false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        assert_eq!(corpo["max_tokens"], 64);
        assert!(corpo.get("max_completion_tokens").is_none());
    }
}
