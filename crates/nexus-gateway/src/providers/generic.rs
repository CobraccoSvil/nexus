//! Provider OpenAI-compatibile GENERICO, costruito da descrittore (registry).
//!
//! Gemello parametrico di [`crate::providers::mistral::MistralProvider`]: stesso
//! thin wrapper su [`OpenAiCompatClient`] (regola L, composizione), ma `name`,
//! `tier`, `max_context_tokens` e `supports_tools` arrivano dal registry provider
//! (`nexus_provider_registry`) invece di essere hardcoded. Cosi' un nuovo provider
//! OpenAI-compatibile SENZA quirk (Perplexity, OpenRouter, Groq) si aggiunge con
//! una riga nel registry + righe catalog, ZERO nuovo codice (regola G).
//!
//! I provider con quirk (OpenAI o-series, DeepSeek XML/thinking, Anthropic cache,
//! Google Vertex) restano nei loro adapter dedicati: la factory del bootstrap li
//! seleziona per nome; questo generico copre solo il caso OpenAI-compat puro.
//!
//! REASONING: come Mistral/vLLM delega a `OpenAiCompatClient` col dialetto `None`
//! (nessun `reasoning_effort`/`extra_body.thinking`). Il client mappa comunque un
//! eventuale `reasoning_content` emesso dal modello.

use std::borrow::Cow;

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
use crate::types::{LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier};

/// Come questo endpoint del registry vuole che si dichiari il riuso del
/// prefisso (vedi [`PromptCacheKeying`]).
///
/// Qui il provider non e' noto a compile time — e' un descrittore di riga — e la
/// sola cosa che lo identifica e' il nome, quindi e' da li' che il dialetto va
/// letto. Resta un punto solo: chi aggiunge un instradatore lo aggiunge qui e
/// nessun altro file cambia.
///
/// Nota: `nexus_provider_capabilities` ha due colonne che sembrerebbero il posto
/// giusto (`supports_prompt_cache`, `prompt_cache_dialect`), ma sono FOSSILI —
/// non compaiono in nessun file di codice, solo nelle migrazioni che le hanno
/// create, e i valori sono ormai falsi (deepseek vi risulta senza cache mentre
/// ne serve il 63% misurato). Leggerle oggi significherebbe spegnere la cache
/// dove funziona.
fn cache_keying_per_endpoint(name: &str) -> PromptCacheKeying {
    match name {
        // Smista verso fornitori terzi: senza `session_id` i turni successivi
        // possono atterrare su un endpoint che il prefisso non ce l'ha.
        "openrouter" => PromptCacheKeying::RequiresSessionId,
        // groq, perplexity e gli altri: nessun campo documentato, si arrangiano.
        _ => PromptCacheKeying::ProviderManaged,
    }
}

/// Provider OpenAI-compatibile con capacita' dichiarate dal registry.
pub struct GenericOpenAiProvider {
    client: OpenAiCompatClient,
    name: String,
    tiers: Vec<SensitivityTier>,
    max_context_tokens: u32,
    supports_tools: bool,
}

impl GenericOpenAiProvider {
    /// Costruisce il provider dai campi del descrittore registry. `base_url` e'
    /// gia' risolto dal loader (setting `<name>_base_url` -> default del registry).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        http: Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        name: impl Into<String>,
        tiers: Vec<SensitivityTier>,
        max_context_tokens: u32,
        supports_tools: bool,
    ) -> Self {
        let name = name.into();
        let client = OpenAiCompatClient::new(http, base_url, api_key, name.as_str())
            .with_prompt_cache_keying(cache_keying_per_endpoint(&name));
        Self {
            client,
            name,
            tiers,
            max_context_tokens,
            supports_tools,
        }
    }

    /// Garanzia difensiva (regola H): se il provider dichiara `supports_tools=false`
    /// (es. Perplexity sonar, che rifiuta le tool definitions con HTTP 400), rimuove
    /// `tools`/`tool_choice` dalla richiesta PRIMA dell'invio. La garanzia PRIMARIA
    /// e' il layer di selezione (il selettore agentico esclude i modelli
    /// `supports_tool_use=false`), ma un pin/override utente potrebbe forzare il
    /// provider con tool allegati: qui evitiamo il 400 alla fonte. Zero-copy
    /// (`Cow::Borrowed`) quando non c'e' nulla da strippare.
    fn prepared<'a>(&self, req: &'a LlmRequest) -> Cow<'a, LlmRequest> {
        if self.supports_tools || (req.tools.is_none() && req.tool_choice.is_none()) {
            Cow::Borrowed(req)
        } else {
            let mut r = req.clone();
            r.tools = None;
            r.tool_choice = None;
            Cow::Owned(r)
        }
    }
}

#[async_trait]
impl LlmProvider for GenericOpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_tools(&self) -> bool {
        self.supports_tools
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        self.max_context_tokens
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        &self.tiers
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let prepared = self.prepared(req);
        self.client.complete(prepared.as_ref()).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let prepared = self.prepared(req);
        self.client.stream(prepared.as_ref()).await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        self.client.list_models_meta().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacita_dai_parametri() {
        let p = GenericOpenAiProvider::new(
            Client::new(),
            "https://api.perplexity.ai",
            "key",
            "perplexity",
            vec![0, 1, 2],
            127_000,
            false, // sonar rifiuta le tool definitions
        );
        assert_eq!(p.name(), "perplexity");
        assert!(!p.supports_tools());
        assert_eq!(p.max_context_tokens(), 127_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }
}
