//! Adapter del trait [`nexus_agent_graph::runtime::ports::ContextOffload`].
//!
//! IMPLEMENTA (FASE 2b) `ContextOffload::offload_to_rag` scrivendo il payload su
//! RAG (Qdrant + embeddings) e ritornando un POINTER opaco, delegando al punto
//! unico di indicizzazione di mcp-core ([`crate::rag::indexer::index_text`], che
//! risolve internamente la config DB-driven — incluso `agent.rag.qdrant_url`, mig
//! `rag/config.rs` — e fa chunking + embed + upsert su Qdrant). La logica
//! head/tail/pointer-size (DECIDERE cosa offloadare) resta PURA fuori da qui
//! (regola L): questo adapter isola SOLO l'I/O di offload.
//!
//! GATE Real/Replay (PUNTO UNICO del gate shadow, regola L): in
//! [`ExecMode::Replay`] (run shadow read-only) e' un NO-OP che ritorna `PortError`
//! -> il chiamante degrada a troncamento testa+coda non-RAG (zero side-effect su
//! Qdrant). In [`ExecMode::Real`] indicizza davvero. BEST-EFFORT con DEGRADO A
//! TRONCAMENTO: su guasto infra (embed/Qdrant down, RAG disabilitato) ritorna
//! `PortError` e il chiamante degrada (non blocca il run).

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{ContextOffload, ExecMode, OffloadKind, PortError};

use crate::rag::{indexer, SourceKind};

/// Adapter [`ContextOffload`] -> RAG (Qdrant + embeddings) via
/// [`crate::rag::indexer::index_text`].
pub struct RagContextOffloadAdapter {
    /// Pool Postgres: l'indexer legge da qui la config RAG (qdrant_url, chunk size,
    /// embedding dim) DB-driven e indicizza (regola G).
    db: PgPool,
}

impl RagContextOffloadAdapter {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ContextOffload for RagContextOffloadAdapter {
    /// Scrive `payload` su RAG e ritorna un POINTER opaco (la chiave `source_id`
    /// con cui il recupero successivo filtra i chunk). DELEGA a
    /// [`crate::rag::indexer::index_text`] con kind [`SourceKind::ToolResult`]
    /// (collection del contesto offloadato).
    ///
    /// `kind` sceglie la collection RAG ([`OffloadKind::ToolResult`] per i
    /// tool_result compressi, [`OffloadKind::ChatHistory`] per gli originali del
    /// rolling-summary, recuperabili filtrando per `session_id`). GATE Real: in
    /// [`ExecMode::Replay`] e' un no-op che ritorna `PortError` (il run shadow non
    /// scrive Qdrant). Su guasto infra (anche in Real) ritorna `PortError` (il
    /// chiamante degrada a troncamento).
    async fn offload_to_rag(
        &self,
        payload: Value,
        kind: OffloadKind,
        session_id: Option<String>,
        project_id: Option<String>,
        mode: ExecMode,
    ) -> Result<String, PortError> {
        if mode != ExecMode::Real {
            // Run shadow read-only: nessuna scrittura su Qdrant (gate shadow).
            return Err(PortError::Tool(
                "context_offload: no-op in Replay (run shadow read-only)".to_string(),
            ));
        }

        // Il payload e' JSON opaco: lo serializziamo a testo per l'indicizzazione
        // (chunking + embed). Una stringa JSON resta testo indicizzabile.
        let text = match &payload {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.trim().is_empty() {
            return Err(PortError::Tool(
                "context_offload: payload vuoto, niente da offloadare".to_string(),
            ));
        }

        // Mappa OffloadKind (agnostico, regola L) -> SourceKind concreto: la
        // traduzione infra vive qui, unico confine col RAG di mcp-core.
        let source_kind = match kind {
            OffloadKind::ToolResult => SourceKind::ToolResult,
            OffloadKind::ChatHistory => SourceKind::ChatHistory,
        };
        // session/project come UUID (Some solo se parsano): abilitano il filtro di
        // retrieval (chat_history filtra su session_id). Un valore non-UUID e'
        // ignorato (offload senza filtro) invece di far fallire l'offload.
        let session_uuid = session_id.as_deref().and_then(|s| Uuid::parse_str(s).ok());
        let project_uuid = project_id.as_deref().and_then(|s| Uuid::parse_str(s).ok());

        // Pointer opaco deterministico per questa scrittura (source_id Qdrant).
        let pointer = format!("ctx-offload-{}", Uuid::new_v4());

        match indexer::index_text(
            &self.db,
            source_kind,
            &pointer,
            project_uuid,
            session_uuid,
            &text,
            Value::Null,
        )
        .await
        {
            Ok(n_chunks) if n_chunks > 0 => {
                tracing::info!(
                    pointer = %pointer,
                    chunks = n_chunks,
                    kind = ?kind,
                    "context_offload: payload indicizzato su RAG"
                );
                Ok(pointer)
            }
            Ok(_) => Err(PortError::Tool(
                "context_offload: nessun chunk indicizzato (payload troppo piccolo)".to_string(),
            )),
            Err(e) => {
                // Degrado a troncamento: errore infra (embed/Qdrant down, RAG off).
                tracing::warn!(
                    error = %e,
                    "context_offload: indicizzazione fallita, il chiamante degrada a troncamento"
                );
                Err(PortError::Tool(format!("context_offload: {e}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In `Replay` l'offload e' un no-op che ritorna `PortError` (gate shadow):
    /// nessun accesso a Qdrant, nessuna dipendenza dal bridge embed. Il chiamante
    /// degrada a troncamento testa+coda. Non serve schema DB: il gate scatta prima.
    #[sqlx::test]
    async fn replay_e_un_noop_che_ritorna_porterror(pool: PgPool) {
        let port = RagContextOffloadAdapter::new(pool.clone());
        let res = port
            .offload_to_rag(
                serde_json::json!({"k": "v"}),
                OffloadKind::ToolResult,
                None,
                None,
                ExecMode::Replay,
            )
            .await;
        assert!(
            res.is_err(),
            "in Replay l'offload deve fallire (no-op), il chiamante degrada a troncamento"
        );
    }

    /// Payload vuoto in `Real` -> `PortError` (niente da offloadare). Non tocca il
    /// bridge embed perche' il controllo del payload vuoto precede l'indicizzazione.
    #[sqlx::test]
    async fn payload_vuoto_in_real_ritorna_porterror(pool: PgPool) {
        let port = RagContextOffloadAdapter::new(pool.clone());
        let res = port
            .offload_to_rag(
                serde_json::Value::String(String::new()),
                OffloadKind::ChatHistory,
                None,
                None,
                ExecMode::Real,
            )
            .await;
        assert!(res.is_err(), "payload vuoto -> PortError");
    }
}
