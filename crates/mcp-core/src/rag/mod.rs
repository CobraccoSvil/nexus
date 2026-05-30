//! RAG strutturale unificato (ADR 0015).
//!
//! Layer di Retrieval-Augmented Generation che sostituisce i pre-extract
//! "whole file" con chunking + embedding + similarity search su Qdrant.

pub mod chunker;
pub mod qdrant_client;
pub mod indexer;
pub mod search;
mod config;

pub use config::{RagConfig, current_config};
pub use indexer::{index_text, index_attachment, delete_source};
pub use search::{search_semantic, SearchHit};

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
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Attachment => "attachment",
            SourceKind::Kb => "kb",
            SourceKind::ChatHistory => "chat_history",
            SourceKind::ToolResult => "tool_result",
            SourceKind::Code => "code",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "attachment" => Some(SourceKind::Attachment),
            "kb" => Some(SourceKind::Kb),
            "chat_history" => Some(SourceKind::ChatHistory),
            "tool_result" => Some(SourceKind::ToolResult),
            "code" => Some(SourceKind::Code),
            _ => None,
        }
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
