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
    CountTokensResponse, ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse,
    LlmStreamChunk, SensitivityTier, TranscribeRequest, TranscribeResponse, TtsRequest,
    TtsResponse, VideoGenRequest, VideoGenResponse,
};

/// Stream di chunk prodotto da un provider in modalita' streaming. Ogni
/// elemento e' un chunk valido oppure un errore di trasporto/parsing.
pub type ChunkStream = BoxStream<'static, anyhow::Result<LlmStreamChunk>>;

/// Metadati di un modello esposti dall'API di listing del provider.
///
/// `context_window` e' la finestra di contesto DICHIARATA DAL PROVIDER
/// (es. `max_context_length` nel dialetto Mistral): `None` quando l'API non la
/// espone. Mai inventata a valle (regola H: il catalogo scrive 0 = ignota,
/// non un placeholder): l'incidente sub-agente 2026-07-06 nasce da un default
/// schema 8192 preso per finestra reale dal predictive cap.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelMeta {
    /// Id canonico del modello (es. "mistral-medium-3").
    pub id: String,
    /// Finestra di contesto in token dichiarata dal provider, se esposta.
    pub context_window: Option<i64>,
    /// Tetto di output in token DICHIARATO DAL PROVIDER nel listing
    /// (`outputTokenLimit` di Google; `top_provider.max_completion_tokens` di
    /// OpenRouter, misurato sul body vero il 16/08/2026): `None` quando l'API
    /// non lo dichiara. MAI inventato a valle (regole G/H): un tetto stretto
    /// indovinato e' cio' che produce il turno vuoto fatturato (vedi
    /// [[tetto-di-output]] e [[dichiarazione_fornitore]] in CLAUDE.md).
    pub output_token_limit: Option<i64>,
}

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

    /// Autodiscovery live CON METADATI ([`ModelMeta`]): id + finestra di
    /// contesto dichiarata dal provider quando l'API la espone.
    ///
    /// Default impl: delega a [`Self::list_models`] con `context_window=None`
    /// (il provider non dichiara la finestra nel suo listing). I provider il cui
    /// dialetto la espone (es. Mistral `max_context_length`) sovrascrivono.
    /// Punto unico a valle: il catalog sync scrive la finestra SOLO se
    /// dichiarata, altrimenti 0 = ignota (regola G/H, mai placeholder).
    async fn list_models_meta(&self) -> anyhow::Result<Vec<ModelMeta>> {
        Ok(self
            .list_models()
            .await?
            .into_iter()
            .map(|id| ModelMeta {
                id,
                context_window: None,
                output_token_limit: None,
            })
            .collect())
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

    /// Se il provider sa dire QUANTI token d'ingresso costerebbe una richiesta,
    /// prima di mandarla.
    ///
    /// Default impl: `false`. Lo implementa chi espone un endpoint di conteggio
    /// (anthropic: `POST /messages/count_tokens`, gratuito). Non e' una stima
    /// nostra e non la sostituisce: e' il conteggio del tokenizzatore del
    /// FORNITORE, l'unico che corrisponde a cio' che verra' fatturato. Gemello
    /// di [`Self::supports_image_gen`].
    fn supports_count_tokens(&self) -> bool {
        false
    }

    /// Chiede al fornitore quanti token d'ingresso vale questa richiesta.
    ///
    /// Default impl: errore esplicito (regola H, come [`Self::generate_image`]):
    /// un provider che non dichiara `supports_count_tokens()` non deve mai
    /// essere chiamato, e se accade deve fallire visibilmente. MAI uno zero:
    /// «non lo so» e «zero token» sono due cose diverse, e la seconda e' una
    /// misura falsa (regola Q).
    async fn count_tokens(&self, _req: &LlmRequest) -> anyhow::Result<CountTokensResponse> {
        anyhow::bail!("{}: conteggio token non supportato", self.name())
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
