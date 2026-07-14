// ═══════════════════════════════════════════════════════════════════════════
// wiki/ — Knowledge Graph unificato (ADR 0017 v2).
//
// Split 7.4 fase F: la logica (model, storage, vault, acl, workers, watcher,
// code_graph, reingest, title_gen, triple_extractor, content_points) vive nel
// crate nexus-wiki, de-axumizzata: niente AppState, dipendenze via `WikiDeps`
// (db + template_cache + servizi AI dietro il trait `WikiAiServices`).
// Qui restano gli handler HTTP (routes, search, internal, redirects) e
// l'adapter `AppState::wiki_deps()` + l'impl `WikiAiServices` su
// NeuralCoreClient/internal_routing. Il re-export mantiene validi i path
// storici `crate::wiki::*`.
//
// Le tabelle di riferimento sono:
//   - wiki_docs              (scope ∈ {meta, project})
//   - wiki_links             (FK su wiki_docs)
//   - wiki_concept_triples   (FK su wiki_docs)
//   - wiki_doc_revisions     (FK su wiki_docs, polimorfico via doc_id)
//
// L'ACL e' applicata in un punto solo (`acl::WikiAcl`) e i path REST vivono
// sotto `/api/wiki/*` con `scope` come query-param. Vedi ADR 0017 v2.
// ═══════════════════════════════════════════════════════════════════════════

pub use nexus_wiki::*;

pub mod internal;
pub mod redirects;
pub mod routes;
pub mod search;

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

/// Impl mcp-core dei servizi AI del wiki: embedding/completion via
/// NeuralCoreClient, purpose model via internal_routing (punti unici G/L).
#[derive(Clone)]
pub(crate) struct AppStateWikiAi {
    state: crate::AppState,
}

impl std::fmt::Debug for AppStateWikiAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppStateWikiAi")
    }
}

impl WikiAiServices for AppStateWikiAi {
    fn embed_text(&self, model: &str, text: &str) -> BoxFuture<'_, anyhow::Result<Vec<f32>>> {
        let model = model.to_string();
        let text = text.to_string();
        Box::pin(async move {
            self.state
                .orchestrator
                .neural
                .embed_text(&model, &text)
                .await
        })
    }

    fn generate_completion(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
    ) -> BoxFuture<'_, anyhow::Result<Value>> {
        let provider = provider.to_string();
        let model = model.to_string();
        let prompt = prompt.to_string();
        Box::pin(async move {
            self.state
                .orchestrator
                .neural
                .generate_completion(&provider, &model, &prompt)
                .await
        })
    }

    fn resolve_purpose_model(
        &self,
        purpose: &str,
    ) -> BoxFuture<'_, Result<(String, String), String>> {
        let purpose = purpose.to_string();
        Box::pin(async move {
            crate::internal_routing::resolve_purpose_model(&self.state, &purpose)
                .await
                .into_model(&purpose)
        })
    }

    fn resolve_purpose_model_excluding(
        &self,
        purpose: &str,
        exclude_providers: &[String],
    ) -> BoxFuture<'_, Result<(String, String), String>> {
        let purpose = purpose.to_string();
        let exclude = exclude_providers.to_vec();
        Box::pin(async move {
            crate::internal_routing::resolve_purpose_model_db_excluding(
                &self.state.db,
                &purpose,
                &exclude,
            )
            .await
            .into_model(&purpose)
        })
    }

    fn notify_provider_llm_failure(
        &self,
        provider: &str,
        error_class: Option<&str>,
        message: &str,
    ) -> BoxFuture<'_, ()> {
        let provider = provider.to_string();
        let error_class = error_class.map(str::to_string);
        let message = message.to_string();
        Box::pin(async move {
            crate::brain_agent_client::handle_provider_llm_failure(
                &provider,
                error_class.as_deref(),
                &message,
            );
        })
    }
}

/// Impl mcp-core del risolutore pool per-progetto per i worker wiki. Delega al
/// registry globale `project_data_pool_from` (punto unico, regola L), che
/// instrada sul pool di `<slug>_nexus` e vi aggiunge il layer che il crate
/// read-only `nexus-project-pools` non puo' offrire: provisioning al primo
/// accesso, migrazioni `db/migrations/project` sotto lock per-progetto e cache
/// pool condivisa con AppState. Ritorna il meta solo per resilienza (registry
/// non inizializzato o provisioning fallito), mai per configurazione: il flag
/// separazione e' stato rimosso (mig 0527). Tiene solo il meta pool (i worker
/// girano in background, senza AppState vivo).
#[derive(Clone)]
pub(crate) struct AppStateProjectPool {
    meta: sqlx::PgPool,
}

impl std::fmt::Debug for AppStateProjectPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppStateProjectPool")
    }
}

impl nexus_wiki::ProjectPoolResolver for AppStateProjectPool {
    fn project_pool(&self, project_id: uuid::Uuid) -> BoxFuture<'_, sqlx::PgPool> {
        let meta = self.meta.clone();
        Box::pin(async move {
            crate::project_db_routes::project_data_pool_from(&meta, project_id).await
        })
    }
}

impl crate::AppState {
    /// Costruisce il contesto `WikiDeps` per le funzioni del crate nexus-wiki.
    pub(crate) fn wiki_deps(&self) -> WikiDeps {
        WikiDeps {
            db: self.db.clone(),
            template_cache: self.template_cache.clone(),
            ai: Arc::new(AppStateWikiAi {
                state: self.clone(),
            }),
            project_pool: Some(Arc::new(AppStateProjectPool {
                meta: self.db.clone(),
            })),
        }
    }
}
