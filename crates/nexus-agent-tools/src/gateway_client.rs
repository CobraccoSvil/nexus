//! Client HTTP minimale del Nexus LLM Gateway per i tool di `nexus-agent-tools`.
//!
//! `nexus-agent-tools` e' un crate INFERIORE a `mcp-core` nella gerarchia del
//! workspace: non puo' dipendere da `mcp-core::nexus_gateway::NexusGatewayClient`
//! (creerebbe un ciclo). Questo modulo espone un client ridotto al solo
//! sottoinsieme che serve ai tool del crate (oggi: la chiamata multimodale
//! vision), parlando lo STESSO contratto wire del gateway (`POST /v1/complete`,
//! stessi nomi di campo serde di `nexus-gateway::types`). NON re-implementa la
//! logica di routing/cooldown/privacy: quella vive nel gateway (regola L,
//! punto unico). Qui si fa solo il trasporto HTTP + il pin del provider deciso
//! a monte via routing matrix DB (regola G: nessun modello hardcoded).
//!
//! URL e token sono risolti dal DB/env senza panicare (a differenza di
//! `nexus_auth::resolve_port`, pensato per l'avvio): un purpose non configurato
//! o il gateway giu' devono degradare a errore restituito al modello, non far
//! crashare il processo.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use nexus_auth::get_setting_checked;

/// Porta di default del gateway, coerente con `settings.nexus_gateway_port`
/// (mig 0239). Usata SOLO come rete di sicurezza se la lettura del setting
/// fallisce: documentata, non un "magic fallback" di modello (regola G).
const GATEWAY_DEFAULT_PORT: u16 = 4060;
/// Timeout HTTP della chiamata al gateway. Vision puo' essere lento (immagini
/// grandi, cold start del provider).
const GATEWAY_HTTP_TIMEOUT_SECS: u64 = 60;

/// Chiave settings (regola G) del timeout del poll-loop video-gen lato gateway,
/// in secondi. La stessa letta dal provider Google nel gateway (mig 0482): unica
/// fonte di verita' condivisa. Il client usa questo valore per dimensionare il
/// proprio timeout HTTP >= del poll-loop (regola H: il client non stacca prima).
const VIDEO_POLL_TIMEOUT_SETTING: &str = "media.video.poll_timeout_s";

/// Timeout del poll-loop video usato SOLO se il setting e' illeggibile dal DB
/// (fallback graceful documentato, regola G/H). Allineato al default '300' della
/// mig 0482.
const VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK: u64 = 300;

/// Margine (secondi) aggiunto al poll-timeout per il timeout HTTP del client
/// video-gen: copre connessione + ultimo poll + download del video. Garantisce
/// che il client non stacchi PRIMA del poll-loop lato gateway (regola H).
const VIDEO_HTTP_TIMEOUT_MARGIN_SECS: u64 = 30;

/// Metadati di tracciamento/tenancy della richiesta (`RequestMetadata` del
/// gateway). I tool interni valorizzano solo `feature`; il resto va a default
/// (stringhe vuote, tier 0), come gli altri call site interni di mcp-core
/// (es. `intent_classifier`).
#[derive(Serialize, Default)]
struct GwMetadata {
    tenant_id: String,
    user_id: String,
    request_id: String,
    sensitivity_tier: u8,
    feature: String,
}

/// Corpo di `POST /v1/complete` (sottoinsieme usato dai tool del crate).
#[derive(Serialize)]
struct GwRequestBody {
    model: String,
    /// Messaggi della conversazione. `content` e' un [`Value`] perche' il
    /// contratto del gateway (`MessageContent` untagged) accetta sia una
    /// stringa sia una lista di blocchi `{type, ...}` (text/image_url).
    messages: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Pin esplicito del provider deciso a monte (routing matrix DB): il
    /// gateway esegue ESATTAMENTE quel provider senza secondo routing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Risposta di `POST /v1/complete` (solo i campi consumati dai tool del crate).
/// Deserializzazione tollerante: i campi extra del contratto sono ignorati.
#[derive(Deserialize)]
struct GwResponseBody {
    #[serde(default)]
    content: String,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una chiamata multimodale al gateway: testo grezzo + modello usato.
pub struct GwVisionResult {
    /// Testo grezzo della risposta del modello (da parsare dal chiamante).
    pub content: String,
    /// Etichetta `provider/model` realmente eseguita dal gateway, per
    /// trasparenza verso il modello agente.
    pub model_used: String,
}

/// Esegue una chiamata multimodale (testo + immagini) al gateway pinnando il
/// provider deciso a monte. `Err(messaggio)` se URL/token non risolvibili, il
/// gateway e' irraggiungibile o risponde con errore: il chiamante (tool vision)
/// rigira l'errore al modello, che ricade su un altro tool (fallback onesto).
///
/// - `provider`/`model`: risolti dal purpose via routing (regola G).
/// - `content_blocks`: lista di blocchi `{type:"text"|"image_url", ...}` gia'
///   costruita dal chiamante (data URI base64 per le immagini).
/// - `feature`: etichetta di tracciamento (es. nome del purpose).
pub async fn gateway_vision_complete(
    db: &PgPool,
    provider: &str,
    model: &str,
    content_blocks: Value,
    max_tokens: u32,
    feature: &str,
) -> Result<GwVisionResult, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let body = GwRequestBody {
        // Il gateway accetta "provider/model" come pin esplicito; valorizziamo
        // anche `pin_provider` per evitare un secondo routing divergente.
        model: format!("{provider}/{model}"),
        messages: json!([
            {
                "role": "user",
                "content": content_blocks,
            }
        ]),
        max_tokens: Some(max_tokens),
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/complete"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway LLM ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway non valida: {e}"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };

    Ok(GwVisionResult {
        content: parsed.content,
        model_used,
    })
}

// ── Image generation (delega al gateway: POST /v1/images/generations) ────────

/// Corpo di `POST /v1/images/generations` (sottoinsieme usato dai tool del
/// crate). Wire-compatibile con `GwImageRequest` di mcp-core::nexus_gateway /
/// `ImageGenRequest` del gateway: stesso contratto serde (`model`, `prompt`,
/// `n`, `size`, `pin_provider`, `metadata`). NON dipende da quel tipo per non
/// introdurre un ciclo di crate (vedi nota di modulo).
#[derive(Serialize)]
struct GwImageRequestBody {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    /// Pin esplicito del provider deciso a monte (routing per capability nel
    /// purpose `generate_image`): il gateway esegue ESATTAMENTE quel provider
    /// senza secondo routing (parita' con `gateway_vision_complete`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Una immagine generata, come la riporta il gateway. Deserializzazione
/// tollerante: almeno uno tra `b64_json` e `url` e' valorizzato.
#[derive(Deserialize, Default)]
struct GwGeneratedImageBody {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    mime: Option<String>,
}

/// Risposta di `POST /v1/images/generations` (solo i campi consumati dal tool).
#[derive(Deserialize)]
struct GwImageResponseBody {
    #[serde(default)]
    images: Vec<GwGeneratedImageBody>,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una generazione immagine al gateway: la prima immagine prodotta
/// (base64 inline) + l'etichetta provider/model realmente eseguita.
pub struct GwImageOut {
    /// Base64 (`b64_json`) della prima immagine. `None` se il provider ha
    /// risposto solo con una `url` temporanea (il chiamante non puo' salvarla
    /// path-safe: errore esplicito a monte).
    pub b64_json: Option<String>,
    /// URL temporanea della prima immagine, se il provider non ha inviato il
    /// base64 inline. Riportata per trasparenza al modello agente.
    pub url: Option<String>,
    /// MIME dichiarato dal provider (es. `image/png`), se presente.
    pub mime: Option<String>,
    /// Etichetta `provider/model` realmente eseguita dal gateway.
    pub model_used: String,
}

/// Genera un'immagine via il gateway pinnando il provider deciso a monte dal
/// purpose `generate_image` (regola G: provider/model gia' risolti, nessun
/// modello hardcoded qui). Gemella di [`gateway_vision_complete`]: riusa la
/// risoluzione porta/token del crate (regola L, niente routing duplicato) e
/// rigira l'errore HTTP al chiamante (regola H: niente fallback inventato; se
/// il provider non genera immagini il gateway risponde 5xx col motivo).
///
/// - `provider`/`model`: risolti dal purpose via routing.
/// - `size`: dimensione richiesta (es. `1024x1024`), opzionale.
/// - `feature`: etichetta di tracciamento (di norma il nome del purpose).
pub async fn gateway_image_generate(
    db: &PgPool,
    provider: &str,
    model: &str,
    prompt: &str,
    size: Option<String>,
    feature: &str,
) -> Result<GwImageOut, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let body = GwImageRequestBody {
        model: model.to_string(),
        prompt: prompt.to_string(),
        // Una sola immagine: il tool salva un file per chiamata.
        n: Some(1),
        size,
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/images/generations"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/images/generations ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwImageResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway image-gen non valida: {e}"))?;

    let first = parsed
        .images
        .into_iter()
        .next()
        .ok_or_else(|| format!("il gateway non ha restituito immagini ({provider}/{model})"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };

    Ok(GwImageOut {
        b64_json: first.b64_json,
        url: first.url,
        mime: first.mime,
        model_used,
    })
}

// ── Video generation (delega al gateway: POST /v1/videos) ────────────────────

/// Corpo di `POST /v1/videos` (sottoinsieme usato dai tool del crate).
/// Wire-compatibile con `GwVideoRequest` di mcp-core::nexus_gateway /
/// `VideoGenRequest` del gateway: stesso contratto serde (`model`, `prompt`,
/// `duration_seconds`, `pin_provider`, `metadata`). NON dipende da quel tipo per
/// non introdurre un ciclo di crate (vedi nota di modulo).
#[derive(Serialize)]
struct GwVideoRequestBody {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u32>,
    /// Pin esplicito del provider deciso a monte (routing per capability nel
    /// purpose `generate_video`): il gateway esegue ESATTAMENTE quel provider
    /// senza secondo routing (parita' con `gateway_image_generate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Risposta di `POST /v1/videos` (solo i campi consumati dal tool).
#[derive(Deserialize)]
struct GwVideoResponseBody {
    #[serde(default)]
    video_base64: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una generazione video al gateway: il video (base64 inline) oppure la
/// URL (gcsUri) + il MIME + l'etichetta provider/model realmente eseguita.
pub struct GwVideoOut {
    /// Base64 del video. `None` se il provider ha risposto solo con una `url`
    /// (gcsUri): il chiamante non puo' salvarla path-safe, la riporta con nota.
    pub video_base64: Option<String>,
    /// URL del video (gcsUri), se il provider non ha inviato i byte inline.
    pub url: Option<String>,
    /// MIME del video prodotto (es. `video/mp4`): per scegliere l'estensione.
    pub mime: String,
    /// Etichetta `provider/model` realmente eseguita dal gateway.
    pub model_used: String,
}

/// Genera un video via il gateway pinnando il provider deciso a monte dal purpose
/// `generate_video` (regola G: provider/model gia' risolti, nessun modello
/// hardcoded qui). Gemella di [`gateway_image_generate`] ma per il backend ASYNC
/// Veo: il gateway incapsula il poll-loop (start + poll con timeout DB-driven).
///
/// TIMEOUT (regola H): il client legge il setting `media.video.poll_timeout_s` dal
/// DB e dimensiona il proprio timeout HTTP a `poll_timeout + margine`, sempre >=
/// del poll-loop lato gateway: cosi' il client non stacca PRIMA che il video sia
/// pronto. Rigira l'errore HTTP al chiamante (niente fallback inventato; se il
/// provider non genera video o il poll va in timeout il gateway risponde col motivo).
///
/// - `provider`/`model`: risolti dal purpose via routing.
/// - `duration_seconds`: durata richiesta del video, opzionale.
/// - `feature`: etichetta di tracciamento (di norma il nome del purpose).
pub async fn gateway_generate_video(
    db: &PgPool,
    provider: &str,
    model: &str,
    prompt: &str,
    duration_seconds: Option<u32>,
    feature: &str,
) -> Result<GwVideoOut, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();
    let poll_timeout = resolve_video_poll_timeout(db).await;

    let body = GwVideoRequestBody {
        model: model.to_string(),
        prompt: prompt.to_string(),
        duration_seconds,
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    // Timeout HTTP >= poll-loop lato gateway (regola H): il client non stacca
    // prima che il video sia pronto.
    let http_timeout =
        Duration::from_secs(poll_timeout.saturating_add(VIDEO_HTTP_TIMEOUT_MARGIN_SECS));
    let client = reqwest::Client::builder()
        .timeout(http_timeout)
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/videos"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/videos ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwVideoResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway video-gen non valida: {e}"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };
    let mime = if parsed.mime.is_empty() {
        "video/mp4".to_string()
    } else {
        parsed.mime
    };

    Ok(GwVideoOut {
        video_base64: parsed.video_base64,
        url: parsed.url,
        mime,
        model_used,
    })
}

/// Risolve il timeout del poll-loop video dal setting `media.video.poll_timeout_s`
/// (mig 0482) in modo TOLLERANTE (no panic): se la lettura fallisce o il valore e'
/// invalido ricade sul fallback documentato (regola G/H: il LIMITE esiste sempre).
async fn resolve_video_poll_timeout(db: &PgPool) -> u64 {
    match get_setting_checked(db, VIDEO_POLL_TIMEOUT_SETTING).await {
        Ok(Some(raw)) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|s| *s > 0)
            .unwrap_or(VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK),
        _ => VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK,
    }
}

// ── Audio transcription (delega al gateway: POST /v1/audio/transcriptions) ───

/// Corpo di `POST /v1/audio/transcriptions` (sottoinsieme usato dai tool del
/// crate). Wire-compatibile con `GwTranscribeRequest` di mcp-core::nexus_gateway /
/// `TranscribeRequest` del gateway: stesso contratto serde (`model`,
/// `audio_base64`, `mime`, `language`, `pin_provider`, `metadata`). NON dipende da
/// quel tipo per non introdurre un ciclo di crate (vedi nota di modulo).
#[derive(Serialize)]
struct GwTranscribeRequestBody {
    model: String,
    audio_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    /// Pin esplicito del provider deciso a monte (routing per capability nel
    /// purpose `transcribe_audio`): il gateway esegue ESATTAMENTE quel provider
    /// senza secondo routing (parita' con `gateway_image_generate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Risposta di `POST /v1/audio/transcriptions` (solo i campi consumati dal tool).
#[derive(Deserialize)]
struct GwTranscribeResponseBody {
    #[serde(default)]
    text: String,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una trascrizione audio al gateway: il testo + l'etichetta
/// provider/model realmente eseguita.
pub struct GwTranscribeOut {
    /// Testo trascritto dall'audio.
    pub text: String,
    /// Etichetta `provider/model` realmente eseguita dal gateway.
    pub model_used: String,
}

/// Trascrive un audio via il gateway pinnando il provider deciso a monte dal
/// purpose `transcribe_audio` (regola G: provider/model gia' risolti, nessun
/// modello hardcoded qui). Gemella di [`gateway_image_generate`]: riusa la
/// risoluzione porta/token del crate (regola L, niente routing duplicato) e
/// rigira l'errore HTTP al chiamante (regola H: niente fallback inventato; se il
/// provider non trascrive il gateway risponde 5xx col motivo).
///
/// - `provider`/`model`: risolti dal purpose via routing.
/// - `audio_base64`: audio sorgente codificato base64 (costruito dal chiamante).
/// - `mime`: MIME dell'audio (per il nome multipart), opzionale.
/// - `language`: lingua ISO-639-1 dell'audio, opzionale.
/// - `feature`: etichetta di tracciamento (di norma il nome del purpose).
pub async fn gateway_transcribe_audio(
    db: &PgPool,
    provider: &str,
    model: &str,
    audio_base64: String,
    mime: Option<String>,
    language: Option<String>,
    feature: &str,
) -> Result<GwTranscribeOut, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let body = GwTranscribeRequestBody {
        model: model.to_string(),
        audio_base64,
        mime,
        language,
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/audio/transcriptions"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/audio/transcriptions ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwTranscribeResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway transcribe non valida: {e}"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };

    Ok(GwTranscribeOut {
        text: parsed.text,
        model_used,
    })
}

// ── Text-to-speech (delega al gateway: POST /v1/audio/speech) ────────────────

/// Corpo di `POST /v1/audio/speech` (sottoinsieme usato dai tool del crate).
/// Wire-compatibile con `GwTtsRequest` di mcp-core::nexus_gateway / `TtsRequest`
/// del gateway: stesso contratto serde (`model`, `input`, `voice`,
/// `response_format`, `pin_provider`, `metadata`). NON dipende da quel tipo per
/// non introdurre un ciclo di crate (vedi nota di modulo).
#[derive(Serialize)]
struct GwTtsRequestBody {
    model: String,
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<String>,
    /// Pin esplicito del provider deciso a monte (routing per capability nel
    /// purpose `text_to_speech`): il gateway esegue ESATTAMENTE quel provider
    /// senza secondo routing (parita' con `gateway_transcribe_audio`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Risposta di `POST /v1/audio/speech` (solo i campi consumati dal tool).
#[derive(Deserialize)]
struct GwTtsResponseBody {
    #[serde(default)]
    audio_base64: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una sintesi vocale al gateway: l'audio (base64) + il MIME prodotto +
/// l'etichetta provider/model realmente eseguita.
pub struct GwTtsOut {
    /// Audio sintetizzato codificato base64 (il chiamante lo decodifica e salva).
    pub audio_base64: String,
    /// MIME dell'audio prodotto (es. `audio/mpeg`): per scegliere l'estensione.
    pub mime: String,
    /// Etichetta `provider/model` realmente eseguita dal gateway.
    pub model_used: String,
}

/// Sintetizza in audio un testo via il gateway pinnando il provider deciso a monte
/// dal purpose `text_to_speech` (regola G: provider/model gia' risolti, nessun
/// modello hardcoded qui). Gemella di [`gateway_transcribe_audio`]: riusa la
/// risoluzione porta/token del crate (regola L, niente routing duplicato) e
/// rigira l'errore HTTP al chiamante (regola H: niente fallback inventato; se il
/// provider non sintetizza il gateway risponde 5xx col motivo).
///
/// - `provider`/`model`: risolti dal purpose via routing.
/// - `input`: testo da convertire in audio.
/// - `voice`: timbro del modello TTS (es. `alloy`), opzionale.
/// - `response_format`: formato audio (es. `mp3`, `wav`), opzionale.
/// - `feature`: etichetta di tracciamento (di norma il nome del purpose).
#[allow(clippy::too_many_arguments)]
pub async fn gateway_text_to_speech(
    db: &PgPool,
    provider: &str,
    model: &str,
    input: &str,
    voice: Option<String>,
    response_format: Option<String>,
    feature: &str,
) -> Result<GwTtsOut, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let body = GwTtsRequestBody {
        model: model.to_string(),
        input: input.to_string(),
        voice,
        response_format,
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/audio/speech"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/audio/speech ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwTtsResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway tts non valida: {e}"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };

    Ok(GwTtsOut {
        audio_base64: parsed.audio_base64,
        mime: parsed.mime,
        model_used,
    })
}

// ── Batch API (delega al gateway: POST /v1/batch + GET stato/risultati) ──────

/// Singola richiesta di un batch: `custom_id` + system/user prompt risolti dal
/// chiamante. Il `model` (per `LlmRequest` del gateway) e' deciso a monte dal
/// purpose risolto (regola G), comune a tutto il batch, e viene aggiunto qui.
pub struct GwBatchRequest {
    /// Identificatore univoco scelto dal chiamante (echeggiato in ogni risultato).
    pub custom_id: String,
    /// Prompt di sistema (opzionale): il gateway lo estrae come campo `system`
    /// del body Messages Anthropic.
    pub system: Option<String>,
    /// Prompt utente con il contenuto del file/snippet da analizzare.
    pub prompt: String,
}

/// Esito di una singola richiesta del batch, ricollegato al suo `custom_id`.
/// Esattamente uno tra `content` ed `error` e' valorizzato.
pub struct GwBatchResult {
    pub custom_id: String,
    /// Testo della risposta in caso di successo (vuoto se errore).
    pub content: String,
    /// Messaggio d'errore in caso di fallimento del singolo item.
    pub error: Option<String>,
}

/// Stato di avanzamento del batch ritornato da `GET /v1/batch/{provider}/{id}`.
/// `results` valorizzato solo quando `status == "ended"`.
pub struct GwBatchStatus {
    /// Stato canonico del gateway: `"in_progress"` | `"ended"`.
    pub status: String,
    /// Risultati per `custom_id` (presenti solo a batch terminato).
    pub results: Vec<GwBatchResult>,
}

impl GwBatchStatus {
    /// `true` se il batch e' terminato (risultati pronti).
    pub fn is_ended(&self) -> bool {
        self.status == "ended"
    }
}

/// Costruisce il client HTTP del gateway con il timeout batch (riuso del punto
/// unico di trasporto del crate, regola L).
fn batch_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))
}

/// Sottomette un batch al gateway (`POST /v1/batch`) pinnando provider+modello
/// decisi a monte dal purpose (regola G). Converte le richieste interne
/// (`custom_id`/`system`/`prompt`) nel contratto del gateway
/// (`{provider, requests:[{custom_id, model, messages, max_tokens}]}`) e ritorna
/// il `batch_id`. `Err(messaggio)` se URL/token non risolvibili, il gateway e'
/// irraggiungibile o risponde con errore (il chiamante rigira l'errore al modello).
///
/// Solo i provider con batch supportato dal gateway sono validi (oggi:
/// `anthropic`). Il gateway risponde 400/501 per gli altri: l'errore HTTP
/// risale onestamente al chiamante (niente fallback inventato).
pub async fn gateway_batch_submit(
    db: &PgPool,
    provider: &str,
    model: &str,
    requests: &[GwBatchRequest],
    max_tokens: u32,
) -> Result<String, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let items: Vec<Value> = requests
        .iter()
        .map(|r| {
            let mut messages: Vec<Value> = Vec::new();
            if let Some(sys) = &r.system {
                if !sys.is_empty() {
                    messages.push(json!({ "role": "system", "content": sys }));
                }
            }
            messages.push(json!({ "role": "user", "content": r.prompt }));
            json!({
                "custom_id": r.custom_id,
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
                // Metadata richiesto dal contratto LlmRequest del gateway.
                "metadata": { "feature": "batch_analyze_code" },
            })
        })
        .collect();
    let body = json!({ "provider": provider, "requests": items });

    let client = batch_http_client()?;
    let resp = client
        .post(format!("{base_url}/v1/batch"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/batch ha risposto HTTP {} (provider {provider}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway /v1/batch non valida: {e}"))?;
    parsed
        .get("batch_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("gateway /v1/batch senza campo 'batch_id': {parsed}"))
}

/// Recupera stato + (se terminato) risultati di un batch
/// (`GET /v1/batch/{provider}/{batch_id}`). Mappa `results[].response.content`
/// in `GwBatchResult.content` (parita' col contratto storico atteso dal tool);
/// gli item con `error` valorizzato producono `error`. `Err(messaggio)` su
/// gateway irraggiungibile o risposta non valida.
pub async fn gateway_batch_status(
    db: &PgPool,
    provider: &str,
    batch_id: &str,
) -> Result<GwBatchStatus, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let client = batch_http_client()?;
    let resp = client
        .get(format!("{base_url}/v1/batch/{provider}/{batch_id}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway /v1/batch/{provider}/{batch_id} ha risposto HTTP {}: {detail}",
            status.as_u16()
        ));
    }

    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway stato batch non valida: {e}"))?;
    Ok(parse_batch_status(&parsed))
}

/// Parsa la risposta JSON di `GET /v1/batch/{provider}/{id}` nel tipo del crate.
/// Funzione pura testabile: mappa `response.content` su `content`, altrimenti
/// `error`. Lo stato sconosciuto e' trattato come `in_progress` (fail-safe:
/// non si interrompe il polling prima del tempo).
fn parse_batch_status(body: &Value) -> GwBatchStatus {
    let status = body
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("in_progress")
        .to_string();
    let results: Vec<GwBatchResult> = body
        .get("results")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let custom_id = item.get("custom_id").and_then(Value::as_str)?.to_string();
                    if let Some(content) = item
                        .get("response")
                        .and_then(|r| r.get("content"))
                        .and_then(Value::as_str)
                    {
                        Some(GwBatchResult {
                            custom_id,
                            content: content.to_string(),
                            error: None,
                        })
                    } else {
                        let error = item
                            .get("error")
                            .map(|e| match e.as_str() {
                                Some(s) => s.to_string(),
                                None => e.to_string(),
                            })
                            .unwrap_or_else(|| "unknown error".to_string());
                        Some(GwBatchResult {
                            custom_id,
                            content: String::new(),
                            error: Some(error),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    GwBatchStatus { status, results }
}

/// Risolve l'URL del gateway da `settings.nexus_gateway_port` (mig 0239) in modo
/// TOLLERANTE (no panic): se la lettura fallisce ricade sulla porta di default
/// documentata. L'override di emergenza `NEXUS_GATEWAY_PORT` (stesso usato
/// dall'avvio) e' rispettato.
async fn resolve_gateway_url(db: &PgPool) -> String {
    if let Ok(port) = std::env::var("NEXUS_GATEWAY_PORT") {
        if let Ok(p) = port.trim().parse::<u16>() {
            if p > 0 {
                return format!("http://127.0.0.1:{p}");
            }
        }
    }
    let port = match get_setting_checked(db, "nexus_gateway_port").await {
        Ok(Some(raw)) => raw.trim().parse::<u16>().ok().filter(|p| *p > 0),
        _ => None,
    };
    let port = port.unwrap_or_else(|| {
        tracing::warn!(
            "gateway_client: settings.nexus_gateway_port non leggibile, uso default {}",
            GATEWAY_DEFAULT_PORT
        );
        GATEWAY_DEFAULT_PORT
    });
    format!("http://127.0.0.1:{port}")
}

/// Token di servizio del gateway. E' un SEGRETO: vive in env
/// `NEXUS_GATEWAY_SERVICE_TOKEN` (stessa convenzione di `main.rs`), mai nel DB.
/// Il fallback dev e' coerente con gli altri call site interni.
fn resolve_gateway_token() -> String {
    std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN").unwrap_or_else(|_| "dev-internal-token".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_in_progress_senza_risultati() {
        let body = json!({
            "status": "in_progress",
            "request_counts": { "processing": 2, "succeeded": 0 },
            "results": []
        });
        let parsed = parse_batch_status(&body);
        assert_eq!(parsed.status, "in_progress");
        assert!(!parsed.is_ended());
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn parse_status_ended_mappa_content_e_error() {
        // Un item con response.content (successo) e uno con error (fallito).
        let body = json!({
            "status": "ended",
            "results": [
                {
                    "custom_id": "file-0",
                    "response": { "content": "analisi ok", "model_used": "claude-x" }
                },
                {
                    "custom_id": "file-1",
                    "error": { "type": "invalid_request_error", "message": "boom" }
                }
            ]
        });
        let parsed = parse_batch_status(&body);
        assert!(parsed.is_ended());
        assert_eq!(parsed.results.len(), 2);

        let ok = parsed
            .results
            .iter()
            .find(|r| r.custom_id == "file-0")
            .unwrap();
        assert_eq!(ok.content, "analisi ok");
        assert!(ok.error.is_none());

        let ko = parsed
            .results
            .iter()
            .find(|r| r.custom_id == "file-1")
            .unwrap();
        assert!(ko.content.is_empty());
        assert!(ko.error.as_ref().unwrap().contains("boom"));
    }

    #[test]
    fn parse_status_error_stringa_diretta() {
        // Il gateway puo' anche ritornare error come stringa semplice.
        let body = json!({
            "status": "ended",
            "results": [
                { "custom_id": "c-1", "error": "timeout" }
            ]
        });
        let parsed = parse_batch_status(&body);
        let item = &parsed.results[0];
        assert_eq!(item.error.as_deref(), Some("timeout"));
    }

    #[test]
    fn parse_status_sconosciuto_e_in_progress() {
        // Stato mancante -> in_progress (fail-safe: non interrompe il polling).
        let body = json!({ "results": [] });
        let parsed = parse_batch_status(&body);
        assert_eq!(parsed.status, "in_progress");
        assert!(!parsed.is_ended());
    }
}
