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
