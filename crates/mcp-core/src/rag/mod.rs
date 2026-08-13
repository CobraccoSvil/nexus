//! RAG strutturale unificato (ADR 0015).
//!
//! Layer di Retrieval-Augmented Generation che sostituisce i pre-extract
//! "whole file" con chunking + embedding + similarity search su Qdrant.

pub mod chunker;
pub mod collezioni;
mod config;
pub mod indexer;
pub mod qdrant_client;
pub mod search;

pub use config::current_config;
pub use indexer::index_attachment;
pub use search::{search_semantic, SemanticSearchReport};

/// Il vocabolario delle sorgenti indicizzabili: RI-ESPORTA il punto unico, che
/// vive in [`nexus_types::source_kind`].
///
/// Nasceva qui, accanto a chi interroga Qdrant, ma e' anche il vocabolario che
/// il tool `nexus_search_semantic` promette al modello — e lo schema di quel
/// tool sta in `nexus-agent-tools`, che questo crate non puo' raggiungere
/// (la dipendenza va nell'altro verso). Finche' quello schema era JSON scritto
/// a mano la duplicazione non si vedeva, e aveva gia' prodotto una divergenza:
/// il catalogo elencava 5 valori, `parse` ne accettava 8. I call site storici
/// (`crate::rag::SourceKind`) non cambiano.
pub use nexus_types::source_kind::SourceKind;

#[derive(Debug, thiserror::Error)]
pub enum RagError {
    #[error("RAG disabilitato via settings (agent.rag.enabled=false)")]
    Disabled,
    #[error("brain embed endpoint fallito: {0}")]
    Embed(String),
    #[error("qdrant fallito: {0}")]
    Qdrant(String),
    /// L'indexer ha ricevuto un kind la cui collection e' scritta da ALTRI:
    /// scriverci dentro ne romperebbe il payload. Distinta da [`Self::Qdrant`]
    /// perche' non e' un guasto dell'infrastruttura, e' un errore di
    /// programmazione (vedi [`collezioni::Scrittore`]).
    #[error("collection non scritta dal RAG: {0}")]
    ScritturaNonAmmessa(String),
    #[error("configurazione RAG invalida: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
