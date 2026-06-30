//! nexus-wiki — Knowledge Graph unificato (ADR 0017 v2) estratto dal
//! monolite mcp-core (split 7.4, fase F).
//!
//! Contiene la logica del wiki (model, storage, vault, workers, watcher,
//! code graph) de-axumizzata: niente AppState, le dipendenze arrivano via
//! `WikiDeps` (db, template_cache, servizi AI dietro il trait
//! `WikiAiServices`). Gli handler HTTP (routes, search, internal, redirects)
//! restano in mcp-core::wiki, che re-esporta questo crate per mantenere
//! validi i path storici `crate::wiki::*`.

pub mod acl;
pub mod chat_note_worker;
pub mod code_docs_enricher;
pub mod code_graph;
pub mod content_points;
pub mod deps;
pub mod links_worker;
pub mod model;
pub mod reingest;
pub mod revisions;
pub mod run_summary_worker;
pub mod storage;
pub mod title_gen;
pub mod triple_extractor;
pub mod vault;
pub mod watcher;

pub use deps::{ProjectPoolResolver, WikiAiServices, WikiDeps};
