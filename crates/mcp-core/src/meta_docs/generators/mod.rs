// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators — Trait + implementazioni dei generator
//
// Ogni generator e' responsabile di produrre un sottoinsieme di note del vault.
// Vengono invocati a ogni `ingest_commit` (filtrati per `relevant_for(files)`)
// e dal worker periodico `MetaDocsRefreshWorker`.
// ═══════════════════════════════════════════════════════════════════════════

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

pub mod schema;
pub mod architecture;
pub mod api;
pub mod changelog;
pub mod decisions;
pub mod concepts;

/// Output di un generator: una nota da scrivere/aggiornare nel vault.
pub struct GeneratedDoc {
    pub kind: String,
    pub title: String,
    pub slug: String,
    pub body_md: String,
    pub tags: Vec<String>,
    pub source_files: Vec<String>,
    pub source_commit: Option<String>,
    pub vault_file_path: String,
    pub now: DateTime<Utc>,
}

/// Contesto passato a ogni generator.
pub struct MetaDocContext<'a> {
    pub db: &'a PgPool,
    pub repo_root: String,
    pub vault_root: String,
    pub commit_sha: Option<String>,
    pub files_changed: Vec<String>,
}

#[async_trait]
pub trait MetaDocGenerator: Send + Sync {
    fn name(&self) -> &'static str;

    /// Decide se questo generator deve girare per il commit dato.
    /// Default: girare sempre (override per generator selettivi).
    fn relevant_for(&self, _files: &[String]) -> bool {
        true
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>>;
}

/// Registry dei generator disponibili. L'ordine determina la priorita' di esecuzione.
pub fn all_generators() -> Vec<Box<dyn MetaDocGenerator>> {
    vec![
        Box::new(schema::SchemaGenerator),
        Box::new(architecture::ArchitectureGenerator),
        Box::new(api::ApiGenerator),
        Box::new(changelog::ChangelogGenerator),
        Box::new(decisions::DecisionExtractor),
        Box::new(concepts::ConceptsGenerator),
    ]
}
