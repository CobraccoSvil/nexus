//! Client gRPC/HTTP verso il neural-core (brain Python).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use mcp_proto::neural::{
    neural_core_service_client::NeuralCoreServiceClient, ClassifyIntentRequest, EmbedTextRequest,
    GenerateAgentTurnRequest, GenerateCompletionRequest, RouteModelRequest,
};

use crate::{
    billing::{self, UsageNumbers},
    domain::OrchestratorAudit,
    nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest, NexusGatewayClient},
    provider_cooldown::{is_provider_in_cooldown, put_provider_in_cooldown},
    vector_memory,
};

use super::*;

#[derive(Clone)]
pub struct NeuralCoreClient {
    client: NeuralCoreServiceClient<tonic::transport::Channel>,
    brain_http_url: String,
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
        let brain_http_url =
            std::env::var("BRAIN_HTTP_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        Ok(Self {
            client,
            brain_http_url,
        })
    }

    #[allow(dead_code)]
    pub async fn classify_intent(
        &self,
        project_id: &str,
        profile_id: &str,
        message: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .classify_intent(ClassifyIntentRequest {
                project_id: project_id.to_string(),
                profile_id: profile_id.to_string(),
                message: message.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    #[allow(dead_code)]
    pub async fn route_model(
        &self,
        project_id: &str,
        profile_id: &str,
        intent: &str,
        token_budget: u32,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .route_model(RouteModelRequest {
                project_id: project_id.to_string(),
                profile_id: profile_id.to_string(),
                intent: intent.to_string(),
                token_budget,
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn embed_text(&self, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
        let (_model, vector) = self.embed_text_with_model(model, text).await?;
        Ok(vector)
    }

    /// Come `embed_text`, ma ritorna anche il nome del modello effettivamente
    /// usato dal brain per generare il vettore. Serve a costruire una signature
    /// dell'embedder (modello + dimensione) da incorporare negli hash di
    /// indicizzazione: cosi' un cambio di embedder invalida automaticamente gli
    /// hash e forza il reindex, senza interventi manuali sul DB.
    pub async fn embed_text_with_model(
        &self,
        model: &str,
        text: &str,
    ) -> anyhow::Result<(String, Vec<f32>)> {
        let mut client = self.client.clone();
        let resp = client
            .embed_text(EmbedTextRequest {
                model: model.to_string(),
                text: text.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        let vector = json
            .get("vector")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("invalid_embed_response"))?
            .iter()
            .filter_map(|value| value.as_f64().map(|num| num as f32))
            .collect::<Vec<_>>();
        if vector.is_empty() {
            anyhow::bail!("empty_embed_vector");
        }
        // Il brain ritorna il modello realmente usato (default risolto se input
        // vuoto). Se assente, ripieghiamo su una etichetta neutra.
        let used_model = json
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("unknown")
            .to_string();
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

    /// Streaming di un turno agente via HTTP SSE: chiama il brain su /agent-turn/stream
    /// e invoca `on_token` per ogni token parziale ricevuto.
    /// Ritorna il risultato completo (stesso schema JSON di generate_agent_turn) al termine.
    pub async fn generate_agent_turn_stream(
        &self,
        provider: &str,
        model: &str,
        messages_json: &str,
        tools_json: &str,
        max_tokens: u32,
        system_text: &str,
        on_token: impl Fn(String) + Send,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/agent-turn/stream", self.brain_http_url);
        let body = serde_json::json!({
            "provider": provider,
            "model": model,
            "messages_json": messages_json,
            "tools_json": tools_json,
            "max_tokens": max_tokens,
            "system_text": system_text,
        });
        let http_result = reqwest::Client::new().post(&url).json(&body).send().await;
        let mut resp = match http_result {
            Ok(r) if r.status().is_success() => r,
            Ok(_) | Err(_) => {
                // Provider non supporta streaming HTTP — fallback a gRPC senza token delta
                tracing::debug!(
                    "brain HTTP stream non disponibile per provider={}, fallback a gRPC",
                    provider
                );
                return self
                    .generate_agent_turn(
                        provider,
                        model,
                        messages_json,
                        tools_json,
                        max_tokens,
                        system_text,
                    )
                    .await;
            }
        };

        let mut fallback_required = false;

        let mut result: Option<Value> = None;
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            loop {
                if let Some(pos) = buf.find("\n\n") {
                    let line = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            match json.get("type").and_then(Value::as_str) {
                                Some("token") => {
                                    if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                                        on_token(delta.to_string());
                                    }
                                }
                                Some("done") => {
                                    result = json.get("result").cloned();
                                }
                                Some("error") => {
                                    let msg = json
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("errore sconosciuto");
                                    if msg.contains("non supporta lo streaming") {
                                        // Provider non supporta streaming — fallback a gRPC
                                        tracing::debug!(
                                            "provider {} non supporta streaming, fallback a gRPC",
                                            provider
                                        );
                                        fallback_required = true;
                                        break;
                                    }
                                    // Estrai retry_after dal metadata (header Retry-After) per
                                    // cooldown dinamico nel loop agente. Lo incastoniamo come
                                    // marcatore `[retry_after=N]` nel messaggio.
                                    let retry_after = json
                                        .get("metadata")
                                        .and_then(|m| m.get("retry_after_seconds"))
                                        .and_then(Value::as_u64);
                                    let error_class = json
                                        .get("metadata")
                                        .and_then(|m| m.get("error_class"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    let marker_retry = retry_after
                                        .map(|n| format!(" [retry_after={}]", n))
                                        .unwrap_or_default();
                                    let marker_class = if !error_class.is_empty() {
                                        format!(" [error_class={}]", error_class)
                                    } else {
                                        String::new()
                                    };
                                    anyhow::bail!(
                                        "brain stream error: {}{}{}",
                                        msg,
                                        marker_retry,
                                        marker_class
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    break;
                }
            }
        }

        if fallback_required {
            return self
                .generate_agent_turn(
                    provider,
                    model,
                    messages_json,
                    tools_json,
                    max_tokens,
                    system_text,
                )
                .await;
        }

        result.ok_or_else(|| anyhow::anyhow!("nessun risultato dal brain stream"))
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
    /// (vedi ``retry_after_seconds`` letto a riga 263-266 di questo file).
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
