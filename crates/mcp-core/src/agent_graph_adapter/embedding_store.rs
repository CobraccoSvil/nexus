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
//! BEST-EFFORT con DEGRADO: su guasto embedder (bridge non inizializzato, vettore
//! vuoto) ritorna `PortError` e il nodo degrada (niente continuity-trim, history
//! invariata).
//!
//! Regola F: niente testo in chiaro nei log (solo conteggi/lunghezze).

use async_trait::async_trait;

use nexus_agent_graph::runtime::ports::{EmbeddingStore, PortError};

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
    /// Embedda `texts` (batch) col MiniLM in-process, preservando l'ordine. Su
    /// qualunque guasto ritorna `PortError`. `texts` vuoto -> `Ok(vec![])`.
    /// Best-effort.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, PortError> {
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
                    return Err(PortError::Tool(format!("embed: {e}").into()));
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

    /// `texts` vuoto -> `Ok(vec![])` (niente da embeddare, non e' un errore). Non
    /// tocca il bridge (il controllo del vuoto precede la risoluzione embedder).
    #[tokio::test]
    async fn testi_vuoti_ritorna_vec_vuoto() {
        let store = PgEmbeddingStore::new();
        let res = store.embed(vec![]).await;
        assert!(matches!(res, Ok(v) if v.is_empty()));
    }
}
