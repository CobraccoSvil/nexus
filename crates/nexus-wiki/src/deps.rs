//! Dipendenze iniettate del wiki (`WikiDeps`).
//!
//! Il wiki estratto dal monolite non conosce AppState: riceve i soli servizi
//! che usa. mcp-core costruisce `WikiDeps` da AppState (adapter in
//! mcp-core::wiki) e implementa `WikiAiServices` su NeuralCoreClient +
//! internal_routing. Nei file migrati il parametro si chiama ancora `state`
//! per minimizzare il diff (state.db, state.template_cache restano validi).

use std::sync::Arc;

use futures::future::BoxFuture;
use nexus_types::TemplateCache;
use serde_json::Value;
use sqlx::PgPool;

/// Servizi AI richiesti dal wiki: embedding, completion e risoluzione del
/// purpose model (regola G: la configurazione resta nel DB, qui solo il
/// contratto di invocazione).
pub trait WikiAiServices: std::fmt::Debug + Send + Sync {
    /// Embedding del testo (model vuoto = default del brain).
    fn embed_text(&self, model: &str, text: &str)
        -> BoxFuture<'_, anyhow::Result<Vec<f32>>>;

    /// Completion LLM presso provider/model espliciti.
    fn generate_completion(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
    ) -> BoxFuture<'_, anyhow::Result<Value>>;

    /// Risolve il purpose model interno (es. "wiki_title_gen") in
    /// `(provider, model)`. Err con messaggio diagnostico se non risolvibile.
    fn resolve_purpose_model(
        &self,
        purpose: &str,
    ) -> BoxFuture<'_, Result<(String, String), String>>;
}

/// Contesto dei servizi usati dal wiki (sottoinsieme di AppState).
#[derive(Debug, Clone)]
pub struct WikiDeps {
    pub db: PgPool,
    pub template_cache: TemplateCache,
    pub ai: Arc<dyn WikiAiServices>,
}

/// Estrae `(input_tokens, output_tokens)` dal payload di `generate_completion`,
/// tollerando le varianti di naming dei provider (`prompt_tokens`/`input_tokens`
/// e annidamento sotto `usage`). Punto unico (regola L) per i worker wiki che
/// contabilizzano i token (`title_gen`, `triple_extractor`).
pub fn extract_usage_tokens(resp: &Value) -> (i64, i64) {
    let input = resp
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .or_else(|| resp.get("input_tokens").and_then(|v| v.as_i64()))
        .or_else(|| {
            resp.get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    let output = resp
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .or_else(|| resp.get("output_tokens").and_then(|v| v.as_i64()))
        .or_else(|| {
            resp.get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    (input, output)
}
