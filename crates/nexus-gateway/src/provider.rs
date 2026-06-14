//! Trait comune a tutti i provider LLM.
//!
//! Corrisponde all'interfaccia `LLMProvider` di
//! `packages/llm-gateway/src/types.ts`. Ogni provider concreto (OpenAI,
//! Anthropic, Mistral, DeepSeek, Google, vLLM) implementa questo trait; la
//! composizione (non l'ereditarieta') e' il meccanismo di riuso (regola L):
//! i provider OpenAI-compatibili delegheranno a un client condiviso.

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::types::{LlmRequest, LlmResponse, LlmStreamChunk, SensitivityTier};

/// Stream di chunk prodotto da un provider in modalita' streaming. Ogni
/// elemento e' un chunk valido oppure un errore di trasporto/parsing.
pub type ChunkStream = BoxStream<'static, anyhow::Result<LlmStreamChunk>>;

/// Contratto di un provider LLM. `Send + Sync` perche' i provider vengono
/// condivisi tra task tokio dietro `Arc`.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Nome canonico del provider (es. "openai").
    fn name(&self) -> &str;

    /// Se il provider supporta le tool-call.
    fn supports_tools(&self) -> bool;

    /// Se il provider supporta lo streaming SSE.
    fn supports_streaming(&self) -> bool;

    /// Limite di contesto in token del modello principale.
    fn max_context_tokens(&self) -> u32;

    /// Tier di sensibilita' ammessi per questo provider.
    fn tier_compatibility(&self) -> &[SensitivityTier];

    /// Esegue una completion non-streaming.
    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse>;

    /// Esegue una completion in streaming, ritornando uno stream di chunk.
    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream>;

    /// Probe di salute: `false` se il provider e' down o in billing error.
    async fn healthcheck(&self) -> bool;
}
