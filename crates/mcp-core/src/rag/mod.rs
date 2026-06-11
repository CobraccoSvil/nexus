//! RAG strutturale unificato (ADR 0015).
//!
//! Layer di Retrieval-Augmented Generation che sostituisce i pre-extract
//! "whole file" con chunking + embedding + similarity search su Qdrant.

pub mod chunker;
mod config;
pub mod indexer;
pub mod qdrant_client;
pub mod search;

pub use config::current_config;
pub use indexer::index_attachment;
pub use search::search_semantic;

use serde::{Deserialize, Serialize};

/// Tipologie di sorgenti indicizzabili.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Attachment,
    Kb,
    ChatHistory,
    ToolResult,
    Code,
    // Mini-PR: collection legacy esposte tramite il canale unico
    // nexus_search_semantic. Payload eterogeneo gestito in search.rs.
    MetaDoc,
    Conversation,
    PromptCorrection,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Attachment => "attachment",
            SourceKind::Kb => "kb",
            SourceKind::ChatHistory => "chat_history",
            SourceKind::ToolResult => "tool_result",
            SourceKind::Code => "code",
            SourceKind::MetaDoc => "meta_doc",
            SourceKind::Conversation => "conversation",
            SourceKind::PromptCorrection => "prompt_correction",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "attachment" => Some(SourceKind::Attachment),
            "kb" => Some(SourceKind::Kb),
            "chat_history" => Some(SourceKind::ChatHistory),
            "tool_result" => Some(SourceKind::ToolResult),
            "code" => Some(SourceKind::Code),
            "meta_doc" => Some(SourceKind::MetaDoc),
            "conversation" => Some(SourceKind::Conversation),
            "prompt_correction" => Some(SourceKind::PromptCorrection),
            _ => None,
        }
    }

    /// True se la collection del kind ha `project_id` nel payload (filtrabile).
    /// Conversation usa session_id; MetaDoc e' globale (nessun filtro project).
    pub fn supports_project_filter(&self) -> bool {
        !matches!(self, SourceKind::Conversation | SourceKind::MetaDoc)
    }

    /// True se il kind filtra per session_id (chat conversazionali).
    pub fn uses_session_filter(&self) -> bool {
        matches!(self, SourceKind::ChatHistory | SourceKind::Conversation)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RagError {
    #[error("RAG disabilitato via settings (agent.rag.enabled=false)")]
    Disabled,
    #[error("brain embed endpoint fallito: {0}")]
    Embed(String),
    #[error("qdrant fallito: {0}")]
    Qdrant(String),
    #[error("configurazione RAG invalida: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
