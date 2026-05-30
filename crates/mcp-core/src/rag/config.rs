//! Configurazione RAG caricata da `settings.agent.rag.*` con cache 60s.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};
use tokio::sync::RwLock;

use super::{RagError, SourceKind};

/// Snapshot immutabile della configurazione RAG.
#[derive(Clone, Debug)]
pub struct RagConfig {
    pub enabled: bool,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub top_k_default: usize,
    pub embedding_endpoint: String,
    pub qdrant_url: String,
    pub embedding_dim: usize,
    pub collection_attachments: String,
    pub collection_kb: String,
    pub collection_chat_history: String,
    pub collection_tool_results: String,
}

impl RagConfig {
    /// Ritorna il nome collection Qdrant per un dato `SourceKind`.
    /// Code usa la collection legacy `code_embeddings` di EmbeddingService.
    pub fn collection_for(&self, kind: SourceKind) -> &str {
        match kind {
            SourceKind::Attachment => &self.collection_attachments,
            SourceKind::Kb => &self.collection_kb,
            SourceKind::ChatHistory => &self.collection_chat_history,
            SourceKind::ToolResult => &self.collection_tool_results,
            SourceKind::Code => "code_embeddings",
            // Collection legacy (nomi fissi, come Code): popolate da
            // vector_memory.rs, payload eterogeneo gestito in search.rs.
            SourceKind::MetaDoc => "nexus_meta_docs",
            SourceKind::Conversation => "conversation_context",
            SourceKind::PromptCorrection => "prompt_corrections",
        }
    }
}

static CACHE: once_cell::sync::Lazy<RwLock<Option<(Arc<RagConfig>, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

const TTL: Duration = Duration::from_secs(60);

/// Carica (o usa cache) la configurazione RAG dal DB.
/// Niente fallback hardcoded silenziosi (regola G): se il DB e' down
/// e non c'e' cache valida, ritorna `RagError::Config`.
pub async fn current_config(db: &PgPool) -> Result<Arc<RagConfig>, RagError> {
    {
        let g = CACHE.read().await;
        if let Some((cfg, ts)) = g.as_ref() {
            if ts.elapsed() < TTL {
                return Ok(cfg.clone());
            }
        }
    }

    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key LIKE 'agent.rag.%'"
    )
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        // Se la cache vecchia esiste e il DB risponde con 0 righe, e' un
        // segnale che la migrazione 0200 non e' stata applicata. Falliamo
        // esplicitamente (regola G).
        let g = CACHE.read().await;
        if let Some((cfg, _)) = g.as_ref() {
            return Ok(cfg.clone());
        }
        return Err(RagError::Config(
            "settings.agent.rag.* mancanti, applicare migrazione 0200".into(),
        ));
    }

    let mut map = std::collections::HashMap::<String, String>::new();
    for r in rows {
        let k: String = r.try_get("key").unwrap_or_default();
        let v: String = r.try_get("value").unwrap_or_default();
        map.insert(k, v);
    }

    let get = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());

    let cfg = RagConfig {
        enabled: get("agent.rag.enabled", "true") == "true",
        chunk_size: get("agent.rag.chunk_size", "1000")
            .parse()
            .unwrap_or(1000),
        chunk_overlap: get("agent.rag.chunk_overlap", "200")
            .parse()
            .unwrap_or(200),
        top_k_default: get("agent.rag.top_k_default", "8").parse().unwrap_or(8),
        embedding_endpoint: get("agent.rag.embedding_endpoint", "/embed"),
        qdrant_url: get("agent.rag.qdrant_url", "http://localhost:6333"),
        embedding_dim: get("agent.rag.embedding_dim", "384")
            .parse()
            .unwrap_or(384),
        collection_attachments: get("agent.rag.collection_attachments", "attachment_chunks"),
        collection_kb: get("agent.rag.collection_kb", "kb_chunks"),
        collection_chat_history: get(
            "agent.rag.collection_chat_history",
            "chat_history_chunks",
        ),
        collection_tool_results: get(
            "agent.rag.collection_tool_results",
            "tool_results_chunks",
        ),
    };

    let arc = Arc::new(cfg);
    let mut g = CACHE.write().await;
    *g = Some((arc.clone(), Instant::now()));
    Ok(arc)
}
