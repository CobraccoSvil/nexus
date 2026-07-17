//! Adapter del trait [`nexus_agent_graph::runtime::ports::EmbeddingStore`].
//!
//! IMPLEMENTA `EmbeddingStore::embed` calcolando l'embedding di ciascun testo con
//! l'embedder ONNX MiniLM-384d IN-PROCESS via il PUNTO UNICO
//! [`crate::orchestrator::NeuralCoreClient::embed_text_with_model`] (regola L: lo
//! stesso embedder usato da `agent_tools::semantic_tools` e da `rag`, nessun
//! round-trip). La DECISIONE (coseno vs focus, chi scartare) resta PURA in
//! [`nexus_agent_graph::decisions::context_reduction`] (regola L): questo adapter
//! isola SOLO l'I/O di embedding.
//!
//! GATE Real/Replay (PUNTO UNICO del gate shadow, regola L; uniforme con
//! [`super::summary_store::PgSummaryStore`] /
//! [`super::context_offload::RagContextOffloadAdapter`]): in [`ExecMode::Replay`]
//! (run shadow read-only) e' un NO-OP che ritorna `PortError` -> il nodo degrada al
//! troncamento POSIZIONALE odierno (zero divergenza dal replay, zero costo CPU).
//! In [`ExecMode::Real`] embedda davvero. BEST-EFFORT con DEGRADO: su guasto
//! embedder (bridge non inizializzato, vettore vuoto) ritorna `PortError` e il nodo
//! degrada (niente continuity-trim, history invariata).
//!
//! Regola F: niente testo in chiaro nei log (solo conteggi/lunghezze).

use async_trait::async_trait;

use nexus_agent_graph::runtime::ports::{EmbeddingStore, ExecMode, PortError};

use crate::orchestrator::NeuralCoreClient;

/// Adapter [`EmbeddingStore`] -> embedder ONNX MiniLM-384d in-process
/// (`NeuralCoreClient`, singleton `NexusBridge`). Nessuno stato da iniettare.
#[derive(Default)]
pub struct PgEmbeddingStore;

impl PgEmbeddingStore {
    /// Costruisce l'adapter. L'embedder e' il singleton in-process `NexusBridge`
    /// (risolto per-chiamata via `NeuralCoreClient::connect`, che non apre canali).
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EmbeddingStore for PgEmbeddingStore {
    /// Embedda `texts` (batch) col MiniLM in-process, preservando l'ordine. GATE
    /// Real: in [`ExecMode::Replay`] e' un no-op che ritorna `PortError` (il nodo
    /// degrada al troncamento posizionale). Su qualunque guasto (anche in Real)
    /// ritorna `PortError`. `texts` vuoto -> `Ok(vec![])`. Best-effort.
    async fn embed(&self, texts: Vec<String>, mode: ExecMode) -> Result<Vec<Vec<f32>>, PortError> {
        if mode != ExecMode::Real {
            // Run shadow read-only: niente embedding (gate shadow). Il nodo degrada
            // al troncamento posizionale, senza divergere dal replay.
            return Err(PortError::Tool(
                "embed: no-op in Replay (run shadow read-only)".to_string(),
            ));
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Punto unico embedder ONNX MiniLM-384d in-process (regola L). Il client
        // e' zero-sized e non apre canali; model "" -> label MiniLM di default.
        let neural = NeuralCoreClient::new();

        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for text in &texts {
            match neural.embed_text_with_model("", text).await {
                Ok((_model, vector)) => out.push(vector),
                Err(e) => {
                    // Guasto embedder: degrado best-effort (niente continuity-trim).
                    tracing::warn!(
                        error = %e,
                        n_texts = texts.len(),
                        "embed: embedder non disponibile, il nodo degrada al troncamento posizionale"
                    );
                    return Err(PortError::Tool(format!("embed: {e}")));
                }
            }
        }
        tracing::debug!(
            n = out.len(),
            "embed: vettori calcolati per continuity-trim"
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In `Replay` l'embed e' un no-op che ritorna `PortError` (gate shadow): il
    /// nodo degrada al troncamento posizionale. Non tocca il bridge embedder.
    #[tokio::test]
    async fn replay_e_un_noop_che_ritorna_porterror() {
        let store = PgEmbeddingStore::new();
        let res = store
            .embed(vec!["ciao".to_string()], ExecMode::Replay)
            .await;
        assert!(res.is_err(), "in Replay l'embed deve fallire (no-op)");
    }

    /// `texts` vuoto in Real -> `Ok(vec![])` (niente da embeddare, non e' un errore).
    /// Non tocca il bridge (il controllo del vuoto precede la risoluzione embedder).
    #[tokio::test]
    async fn testi_vuoti_in_real_ritorna_vec_vuoto() {
        let store = PgEmbeddingStore::new();
        let res = store.embed(vec![], ExecMode::Real).await;
        assert!(matches!(res, Ok(v) if v.is_empty()));
    }
}
