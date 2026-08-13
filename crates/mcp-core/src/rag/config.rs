//! Configurazione RAG caricata da `settings.agent.rag.*` con cache 60s.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};
use tokio::sync::RwLock;

use super::RagError;

/// Snapshot immutabile della configurazione RAG.
#[derive(Clone, Debug)]
pub struct RagConfig {
    pub enabled: bool,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub top_k_default: usize,
    pub qdrant_url: String,
    pub embedding_dim: usize,
    /// Le tre collection SCRITTE dall'indexer di questo modulo: qui il nome e'
    /// davvero una configurazione, perche' `ensure_collection` la crea con
    /// quello (lettore e scrittore sono la stessa funzione).
    pub collection_attachments: String,
    pub collection_chat_history: String,
    pub collection_tool_results: String,
    /// Collection dell'indice di codice: risolta dal punto unico dello
    /// SCRITTORE (`vector_memory::code_index_collection`, chiave
    /// `qdrant_code_index_collection`), mai da un nome inciso qui. Il nome
    /// inciso c'era — "code_embeddings" — ed era di una collection mai
    /// esistita: ogni ricerca sul codice rispondeva zero per costruzione.
    pub collection_code: String,
    /// Collection del wiki (`nexus_wiki::content_points`), che serve i kind
    /// `Kb` e `MetaDoc`: entrambi avevano qui un nome inciso, ed entrambi
    /// nominavano una collection senza scrittore. Vedi [`super::collezioni`].
    pub collection_wiki: String,
    /// Collection del contesto conversazione (`vector_memory`).
    pub collection_conversation: String,
    /// Collection delle correzioni di prompt (`vector_memory`).
    pub collection_corrections: String,
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
    load_uncached(db).await
}

/// Il caricamento vero, senza passare dalla cache globale. Separato perche' la
/// cache non e' chiavata per DB: un test `#[sqlx::test]` che passasse da
/// `current_config` leggerebbe (o avvelenerebbe) la config di un ALTRO pool —
/// la stessa trappola dei sei test flaky gia' misurata su questo pattern.
async fn load_uncached(db: &PgPool) -> Result<Arc<RagConfig>, RagError> {
    let rows = sqlx::query("SELECT key, value FROM settings WHERE key LIKE 'agent.rag.%'")
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
        chunk_size: get("agent.rag.chunk_size", "1000").parse().unwrap_or(1000),
        chunk_overlap: get("agent.rag.chunk_overlap", "200").parse().unwrap_or(200),
        top_k_default: get("agent.rag.top_k_default", "8").parse().unwrap_or(8),
        qdrant_url: get("agent.rag.qdrant_url", "http://localhost:6333"),
        embedding_dim: get("agent.rag.embedding_dim", "384").parse().unwrap_or(384),
        collection_attachments: get("agent.rag.collection_attachments", "attachment_chunks"),
        collection_chat_history: get("agent.rag.collection_chat_history", "chat_history_chunks"),
        collection_tool_results: get("agent.rag.collection_tool_results", "tool_results_chunks"),
        // Dal punto unico dello SCRITTORE, NON da una chiave agent.rag.*:
        // lettore e scrittore non devono divergere mai (regola L). Le quattro
        // collection qui sotto le crea e le popola qualcun altro; questo modulo
        // e' un lettore ospite e non ne sceglie il nome.
        collection_code: crate::vector_memory::code_index_collection(db).await,
        collection_wiki: nexus_wiki::content_points::wiki_content_collection(db)
            .await
            .map_err(|e| RagError::Config(format!("nome collection wiki non risolvibile: {e}")))?,
        collection_conversation: crate::vector_memory::conversation_context_collection_name(db)
            .await
            .map_err(|e| {
                RagError::Config(format!("nome collection conversazione non risolvibile: {e}"))
            })?,
        collection_corrections: crate::vector_memory::prompt_corrections_collection_name(db)
            .await
            .map_err(|e| {
                RagError::Config(format!("nome collection correzioni non risolvibile: {e}"))
            })?,
    };

    let arc = Arc::new(cfg);
    let mut g = CACHE.write().await;
    *g = Some((arc.clone(), Instant::now()));
    Ok(arc)
}

/// Il caricamento senza cache, esposto ai test del modulo fratello
/// [`super::collezioni`]: la domanda «quale collection risponde per questo
/// kind» si verifica sulla config REALE letta dal DB migrato, non su una
/// `RagConfig` costruita a mano (regola O).
#[cfg(test)]
pub(super) async fn config_dal_db(db: &PgPool) -> Result<Arc<RagConfig>, RagError> {
    load_uncached(db).await
}
