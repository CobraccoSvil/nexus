//! Provider OpenAI.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts` + parita' con
//! `brain/providers/openai_provider.py`. Delega il trasporto al client condiviso
//! [`OpenAiCompatClient`] (composizione, regola L); aggiunge la detection
//! o-series (per nome modello) che cambia il dialetto reasoning: i modelli
//! reasoning (o1/o3/o4, gpt-5*, gpt-4.5*) usano `max_completion_tokens` al posto
//! di `max_tokens`, non accettano temperatura e ammettono `reasoning_effort`.

use std::time::Duration;

use async_trait::async_trait;
use nexus_cache::TtlCache;
use reqwest::Client;
use sqlx::PgPool;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint OpenAI di default. La `base_url` resta un parametro del costruttore
/// (override per gateway compatibili); questo valore e' solo il default quando
/// il chiamante non ne passa una.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Chiave settings (regola G) del livello di reasoning per i modelli o-series.
/// Valori ammessi dall'API: `low`/`medium`/`high`. Assente => non si invia
/// `reasoning_effort` (l'API usa il default del modello): nessun hardcoded.
const REASONING_EFFORT_SETTING: &str = "providers.openai.reasoning_effort";

/// TTL della cache settings (60s, come gli altri provider).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Famiglie reasoning (gpt-5*, gpt-4.5*) coperte per PREFISSO: ogni release
/// (gpt-5.1, gpt-5-mini, ...) e' gestita senza elencarla a mano (parita' col
/// Python `_is_o_series` ~104, regola G: famiglia strutturale, non nome esatto).
const O_SERIES_FAMILY_PREFIXES: &[&str] = &["gpt-5", "gpt-4.5"];

/// Basi o-series (o1/o3/o4) trattate per match esatto o `base-...` (parita' col
/// Python `_O_SERIES_MODELS`: `m == base || m.starts_with(base + "-")`). Non un
/// `starts_with` puro, che catturerebbe per errore nomi come `o1abc`.
const O_SERIES_BASES: &[&str] = &["o1", "o3", "o4"];

/// True se il modello richiede il dialetto reasoning o-series. Case-insensitive.
/// Parita' fedele col Python: prefisso per le famiglie gpt-5/gpt-4.5, match
/// esatto o `base-` per o1/o3/o4.
fn is_o_series(model: &str) -> bool {
    let m = model.to_lowercase();
    if O_SERIES_FAMILY_PREFIXES.iter().any(|p| m.starts_with(p)) {
        return true;
    }
    O_SERIES_BASES
        .iter()
        .any(|b| m == *b || m.starts_with(&format!("{b}-")))
}

pub struct OpenAiProvider {
    client: OpenAiCompatClient,
    db: Option<PgPool>,
    reasoning_effort: TtlCache<(), Option<String>>,
}

impl OpenAiProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). L'effort
    /// reasoning non sara' leggibile dai settings: si usa il default del modello.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere `reasoning_effort` dai
    /// settings (regola G). `base_url` opzionale (default OpenAI ufficiale); la
    /// `api_key` e' iniettata dal chiamante (regola F: niente segreti nel codice).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "openai"),
            db,
            reasoning_effort: TtlCache::new(SETTINGS_TTL),
        }
    }

    /// Livello reasoning dai settings (cache TTL 60s). `None` => chiave assente o
    /// DB irraggiungibile: non si invia `reasoning_effort` (default del modello).
    async fn configured_effort(&self) -> Option<String> {
        if let Some(e) = self.reasoning_effort.get(&()) {
            return e;
        }
        let db = self.db.as_ref()?;
        let value = nexus_auth::get_setting(db, REASONING_EFFORT_SETTING)
            .await
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        self.reasoning_effort.insert((), value.clone());
        value
    }

    /// Risolve il dialetto reasoning per la richiesta: o-series se il nome del
    /// modello lo richiede, altrimenti dialetto base. L'`effort` arriva dai
    /// settings solo per o-series.
    async fn resolve(&self, req: &LlmRequest) -> ResolvedReasoning {
        if !is_o_series(&req.model) {
            return ResolvedReasoning::none();
        }
        ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            // o-series e' sempre in reasoning mode (non disattivabile via param):
            // `enabled` informativo, il comportamento e' guidato dal dialetto.
            enabled: true,
            effort: self.configured_effort().await,
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
        let reasoning = self.resolve(req).await;
        self.client.complete_with_reasoning(req, &reasoning).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = self.resolve(req).await;
        self.client.stream_with_reasoning(req, &reasoning).await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
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

    #[test]
    fn detection_o_series_per_famiglia() {
        // Reasoning family per prefisso (parita' col Python).
        assert!(is_o_series("o1"));
        assert!(is_o_series("o1-mini"));
        assert!(is_o_series("o3"));
        assert!(is_o_series("o4-mini"));
        assert!(is_o_series("gpt-5"));
        assert!(is_o_series("gpt-5.1"));
        assert!(is_o_series("gpt-5-nano"));
        assert!(is_o_series("gpt-4.5-preview"));
        assert!(is_o_series("GPT-5")); // case-insensitive
                                       // Chat non-reasoning: dialetto base.
        assert!(!is_o_series("gpt-4o"));
        assert!(!is_o_series("gpt-4o-mini"));
        assert!(!is_o_series("gpt-4.1"));
    }
}
