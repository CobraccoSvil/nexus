//! Tool nexus_transcribe_audio.
//!
//! Trascrive un audio allegato alla chat (speech-to-text) usando un modello
//! audio-in (OpenAI whisper di default, configurato in
//! nexus_purpose_model.transcribe_audio).
//!
//! Flusso (gemello di vision_tools, ma INPUT audio):
//!   1) Recupera l'allegato dal DB filtrando per project_id (regola E).
//!   2) Verifica via magic-byte detection che il kind sia audio_*.
//!   3) Verifica che size_bytes sia entro il limite DB
//!      (agent.attachment.audio_max_bytes, default 25 MB).
//!   4) Legge il file, lo codifica base64 e chiama il Nexus LLM Gateway
//!      (`POST /v1/audio/transcriptions`) pinnando il provider/modello risolto
//!      dal purpose `transcribe_audio`. La chiamata e' tutta Rust.
//!   5) Ritorna { text, model_used } al modello.
//!
//! Niente hardcoded (regola G): il modello arriva dal purpose via routing
//! (resolve_purpose_via_http), il limite size da settings, l'URL del gateway
//! dalla porta nel DB. Il gateway possiede routing/cooldown/privacy e mappa la
//! richiesta al dialetto del provider (regola L: punto unico gateway).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header};
use super::gateway_client::gateway_transcribe_audio;
use super::ToolContextCore;
use nexus_auth::get_setting_checked;
use nexus_types::routing_client::resolve_purpose_via_http;

/// Purpose che mappa al modello audio-in (mig 0480, tier=light,
/// required_capability=audio_in). Punto unico di selezione del modello: niente
/// nome modello hardcoded (regola G).
const AUDIO_PURPOSE: &str = "transcribe_audio";
/// Default safe se il setting agent.attachment.audio_max_bytes non e' impostato.
/// 25 MB e' il limite dell'API OpenAI /audio/transcriptions.
const AUDIO_MAX_BYTES_DEFAULT: usize = 25 * 1024 * 1024;

pub async fn tool_nexus_transcribe_audio(ctx: &ToolContextCore, input: &Value) -> String {
    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => {
            return json!({
                "error": "Parametro 'attachment_id' obbligatorio (UUID valido)."
            })
            .to_string();
        }
    };
    let language = input
        .get("language")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // 1) Lookup allegato (scoped al project_id corrente, regola E).
    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    // 2) Inspect: deve essere audio_*.
    let header = match read_header(&record.file_path).await {
        Ok(h) => h,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);
    if !is_audio_kind(&kind) {
        return json!({
            "error": format!(
                "L'allegato non e' un audio (kind rilevato: '{}'). Usa il tool di estrazione corretto per quel kind.",
                kind
            ),
            "kind": kind,
        })
        .to_string();
    }

    // 3) Limite size dal DB (no fallback nascosto: default safe documentato).
    let max_bytes = audio_max_bytes(&ctx.db).await;
    if record.size_bytes < 0 || (record.size_bytes as usize) > max_bytes {
        return json!({
            "error": format!(
                "Audio troppo grande ({} byte, limite {} byte). Configura 'agent.attachment.audio_max_bytes' in settings se devi alzare il limite.",
                record.size_bytes, max_bytes
            ),
            "size_bytes": record.size_bytes,
            "max_bytes": max_bytes,
        })
        .to_string();
    }

    // 4) Leggi e codifica base64 (il gateway invia il binario come multipart).
    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };
    let audio_base64 = B64.encode(&bytes);

    // 5) Risolvi provider/model dal purpose (regola G: niente modello hardcoded).
    //    resolve_purpose_via_http e' il punto unico cross-processo che interroga
    //    il routing tier-only di mcp-core (capability='audio_in').
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, AUDIO_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            return json!({
                "error": format!(
                    "modello audio-in non risolvibile (purpose '{AUDIO_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.transcribe_audio (mig 0480) e che un modello \
                     audio-in sia abilitato nel catalog."
                )
            })
            .to_string();
        }
    };

    // 6) Trascrivi via gateway (pin del provider risolto). Il gateway gestisce
    //    routing/cooldown/privacy e mappa la richiesta al dialetto del provider
    //    (regola L: punto unico gateway).
    let result = match gateway_transcribe_audio(
        &ctx.db,
        &provider,
        &model,
        audio_base64,
        Some(mime_reale.clone()),
        language,
        AUDIO_PURPOSE,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "error": format!("trascrizione audio via gateway fallita: {e}")
            })
            .to_string();
        }
    };

    json!({
        "attachment_id": record.id.to_string(),
        "file_name": record.file_name,
        "mime_type": mime_reale,
        "text": result.text,
        "model_used": result.model_used,
    })
    .to_string()
}

/// True se il kind rilevato dal magic-byte detection e' audio (parita' con i kind
/// emessi da `mime_to_kind` in attachment_inspector: mp3/wav + generico audio).
fn is_audio_kind(kind: &str) -> bool {
    matches!(kind, "mp3" | "wav" | "audio")
}

/// Legge agent.attachment.audio_max_bytes da settings. Se mancante o DB down,
/// ritorna il default safe documentato (25 MB) e logga WARN. Gemella di
/// `vision_tools::image_max_bytes` (regola L: stesso pattern di lettura setting).
async fn audio_max_bytes(db: &sqlx::PgPool) -> usize {
    match get_setting_checked(db, "agent.attachment.audio_max_bytes").await {
        Ok(Some(raw)) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "audio_tools: 'agent.attachment.audio_max_bytes' non parsabile, uso default {}",
                    AUDIO_MAX_BYTES_DEFAULT
                );
                AUDIO_MAX_BYTES_DEFAULT
            }
        },
        Ok(None) => AUDIO_MAX_BYTES_DEFAULT,
        Err(e) => {
            tracing::warn!(error = %e, "audio_tools: lettura settings fallita, uso default");
            AUDIO_MAX_BYTES_DEFAULT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_audio_kind_riconosce_audio_e_scarta_altro() {
        assert!(is_audio_kind("mp3"));
        assert!(is_audio_kind("wav"));
        assert!(is_audio_kind("audio"));
        // Non-audio: scartati (l'agente viene rimandato al tool corretto).
        assert!(!is_audio_kind("png"));
        assert!(!is_audio_kind("pdf"));
        assert!(!is_audio_kind("binary"));
        assert!(!is_audio_kind("video"));
    }
}
