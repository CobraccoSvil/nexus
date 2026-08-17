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

    /// Dichiara dove questo endpoint espone la lista modelli, quando il fornitore
    /// non la tiene sotto la base delle completion (`nexus_provider_registry.models_path`,
    /// mig 0705). `None` = il `/models` del dialetto OpenAI.
    ///
    /// Delega la normalizzazione al client, che e' il punto in cui quella URL si
    /// compone: qui il valore viene dal registry e non si tocca.
    pub fn with_models_path(mut self, path: Option<&str>) -> Self {
        self.client = self.client.with_models_path(path);
        self
    }

    /// Aggancia il DB da cui gli INSTRADATORI leggono quale fornitore a valle
    /// preferire (`nexus_router_upstream_affinity`, mig 0657). Gli altri
    /// endpoint non lo interrogano: la domanda vale solo dove c'e' davvero
    /// qualcosa da scegliere.
    pub fn with_db(mut self, db: Option<sqlx::PgPool>) -> Self {
        self.client = self.client.with_db(db);
        self
    }

    /// Dichiara gli header extra del registry
    /// (`nexus_provider_registry.extra_headers`, mig 0714): il client li
    /// applica a ogni richiesta HTTP. Openrouter li usa per l'attribuzione
    /// (`HTTP-Referer`/`X-Title`); per gli altri il campo e' NULL e la lista
    /// arriva vuota.
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.client = self.client.with_extra_headers(headers);
        self
    }

    /// Dichiara l'opt-in di usage accounting del registry
    /// (`nexus_provider_registry.usage_accounting`, mig 0717): il client
    /// aggiunge `usage: {"include": true}` al body e il fornitore dichiara il
    /// costo esatto in `usage.cost`. Oggi solo openrouter; per gli altri il
    /// flag e' false e il campo non parte.
    pub fn with_usage_accounting(mut self, attivo: bool) -> Self {
        self.client = self.client.with_usage_accounting(attivo);
        self
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
    use crate::providers::openai_compat::ResolvedReasoning;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    /// L'instradatore del registry su cui i tre livelli di affinita' del
    /// prefisso sono stati misurati (29/07/2026).
    const INSTRADATORE: &str = "openrouter";

    fn richiesta() -> LlmRequest {
        LlmRequest {
            model: "qwen/qwen3-235b-a22b-2507".to_string(),
            messages: vec![LlmMessage {
                role: "system".to_string(),
                content: MessageContent::Text("istruzioni di progetto".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error: None,
            }],
            temperature: None,
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
        }
    }

    /// Dal NOME del registry fino ai campi che partono: e' la catena che questo
    /// file decide, e non era coperta da nulla.
    ///
    /// MISURATO il 29/07/2026: bastavano due caratteri
    /// (`"openrouter"` -> `"open-router"` in [`cache_keying_per_endpoint`]) per
    /// spegnere insieme `session_id`, `prompt_cache_key` e `provider.order`
    /// sull'UNICO instradatore del sistema, e la suite del crate restava a 407
    /// verdi. I due adapter gemelli (mistral, openai) il test del proprio
    /// dialetto ce l'avevano: l'unico senza era quello dove i tre livelli si
    /// applicano tutti (regola O).
    ///
    /// Guarda la CONSEGUENZA (i campi sul wire) e non il valore dell'enum: un
    /// test su `cache_keying()` proverebbe che la riga di `match` ritorna cio'
    /// che c'e' scritto, non che qualcuno la legga.
    #[tokio::test]
    async fn dal_nome_del_registry_ai_campi_di_affinita() {
        let instradatore = GenericOpenAiProvider::new(
            Client::new(),
            "https://openrouter.ai/api/v1",
            "chiave",
            INSTRADATORE,
            vec![0, 1],
            256_000,
            true,
        );
        let corpo = serde_json::to_value(
            instradatore
                .client
                .corpo_della_richiesta(&richiesta(), false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        let chiave = corpo
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .expect("l'instradatore inoltra la chiave al fornitore a valle");
        assert_eq!(
            corpo.get("session_id").and_then(|v| v.as_str()),
            Some(chiave),
            "l'instradatore legge session_id per fissare il fornitore"
        );

        // Un endpoint OpenAI-compat che non instrada verso terzi resta come
        // prima: un campo sconosciuto e' il solo verso che puo' fare danno.
        let diretto = GenericOpenAiProvider::new(
            Client::new(),
            "https://api.groq.com/openai/v1",
            "chiave",
            "groq",
            vec![0, 1],
            128_000,
            true,
        );
        let corpo = serde_json::to_value(
            diretto
                .client
                .corpo_della_richiesta(&richiesta(), false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        assert!(corpo.get("prompt_cache_key").is_none());
        assert!(corpo.get("session_id").is_none());
        assert!(corpo.get("provider").is_none());
    }

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
