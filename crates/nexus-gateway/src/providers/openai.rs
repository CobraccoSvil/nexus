//! Provider OpenAI.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts` + parita' con
//! `brain/providers/openai_provider.py`. Delega il trasporto al client condiviso
//! [`OpenAiCompatClient`] (composizione, regola L); aggiunge la detection
//! o-series (per nome modello) che cambia il dialetto reasoning: i modelli
//! reasoning (o1/o3/o4, gpt-5*, gpt-4.5*) non accettano temperatura e ammettono
//! `reasoning_effort`. Il tetto di output in `max_completion_tokens` NON e' del
//! dialetto: OpenAI ha deprecato `max_tokens` per l'INTERO parco, chat compresi,
//! e lo dichiara il costruttore (vedi
//! [`OpenAiCompatClient::with_tetto_su_completion`]).

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use nexus_cache::TtlCache;
use reqwest::Client;
use sqlx::PgPool;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{
    ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier,
    TranscribeRequest, TranscribeResponse, TtsRequest, TtsResponse,
};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint OpenAI di default. La `base_url` resta un parametro del costruttore
/// (override per gateway compatibili); questo valore e' solo il default quando
/// il chiamante non ne passa una.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Chiave settings (regola G) del livello di reasoning per i modelli o-series.
/// Valori ammessi dall'API: `low`/`medium`/`high`. Assente => non si invia
/// `reasoning_effort` (l'API usa il default del modello): nessun hardcoded.
const REASONING_EFFORT_SETTING: &str = "providers.openai.reasoning_effort";

/// TTL della cache settings (60s, come gli altri provider).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Famiglie reasoning (gpt-5*, gpt-4.5*) coperte per PREFISSO: ogni release
/// (gpt-5.1, gpt-5-mini, ...) e' gestita senza elencarla a mano (parita' col
/// Python `_is_o_series` ~104, regola G: famiglia strutturale, non nome esatto).
const O_SERIES_FAMILY_PREFIXES: &[&str] = &["gpt-5", "gpt-4.5"];

/// Basi o-series (o1/o3/o4) trattate per match esatto o `base-...` (parita' col
/// Python `_O_SERIES_MODELS`: `m == base || m.starts_with(base + "-")`). Non un
/// `starts_with` puro, che catturerebbe per errore nomi come `o1abc`.
const O_SERIES_BASES: &[&str] = &["o1", "o3", "o4"];

/// True se il modello richiede il dialetto reasoning o-series. Case-insensitive.
/// Parita' fedele col Python: prefisso per le famiglie gpt-5/gpt-4.5, match
/// esatto o `base-` per o1/o3/o4.
fn is_o_series(model: &str) -> bool {
    let m = model.to_lowercase();
    if O_SERIES_FAMILY_PREFIXES.iter().any(|p| m.starts_with(p)) {
        return true;
    }
    O_SERIES_BASES
        .iter()
        .any(|b| m == *b || m.starts_with(&format!("{b}-")))
}

pub struct OpenAiProvider {
    client: OpenAiCompatClient,
    db: Option<PgPool>,
    reasoning_effort: TtlCache<(), Option<String>>,
}

impl OpenAiProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). L'effort
    /// reasoning non sara' leggibile dai settings: si usa il default del modello.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere `reasoning_effort` dai
    /// settings (regola G). `base_url` opzionale (default OpenAI ufficiale); la
    /// `api_key` e' iniettata dal chiamante (regola F: niente segreti nel codice).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            // CACHE: `prompt_cache_key` e' un parametro dell'API OpenAI, il
            // dialetto da cui Mistral lo eredita. Ma qui NON e' la differenza
            // fra cacheare e non cacheare, come invece e' su Mistral: misurato
            // il 29/07/2026, gpt-4o-mini riusa 11.392 token su 11.469 anche
            // SENZA la chiave, e con la chiave il risultato e' identico.
            // La si manda lo stesso perche' e' l'hint di affinita' che il
            // provider documenta: il riuso automatico dipende da quale nodo
            // serve la richiesta, e senza chiave quella scelta e' casuale. Che
            // il caso esista non e' un'ipotesi: nello stesso disegno di prova
            // mistral-small, che pure cachea da solo, ha riusato il prefisso
            // una volta su due. Rischio nullo (campo nativo del dialetto),
            // guadagno atteso sui carichi distribuiti.
            // TETTO: `max_tokens` e' deprecato dal PROVIDER per l'intera
            // famiglia (doc API reference: "deprecated in favor of
            // max_completion_tokens"), non dai soli modelli reasoning — anche
            // i chat (gpt-4o*) lo accettano. Percio' la dichiarazione sta sul
            // client e non sul dialetto: finche' viveva nel dialetto, un
            // modello non-reasoning partiva col campo deprecato.
            client: OpenAiCompatClient::new(http, base_url, api_key, "openai")
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey)
                .with_tetto_su_completion(),
            db,
            reasoning_effort: TtlCache::new(SETTINGS_TTL),
        }
    }

    /// Livello reasoning dai settings (cache TTL 60s). `None` => chiave assente o
    /// DB irraggiungibile: non si invia `reasoning_effort` (default del modello).
    async fn configured_effort(&self) -> Option<String> {
        if let Some(e) = self.reasoning_effort.get(&()) {
            return e;
        }
        let db = self.db.as_ref()?;
        let value = nexus_auth::get_setting(db, REASONING_EFFORT_SETTING)
            .await
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        self.reasoning_effort.insert((), value.clone());
        value
    }

    /// Risolve il dialetto reasoning per la richiesta: o-series se il nome del
    /// modello lo richiede, altrimenti dialetto base. L'`effort` arriva dai
    /// settings solo per o-series.
    async fn resolve(&self, req: &LlmRequest) -> ResolvedReasoning {
        if !is_o_series(&req.model) {
            return ResolvedReasoning::none();
        }
        ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            // o-series e' sempre in reasoning mode (non disattivabile via param):
            // `enabled` informativo, il comportamento e' guidato dal dialetto.
            enabled: true,
            effort: self.configured_effort().await,
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        128_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let reasoning = self.resolve(req).await;
        self.client.complete_with_reasoning(req, &reasoning).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = self.resolve(req).await;
        self.client.stream_with_reasoning(req, &reasoning).await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    fn supports_image_gen(&self) -> bool {
        true
    }

    /// Delega al trasporto condiviso (`POST /images/generations`): stesso client
    /// HTTP/auth della chat (regola L). Il modello (es. `gpt-image-1`) arriva dal
    /// chiamante (regola G).
    async fn generate_image(&self, req: &ImageGenRequest) -> anyhow::Result<ImageGenResponse> {
        self.client
            .images_generations(&req.model, &req.prompt, req.n, req.size.as_deref())
            .await
    }

    fn supports_audio_in(&self) -> bool {
        true
    }

    /// Decodifica l'audio base64 e delega al trasporto condiviso
    /// (`POST /audio/transcriptions`, multipart): stesso client HTTP/auth della
    /// chat (regola L). Il modello (es. `whisper-1`, `gpt-4o-transcribe`) arriva
    /// dal chiamante (regola G). Il filename multipart deriva dal mime dichiarato
    /// (estensione) cosi' OpenAI inferisce il formato dell'audio.
    async fn transcribe_audio(
        &self,
        req: &TranscribeRequest,
    ) -> anyhow::Result<TranscribeResponse> {
        let audio_bytes = B64
            .decode(req.audio_base64.trim())
            .map_err(|e| anyhow::anyhow!("audio base64 non valido: {e}"))?;
        let filename = audio_filename(req.mime.as_deref());
        self.client
            .transcribe(&req.model, audio_bytes, &filename, req.language.as_deref())
            .await
    }

    fn supports_audio_out(&self) -> bool {
        true
    }

    /// Delega al trasporto condiviso (`POST /audio/speech`, JSON in -> bytes out):
    /// stesso client HTTP/auth della chat (regola L). Il modello (es.
    /// `gpt-4o-mini-tts`, `tts-1`) arriva dal chiamante (regola G). I bytes audio
    /// vengono codificati base64 per il contratto JSON del gateway.
    async fn text_to_speech(&self, req: &TtsRequest) -> anyhow::Result<TtsResponse> {
        let start = std::time::Instant::now();
        let (bytes, mime) = self
            .client
            .speech(
                &req.model,
                &req.input,
                req.voice.as_deref(),
                req.response_format.as_deref(),
            )
            .await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(TtsResponse {
            audio_base64: B64.encode(&bytes),
            mime,
            model_used: req.model.clone(),
            provider_used: self.name().to_string(),
            latency_ms,
        })
    }
}

/// Nome file multipart per l'audio, derivato dal MIME dichiarato. OpenAI usa
/// l'estensione del `file_name` per inferire il formato; senza mime usiamo `.mp3`
/// (formato piu' comune). Funzione PURA (testabile). Niente nome hardcoded di
/// business: e' solo l'estensione tecnica del file multipart.
fn audio_filename(mime: Option<&str>) -> String {
    let ext = match mime.map(|m| m.trim().to_lowercase()).as_deref() {
        Some("audio/mpeg" | "audio/mp3") => "mp3",
        Some("audio/wav" | "audio/x-wav") => "wav",
        Some("audio/mp4" | "audio/x-m4a" | "audio/m4a") => "m4a",
        Some("audio/ogg" | "audio/opus") => "ogg",
        Some("audio/flac" | "audio/x-flac") => "flac",
        Some("audio/webm") => "webm",
        _ => "mp3",
    };
    format!("audio.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(Client::new(), "sk-test", None)
    }

    fn richiesta(model: &str) -> LlmRequest {
        LlmRequest {
            model: model.to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error: None,
            }],
            temperature: Some(0.5),
            max_tokens: Some(64),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".to_string(),
                user_id: "u".to_string(),
                request_id: "r".to_string(),
                sensitivity_tier: 0,
                feature: "f".to_string(),
            },
            run_timeout_secs: None,
        }
    }

    /// La deprecazione di `max_tokens` e' del PROVIDER, non del dialetto: anche
    /// un modello chat non-reasoning parte con `max_completion_tokens`. Prima
    /// il tetto viveva nel predicato sul dialetto (`OpenAiReasoning | Kimi`) e
    /// un `gpt-4o-mini` — dialetto base — usciva col campo che la doc OpenAI
    /// dichiara deprecato.
    ///
    /// Attraversa `resolve` e `corpo_della_richiesta` REALI (regola O): e' la
    /// coppia dialetto+client che parte in produzione, non una composta a mano.
    /// Il verso opposto (mistral resta su `max_tokens`) e' il test gemello in
    /// mistral.rs: insieme fissano che la dichiarazione e' per-fornitore.
    ///
    /// MUTAZIONE: togliere `.with_tetto_su_completion()` dal costruttore, o
    /// riportare il tetto sul dialetto in `build_request_body` -> gpt-4o-mini
    /// risolve dialetto base e il body torna a `max_tokens`: rosso.
    #[tokio::test]
    async fn anche_un_modello_chat_porta_max_completion_tokens() {
        let p = provider();
        let req = richiesta("gpt-4o-mini");
        let reasoning = p.resolve(&req).await;
        assert_eq!(
            reasoning.dialect,
            ReasoningDialect::None,
            "premessa: gpt-4o-mini non e' o-series"
        );
        let corpo =
            serde_json::to_value(p.client.corpo_della_richiesta(&req, false, &reasoning).await)
                .expect("serializza");
        assert_eq!(corpo["max_completion_tokens"], 64);
        assert!(corpo.get("max_tokens").is_none());
        // La temperatura resta materia del DIALETTO: un chat la manda ancora.
        assert_eq!(corpo["temperature"], 0.5);
    }

    /// OpenAI cachea anche senza la chiave (misurato: 11.392 token su 11.469),
    /// quindi qui non e' un rimedio a un difetto ma l'hint di affinita' che il
    /// provider documenta. Resta dichiarato perche' il nodo che serve la
    /// richiesta altrimenti lo si sceglie a caso: vedi il costruttore.
    #[test]
    fn dichiara_la_chiave_di_cache_del_proprio_dialetto() {
        assert_eq!(
            provider().client.cache_keying(),
            PromptCacheKeying::RequiresKey
        );
    }

    #[test]
    fn capacita_dichiarate() {
        let p = provider();
        assert_eq!(p.name(), "openai");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 128_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
        // Capability media: OpenAI genera immagini, trascrive e sintetizza audio.
        assert!(p.supports_image_gen());
        assert!(p.supports_audio_in());
        assert!(p.supports_audio_out());
    }

    #[test]
    fn audio_filename_dal_mime() {
        assert_eq!(audio_filename(Some("audio/mpeg")), "audio.mp3");
        assert_eq!(audio_filename(Some("audio/wav")), "audio.wav");
        assert_eq!(audio_filename(Some("audio/mp4")), "audio.m4a");
        assert_eq!(audio_filename(Some("audio/ogg")), "audio.ogg");
        assert_eq!(audio_filename(Some("audio/flac")), "audio.flac");
        // Mime assente o sconosciuto -> default mp3.
        assert_eq!(audio_filename(None), "audio.mp3");
        assert_eq!(audio_filename(Some("application/octet-stream")), "audio.mp3");
    }

    #[test]
    fn detection_o_series_per_famiglia() {
        // Reasoning family per prefisso (parita' col Python).
        assert!(is_o_series("o1"));
        assert!(is_o_series("o1-mini"));
        assert!(is_o_series("o3"));
        assert!(is_o_series("o4-mini"));
        assert!(is_o_series("gpt-5"));
        assert!(is_o_series("gpt-5.1"));
        assert!(is_o_series("gpt-5-nano"));
        assert!(is_o_series("gpt-4.5-preview"));
        assert!(is_o_series("GPT-5")); // case-insensitive
                                       // Chat non-reasoning: dialetto base.
        assert!(!is_o_series("gpt-4o"));
        assert!(!is_o_series("gpt-4o-mini"));
        assert!(!is_o_series("gpt-4.1"));
    }
}
