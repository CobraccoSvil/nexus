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

/// Risolutore del pool DB per-progetto (separazione DB per-progetto). Iniettato
/// da mcp-core, che possiede il registry globale (`project_data_pool_from`):
/// nexus-wiki non vede quel registry (la dipendenza e' invertita, mcp-core ->
/// nexus-wiki), quindi riceve solo questo contratto. I worker cross-progetto lo
/// usano per instradare le letture del dominio run/chat sul DB del singolo
/// progetto. A flag separazione OFF l'impl delega comunque a
/// `project_data_pool_from`, che ritorna il meta-DB -> comportamento invariato.
pub trait ProjectPoolResolver: std::fmt::Debug + Send + Sync {
    /// Pool del DB del dominio run/chat per `project_id`. A flag OFF ritorna il
    /// meta-DB; a flag ON il pool di `<slug>_nexus`.
    fn project_pool(&self, project_id: uuid::Uuid) -> BoxFuture<'_, PgPool>;
}

/// Contesto dei servizi usati dal wiki (sottoinsieme di AppState).
#[derive(Debug, Clone)]
pub struct WikiDeps {
    pub db: PgPool,
    pub template_cache: TemplateCache,
    pub ai: Arc<dyn WikiAiServices>,
    /// Risolutore pool per-progetto (separazione DB). `None` -> i worker
    /// cross-progetto ricadono sul meta-DB (`db`), comportamento storico.
    pub project_pool: Option<Arc<dyn ProjectPoolResolver>>,
}

impl WikiDeps {
    /// Pool del dominio run/chat per `project_id`: via il risolutore iniettato
    /// (separazione DB) oppure, se assente, il meta-DB. Punto unico (regola L)
    /// di routing per i worker cross-progetto.
    pub async fn run_pool(&self, project_id: uuid::Uuid) -> PgPool {
        match &self.project_pool {
            Some(r) => r.project_pool(project_id).await,
            None => self.db.clone(),
        }
    }

    /// Elenco dei `project_id` (tabella globale `projects`, sempre sul meta-DB).
    /// I worker cross-progetto iterano questo elenco e instradano le letture del
    /// dominio run/chat sul pool di ciascun progetto via [`Self::run_pool`].
    /// Delega al punto unico `nexus_project_pools::list_project_ids` (regola L).
    pub async fn list_project_ids(&self) -> Vec<uuid::Uuid> {
        nexus_project_pools::list_project_ids(&self.db).await
    }
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
