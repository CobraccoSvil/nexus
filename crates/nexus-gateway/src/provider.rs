//! Trait comune a tutti i provider LLM.
//!
//! Corrisponde all'interfaccia `LLMProvider` di
//! `packages/llm-gateway/src/types.ts`. Ogni provider concreto (OpenAI,
//! Anthropic, Mistral, DeepSeek, Google, vLLM) implementa questo trait; la
//! composizione (non l'ereditarieta') e' il meccanismo di riuso (regola L):
//! i provider OpenAI-compatibili delegheranno a un client condiviso.

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::types::{
    ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse, LlmStreamChunk, SensitivityTier,
    TranscribeRequest, TranscribeResponse, TtsRequest, TtsResponse, VideoGenRequest,
    VideoGenResponse,
};

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

    /// Autodiscovery live: lista dei nomi modello esposti dall'API del provider
    /// in questo momento. Diverso dal catalog DB (`ai_price_catalog`): interroga
    /// l'API live. Usato dall'unificazione del catalog sync sul gateway (punto
    /// unico, regola L): il gateway puo' listare TUTTI i provider, Vertex incluso
    /// (ha gia' l'auth Service Account in `gcp_auth`), eliminando la delega al
    /// brain per Google.
    ///
    /// Default impl: `Ok(vec![])` per i provider che non espongono un endpoint di
    /// listing (es. onprem statici); chi lo supporta lo sovrascrive. Su errore di
    /// rete/auth ritorna `Err` (il chiamante aggrega best-effort).
    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    /// Se il provider supporta la generazione di immagini (text-to-image).
    ///
    /// Default impl: `false`. I provider che la implementano lo sovrascrivono e
    /// forniscono [`Self::generate_image`]. Permette al routing image-gen di
    /// scegliere solo i provider capaci (regola H: niente delega silenziosa a un
    /// provider che non genera immagini).
    fn supports_image_gen(&self) -> bool {
        false
    }

    /// Genera una o piu' immagini dal `prompt`. Default impl: errore esplicito
    /// (regola H, come `create_batch_google` ritorna 501): un provider che non
    /// dichiara `supports_image_gen()` non deve mai essere chiamato; se accade,
    /// fallisce visibilmente invece di restituire un risultato vuoto.
    async fn generate_image(&self, _req: &ImageGenRequest) -> anyhow::Result<ImageGenResponse> {
        anyhow::bail!("{}: image-generation non supportata", self.name())
    }

    /// Se il provider supporta la trascrizione audio (speech-to-text).
    ///
    /// Default impl: `false`. I provider che la implementano lo sovrascrivono e
    /// forniscono [`Self::transcribe_audio`]. Permette al routing audio-in di
    /// scegliere solo i provider capaci (regola H: niente delega silenziosa a un
    /// provider che non trascrive audio). Gemello di [`Self::supports_image_gen`].
    fn supports_audio_in(&self) -> bool {
        false
    }

    /// Trascrive l'audio della richiesta. Default impl: errore esplicito (regola
    /// H, come [`Self::generate_image`]): un provider che non dichiara
    /// `supports_audio_in()` non deve mai essere chiamato; se accade, fallisce
    /// visibilmente invece di restituire un risultato vuoto.
    async fn transcribe_audio(
        &self,
        _req: &TranscribeRequest,
    ) -> anyhow::Result<TranscribeResponse> {
        anyhow::bail!("{}: transcription non supportata", self.name())
    }

    /// Se il provider supporta la sintesi vocale (text-to-speech).
    ///
    /// Default impl: `false`. I provider che la implementano lo sovrascrivono e
    /// forniscono [`Self::text_to_speech`]. Permette al routing audio-out di
    /// scegliere solo i provider capaci (regola H: niente delega silenziosa a un
    /// provider che non produce audio). Gemello di [`Self::supports_audio_in`].
    fn supports_audio_out(&self) -> bool {
        false
    }

    /// Sintetizza in audio il testo della richiesta. Default impl: errore
    /// esplicito (regola H, come [`Self::transcribe_audio`]): un provider che non
    /// dichiara `supports_audio_out()` non deve mai essere chiamato; se accade,
    /// fallisce visibilmente invece di restituire un risultato vuoto.
    async fn text_to_speech(&self, _req: &TtsRequest) -> anyhow::Result<TtsResponse> {
        anyhow::bail!("{}: text-to-speech non supportata", self.name())
    }

    /// Se il provider supporta la generazione di video (text-to-video).
    ///
    /// Default impl: `false`. I provider che la implementano lo sovrascrivono e
    /// forniscono [`Self::generate_video`]. Permette al routing video-gen di
    /// scegliere solo i provider capaci (regola H: niente delega silenziosa a un
    /// provider che non genera video). Gemello di [`Self::supports_image_gen`].
    fn supports_video_gen(&self) -> bool {
        false
    }

    /// Genera un video dal `prompt`. A differenza di image/audio il backend e'
    /// ASYNC long-running: l'implementazione incapsula start + poll-loop e ritorna
    /// solo quando il video e' pronto (o al timeout). Default impl: errore
    /// esplicito (regola H, come [`Self::generate_image`]): un provider che non
    /// dichiara `supports_video_gen()` non deve mai essere chiamato; se accade,
    /// fallisce visibilmente invece di restituire un risultato vuoto.
    async fn generate_video(&self, _req: &VideoGenRequest) -> anyhow::Result<VideoGenResponse> {
        anyhow::bail!("{}: video-generation non supportata", self.name())
    }
}
