//! Client gRPC/HTTP verso il neural-core (brain Python).

use serde_json::Value;

// Tipi mcp_proto::neural ri-esportati da super::* (regola L, S73).

use super::*;

#[derive(Clone)]
pub struct NeuralCoreClient {
    client: NeuralCoreServiceClient<tonic::transport::Channel>,
}

impl std::fmt::Debug for NeuralCoreClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralCoreClient").finish_non_exhaustive()
    }
}

impl NeuralCoreClient {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        // Default tonic decoding limit è 4MB: non basta per prompt grandi.
        // Allineato al server Python (128MB) in neural_service.py::serve.
        const MAX_MSG: usize = 128 * 1024 * 1024;
        let client = NeuralCoreServiceClient::connect(url.to_string())
            .await?
            .max_decoding_message_size(MAX_MSG)
            .max_encoding_message_size(MAX_MSG);
        tracing::info!("Connected to Neural Core at {} (max msg 128MB)", url);
        Ok(Self { client })
    }

    /// Client NON connesso per i test unit: canale lazy verso un endpoint
    /// irraggiungibile. Qualunque RPC fallirebbe al primo uso; serve solo a
    /// costruire un `AgentToolContext` nei test di tool che non toccano il brain.
    #[cfg(test)]
    pub(crate) fn disconnected_for_tests() -> Self {
        let channel =
            tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        Self {
            client: NeuralCoreServiceClient::new(channel),
        }
    }

    pub async fn embed_text(&self, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
        let (_model, vector) = self.embed_text_with_model(model, text).await?;
        Ok(vector)
    }

    /// Come `embed_text`, ma ritorna anche il nome del modello usato per
    /// generare il vettore. Serve a costruire una signature dell'embedder
    /// (modello + dimensione) da incorporare negli hash di indicizzazione:
    /// cosi' un cambio di embedder invalida automaticamente gli hash e forza il
    /// reindex, senza interventi manuali sul DB.
    ///
    /// Embedding ONNX MiniLM-384d IN-PROCESS (regola L: punto unico embedder,
    /// lo stesso `NexusBridge::embedder()` usato da `/api/embed`,
    /// `Orchestrator::embed_text` e dai tool ruvector). Niente piu' round-trip
    /// gRPC verso il brain Python: il brain stesso (`brain/embeddings/service.py`)
    /// ormai delega a `/api/embed` di mcp-core, quindi il gRPC `EmbedText` faceva
    /// un giro inutile Rust -> brain -> HTTP -> mcp-core -> ONNX. Ora chiama
    /// direttamente l'embedder locale.
    ///
    /// Label: replica la logica dell'handler `/api/embed` (label
    /// `all-MiniLM-L6-v2` quando `model` e' vuoto, altrimenti il `model`
    /// richiesto). Tutti i call site passano `""`, quindi il label resta
    /// `all-MiniLM-L6-v2` -- identico a quello che il brain ritornava prima,
    /// percio' gli hash di indicizzazione restano validi (nessun reindex).
    /// I vettori sono identici (stesso modello ONNX, parita' validata).
    pub async fn embed_text_with_model(
        &self,
        model: &str,
        text: &str,
    ) -> anyhow::Result<(String, Vec<f32>)> {
        let bridge = crate::nexus_bridge::NexusBridge::global().ok_or_else(|| {
            anyhow::anyhow!("nexus bridge non inizializzato (embed_text_with_model)")
        })?;
        let used_model = if model.trim().is_empty() {
            "all-MiniLM-L6-v2".to_string()
        } else {
            model.to_string()
        };
        // embed() e' CPU-bound sincrono: spawn_blocking per non bloccare il
        // runtime async (stesso pattern di Orchestrator::embed_text).
        let text = text.to_string();
        let vector = tokio::task::spawn_blocking(move || bridge.embed_one(&text))
            .await
            .map_err(|e| anyhow::anyhow!("embed_text_with_model spawn_blocking join: {e}"))?;
        if vector.is_empty() {
            anyhow::bail!("empty_embed_vector");
        }
        Ok((used_model, vector))
    }

    pub async fn generate_completion(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .generate_completion(GenerateCompletionRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                prompt: prompt.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn generate_agent_turn(
        &self,
        provider: &str,
        model: &str,
        messages_json: &str,
        tools_json: &str,
        max_tokens: u32,
        system_text: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .generate_agent_turn(GenerateAgentTurnRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                messages_json: messages_json.to_string(),
                tools_json: tools_json.to_string(),
                max_tokens,
                system_text: system_text.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn provider_health(&self, provider: &str) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .get_provider_health(mcp_proto::neural::ProviderHealthRequest {
                provider: provider.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn is_healthy(&self) -> bool {
        self.provider_health("system").await.is_ok()
    }

    /// Classificazione errori provider via il PUNTO UNICO Rust
    /// (``crate::provider_error_classifier::classify_text``, paritetico a
    /// ``brain/providers/error_handler.py`` con golden test cross-language —
    /// regola L / ADR 0026, Wave 8b).
    ///
    /// Prima questa funzione faceva una RPC gRPC ``ClassifyError`` al brain
    /// per ogni errore provider — overhead inutile su un path d'errore caldo,
    /// e fragile (se il brain e' down, non riesci nemmeno a classificare).
    /// Ora il classificatore vive in Rust: zero round-trip di rete, zero
    /// dipendenza da brain healthy per gestire un errore provider.
    ///
    /// La parte SDK-specifica (estrazione `retry-after` da
    /// `exc.response.headers`) resta lato Python perche' richiede l'oggetto
    /// eccezione vero, e viaggia gia' nel ``metadata`` del ``ProviderResult``
    /// (campo ``retry_after_seconds`` consumato dal loop agente lato brain).
    ///
    /// L'``&self`` e ``async`` sono mantenuti per compatibilita' di firma con
    /// i call site esistenti, ma la funzione e' di fatto sincrona e priva di
    /// I/O di rete.
    pub async fn classify_error(&self, error_text: &str, _provider: &str) -> String {
        crate::provider_error_classifier::classify_text(error_text).stop_reason
    }

    pub async fn generate_document(
        &self,
        doc_type: &str,
        content_json: &str,
        output_path: &str,
        standard: &str,
        title: &str,
        project_name: &str,
    ) -> anyhow::Result<(String, i32, i32)> {
        let mut client = self.client.clone();
        let resp = client
            .generate_document(mcp_proto::neural::GenerateDocumentRequest {
                doc_type: doc_type.to_string(),
                content_json: content_json.to_string(),
                output_path: output_path.to_string(),
                standard: standard.to_string(),
                title: title.to_string(),
                project_name: project_name.to_string(),
            })
            .await?;
        let inner = resp.into_inner();
        if !inner.error.is_empty() {
            anyhow::bail!("Document generation error: {}", inner.error);
        }
        Ok((inner.file_path, inner.page_count, inner.section_count))
    }
}
