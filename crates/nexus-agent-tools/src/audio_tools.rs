//! Tool audio: nexus_transcribe_audio (speech-to-text) e nexus_text_to_speech
//! (text-to-speech).
//!
//! `nexus_transcribe_audio` trascrive un audio allegato alla chat usando un
//! modello audio-in (OpenAI whisper di default, configurato in
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
//! `nexus_text_to_speech` fa l'inverso (OUTPUT audio, gemello di
//! image_tools::tool_nexus_generate_image): converte un testo in un file audio e
//! lo salva path-safe nel progetto (`.nexus/generated/<nome>.<ext>`), risolvendo
//! il modello dal purpose `text_to_speech`.
//!
//! Niente hardcoded (regola G): il modello arriva dal purpose via routing
//! (resolve_purpose_via_http), il limite size da settings, l'URL del gateway
//! dalla porta nel DB. Il gateway possiede routing/cooldown/privacy e mappa la
//! richiesta al dialetto del provider (regola L: punto unico gateway).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header};
use super::gateway_client::{gateway_text_to_speech, gateway_transcribe_audio};
use super::ToolContextCore;
use nexus_auth::get_setting_checked;
use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::workspace_paths::resolve_workspace_target;

/// Purpose che mappa al modello audio-in (mig 0480, tier=light,
/// required_capability=audio_in). Punto unico di selezione del modello: niente
/// nome modello hardcoded (regola G).
const AUDIO_PURPOSE: &str = "transcribe_audio";
/// Default safe se il setting agent.attachment.audio_max_bytes non e' impostato.
/// 25 MB e' il limite dell'API OpenAI /audio/transcriptions.
const AUDIO_MAX_BYTES_DEFAULT: usize = 25 * 1024 * 1024;

/// Purpose che mappa al modello audio-out (mig 0481, tier=light,
/// required_capability=audio_out). Punto unico di selezione del modello TTS:
/// niente nome modello hardcoded (regola G).
const TTS_PURPOSE: &str = "text_to_speech";
/// Subdir sotto la project_root dove salvare gli audio generati (parita' con
/// image_tools: stesso punto di output per gli artefatti media).
const GENERATED_SUBDIR: &str = ".nexus/generated";
/// Formato/estensione audio di default (l'API OpenAI /audio/speech emette mp3 se
/// non specificato altrimenti). Documentato, non un magic fallback di modello.
const TTS_DEFAULT_FORMAT: &str = "mp3";

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

// ── nexus_text_to_speech (OUTPUT audio) ──────────────────────────────────────

/// Converte un testo in un file audio e lo salva path-safe nel progetto.
///
/// Flusso gemello di image_tools::tool_nexus_generate_image (OUTPUT-file):
///   1) Gate ctx.can_write (il tool PRODUCE un file su disco): senza permesso,
///      errore esplicito (regola H).
///   2) Risolve provider/model dal purpose `text_to_speech` via routing
///      (resolve_purpose_via_http): NESSUN nome modello hardcoded (regola G).
///   3) Chiama il gateway (`POST /v1/audio/speech`) pinnando il provider risolto.
///   4) Decodifica il base64 e salva l'audio path-safe sotto la project_root
///      (resolve_workspace_target, regola E), con estensione coerente col
///      response_format richiesto (default mp3).
///   5) Ritorna { audio_path, model_used }.
pub async fn tool_nexus_text_to_speech(ctx: &ToolContextCore, input: &Value) -> String {
    // 1) Permesso di scrittura obbligatorio: il tool PRODUCE un file su disco.
    if !ctx.can_write {
        return json!({
            "error": "Permesso di scrittura non concesso: impossibile salvare l'audio \
                      generato su disco. Esegui in una modalita' che consente la scrittura file."
        })
        .to_string();
    }

    // 2) Testo obbligatorio + parametri opzionali.
    let text = match input.get("text").and_then(Value::as_str).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return json!({
                "error": "Parametro 'text' obbligatorio (testo da convertire in audio)."
            })
            .to_string();
        }
    };
    let voice = input
        .get("voice")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let filename = input
        .get("filename")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 3) Risolvi provider/model dal purpose (regola G: niente modello hardcoded).
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, TTS_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            return json!({
                "error": format!(
                    "modello audio-out non risolvibile (purpose '{TTS_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.text_to_speech (mig 0481) e che un modello \
                     audio-out sia abilitato nel catalog."
                )
            })
            .to_string();
        }
    };

    // Formato audio fissato a mp3 (default endpoint): l'estensione di salvataggio
    // deve essere coerente con cio' che il provider produce.
    let response_format = TTS_DEFAULT_FORMAT.to_string();

    // 4) Sintetizza via gateway (pin del provider risolto). Il gateway gestisce
    //    routing/cooldown/privacy e mappa la richiesta al dialetto del provider
    //    (regola L: punto unico gateway).
    let result = match gateway_text_to_speech(
        &ctx.db,
        &provider,
        &model,
        &text,
        voice,
        Some(response_format.clone()),
        TTS_PURPOSE,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "error": format!("sintesi vocale via gateway fallita: {e}")
            })
            .to_string();
        }
    };

    // 5) Decodifica base64 -> bytes.
    let bytes = match B64.decode(result.audio_base64.trim()) {
        Ok(b) => b,
        Err(e) => {
            return json!({ "error": format!("decodifica audio base64 fallita: {e}") }).to_string();
        }
    };
    if bytes.is_empty() {
        return json!({
            "error": "il gateway ha restituito un audio vuoto.",
            "model_used": result.model_used,
        })
        .to_string();
    }

    // 6) Estensione coerente col MIME reale (fallback al formato richiesto).
    let ext = ext_from_mime(&result.mime).unwrap_or(&response_format);

    // 7) Salva path-safe sotto la project_root.
    let audio_path = match save_audio(&ctx.root_path, &bytes, filename, ext).await {
        Ok(p) => p,
        Err(e) => return json!({ "error": format!("salvataggio audio fallito: {e}") }).to_string(),
    };

    json!({
        "audio_path": audio_path,
        "model_used": result.model_used,
    })
    .to_string()
}

/// Estensione file dal MIME audio prodotto dal provider. `None` per MIME non
/// riconosciuto (il chiamante ricade sul formato richiesto). Funzione PURA.
fn ext_from_mime(mime: &str) -> Option<&'static str> {
    let m = mime.split(';').next().unwrap_or(mime).trim().to_lowercase();
    let ext = match m.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/opus" => "opus",
        "audio/ogg" => "ogg",
        "audio/aac" => "aac",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/pcm" | "audio/l16" => "pcm",
        _ => return None,
    };
    Some(ext)
}

/// Costruisce un nome file sicuro per l'audio generato. Se l'agente passa
/// `filename`, ne usa SOLO il basename (niente directory ne' traversal: la
/// path-safety finale e' garantita da `resolve_workspace_target`) forzando
/// l'estensione `ext`; altrimenti genera un nome timestampato. Gemella di
/// image_tools::build_filename (regola L: stesso pattern di naming output).
fn build_audio_filename(requested: Option<&str>, ext: &str) -> String {
    let stem = requested
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
            format!("speech_{ts}")
        });
    format!("{stem}.{ext}")
}

/// Riduce un nome richiesto al solo basename, elimina l'estensione e tiene solo
/// caratteri innocui ([A-Za-z0-9._-]). Difesa-in-profondita': il confinamento
/// vero e' in resolve_workspace_target.
fn sanitize_stem(requested: &str) -> String {
    let base = requested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(requested)
        .trim();
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_', '-'])
        .to_string()
}

/// Salva i byte audio sotto la project_root (path-safe) e ritorna il path
/// relativo pulito. Usa `resolve_workspace_target` (punto unico path-safety,
/// regola L): rifiuta traversal e qualunque target fuori dalla root (regola E).
async fn save_audio(
    root: &std::path::Path,
    bytes: &[u8],
    requested_name: Option<&str>,
    ext: &str,
) -> Result<String, String> {
    let name = build_audio_filename(requested_name, ext);
    let rel = format!("{GENERATED_SUBDIR}/{name}");
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|e| format!("path audio non valido: {}", e.message()))?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creazione dir audio fallita: {e}"))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| format!("scrittura audio fallita: {e}"))?;
    Ok(clean_rel)
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

    #[test]
    fn ext_from_mime_mappa_audio_out() {
        assert_eq!(ext_from_mime("audio/mpeg"), Some("mp3"));
        // Content-Type con charset/parametri -> ignorati.
        assert_eq!(ext_from_mime("audio/mpeg; charset=binary"), Some("mp3"));
        assert_eq!(ext_from_mime("audio/wav"), Some("wav"));
        assert_eq!(ext_from_mime("audio/opus"), Some("opus"));
        assert_eq!(ext_from_mime("audio/flac"), Some("flac"));
        // MIME sconosciuto -> None (il chiamante ricade sul formato richiesto).
        assert_eq!(ext_from_mime("application/octet-stream"), None);
    }

    #[test]
    fn build_audio_filename_default_e_timestamp() {
        let name = build_audio_filename(None, "mp3");
        assert!(name.starts_with("speech_"));
        assert!(name.ends_with(".mp3"));
    }

    #[test]
    fn build_audio_filename_forza_estensione_e_strippa_directory() {
        // L'estensione richiesta viene sostituita; le directory nel nome scartate.
        assert_eq!(build_audio_filename(Some("voce.wav"), "mp3"), "voce.mp3");
        assert_eq!(
            build_audio_filename(Some("../../etc/passwd"), "mp3"),
            "passwd.mp3"
        );
    }

    #[test]
    fn sanitize_stem_nome_solo_simboli_diventa_vuoto() {
        assert_eq!(sanitize_stem("..."), "");
        let name = build_audio_filename(Some("..."), "mp3");
        assert!(name.starts_with("speech_"));
    }

    #[tokio::test]
    async fn save_audio_confina_nella_root_e_crea_dir() {
        let tmp = std::env::temp_dir().join(format!("tts_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let mp3 = b"ID3_fake_body";
        let rel = save_audio(&tmp, mp3, Some("saluto"), "mp3").await.unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/saluto.mp3"));
        let written = tokio::fs::read(tmp.join(&rel)).await.unwrap();
        assert_eq!(written, mp3);
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }

    #[tokio::test]
    async fn save_audio_rifiuta_traversal_anche_se_nome_lo_contiene() {
        let tmp = std::env::temp_dir().join(format!("tts_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let rel = save_audio(&tmp, b"x", Some("../../../escape"), "mp3")
            .await
            .unwrap();
        assert_eq!(rel, format!("{GENERATED_SUBDIR}/escape.mp3"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }
}
