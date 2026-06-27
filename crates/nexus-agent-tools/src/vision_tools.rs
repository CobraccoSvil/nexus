//! Tool nexus_describe_image_attachment.
//!
//! Descrive un'immagine allegata alla chat usando un modello vision (Google
//! Gemini di default, configurato in nexus_purpose_model.vision_describe).
//!
//! Flusso:
//!   1) Recupera l'allegato dal DB filtrando per project_id.
//!   2) Verifica via magic-byte detection che il kind sia image_*.
//!   3) Verifica che size_bytes sia entro il limite DB
//!      (agent.attachment.image_max_bytes, default 2 MB).
//!   4) Legge il file, costruisce un data URI base64 e chiama il Nexus LLM
//!      Gateway (`POST /v1/complete`) con una richiesta MULTIMODALE (prompt
//!      testuale + blocco image_url), pinnando il provider/modello risolto dal
//!      purpose `vision_describe`. La chiamata e' tutta Rust: non passa piu'
//!      dal brain Python (/vision/describe rimosso).
//!   5) Parsa la risposta DESCRIZIONE:/OCR: e restituisce description +
//!      ocr_text + model_used al modello.
//!
//! Niente hardcoded (regola G): il modello arriva dal purpose via routing
//! (resolve_purpose_via_http), il limite size da settings, l'URL del gateway
//! dalla porta nel DB. Il gateway possiede routing/cooldown/privacy e mappa il
//! blocco immagine al dialetto del provider (regola L: punto unico gateway).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header};
use super::gateway_client::gateway_vision_complete;
use super::ToolContextCore;
use nexus_auth::get_setting_checked;
use nexus_types::routing_client::resolve_purpose_via_http;

/// Purpose che mappa al modello vision (mig 0194). Punto unico di selezione del
/// modello: niente nome modello hardcoded (regola G).
const VISION_PURPOSE: &str = "vision_describe";
/// Token massimi della risposta vision (parita' col brain: 2048).
const VISION_MAX_TOKENS: u32 = 2048;
/// Default safe se il setting agent.attachment.image_max_bytes non e' impostato.
const IMAGE_MAX_BYTES_DEFAULT: usize = 2 * 1024 * 1024;

/// Prompt di default vision, parita' col brain (`_VISION_DEFAULT_PROMPT`):
/// impone il formato DESCRIZIONE:/OCR: che `parse_vision_response` separa.
const VISION_DEFAULT_PROMPT: &str = "Descrivi il contenuto visivo dell'immagine in italiano. \
Se contiene testo leggibile riporta tutti i testi nella sezione OCR. \
Formato risposta esatto: DESCRIZIONE: ...\nOCR: ... \
(riporta sezione OCR vuota se non c'e' testo).";

pub async fn tool_nexus_describe_image_attachment(
    ctx: &ToolContextCore,
    input: &Value,
) -> String {
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
    let question = input
        .get("question")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // 1) Lookup allegato (scoped al project_id corrente).
    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    // 2) Inspect: deve essere image_*.
    let header = match read_header(&record.file_path).await {
        Ok(h) => h,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);
    if !is_image_kind(&kind) {
        return json!({
            "error": format!(
                "L'allegato non e' un'immagine (kind rilevato: '{}'). Usa il tool di estrazione corretto per quel kind.",
                kind
            ),
            "kind": kind,
        })
        .to_string();
    }

    // 3) Limite size dal DB (no fallback nascosto: default safe documentato).
    let max_bytes = image_max_bytes(&ctx.db).await;
    if record.size_bytes < 0 || (record.size_bytes as usize) > max_bytes {
        return json!({
            "error": format!(
                "Immagine troppo grande ({} byte, limite {} byte). Configura 'agent.attachment.image_max_bytes' in settings se devi alzare il limite.",
                record.size_bytes, max_bytes
            ),
            "size_bytes": record.size_bytes,
            "max_bytes": max_bytes,
        })
        .to_string();
    }

    // 4) Leggi e costruisci il data URI base64 (formato image_url del gateway).
    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };
    let data_uri = format!("data:{};base64,{}", mime_reale, B64.encode(&bytes));

    // 5) Risolvi provider/model dal purpose (regola G: niente modello hardcoded).
    //    resolve_purpose_via_http e' il punto unico cross-processo che interroga
    //    il routing tier-only di mcp-core.
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, VISION_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            return json!({
                "error": format!(
                    "modello vision non risolvibile (purpose '{VISION_PURPOSE}'): {e}. \
                     Verifica nexus_purpose_model.vision_describe (mig 0194)."
                )
            })
            .to_string();
        }
    };

    // 6) Chiamata multimodale al gateway Rust (prompt + immagine). Il gateway
    //    mappa il blocco image_url al dialetto del provider e gestisce
    //    routing/cooldown/privacy (regola L: punto unico gateway).
    let prompt_text = question.as_deref().unwrap_or(VISION_DEFAULT_PROMPT);
    let content_blocks = json!([
        { "type": "text", "text": prompt_text },
        { "type": "image_url", "image_url": { "url": data_uri } },
    ]);

    let result = match gateway_vision_complete(
        &ctx.db,
        &provider,
        &model,
        content_blocks,
        VISION_MAX_TOKENS,
        VISION_PURPOSE,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "error": format!("chiamata vision via gateway fallita: {e}")
            })
            .to_string();
        }
    };

    // 7) Parsa DESCRIZIONE:/OCR: e restituisci al modello (parita' col brain).
    let (description, ocr_text) = parse_vision_response(&result.content);
    json!({
        "attachment_id": record.id.to_string(),
        "file_name": record.file_name,
        "mime_type": mime_reale,
        "description": description,
        "ocr_text": ocr_text.map(Value::String).unwrap_or(Value::Null),
        "model_used": result.model_used,
    })
    .to_string()
}

fn is_image_kind(kind: &str) -> bool {
    matches!(kind, "png" | "jpeg" | "gif" | "webp" | "svg" | "image")
}

/// Separa il payload `DESCRIZIONE: ...\nOCR: ...` in `(descrizione, ocr)`.
/// Parita' col brain (`_parse_vision_response`): se il modello non rispetta il
/// formato, ritorna l'intero testo come descrizione e `ocr = None`.
fn parse_vision_response(text: &str) -> (String, Option<String>) {
    if text.is_empty() {
        return (String::new(), None);
    }
    let upper = text.to_uppercase();
    let desc_idx = match upper.find("DESCRIZIONE:") {
        Some(i) => i,
        None => return (text.trim().to_string(), None),
    };
    let desc_start = desc_idx + "DESCRIZIONE:".len();
    let ocr_idx = upper.find("OCR:");
    match ocr_idx {
        Some(o) if o >= desc_idx => {
            let description = text[desc_start..o].trim().to_string();
            let ocr_text = text[o + "OCR:".len()..].trim();
            let ocr_value = if ocr_text.is_empty() {
                None
            } else {
                Some(ocr_text.to_string())
            };
            (description, ocr_value)
        }
        _ => (text[desc_start..].trim().to_string(), None),
    }
}

/// Legge agent.attachment.image_max_bytes da settings. Se mancante o DB
/// down, ritorna il default safe documentato (2 MB) e logga WARN.
async fn image_max_bytes(db: &sqlx::PgPool) -> usize {
    match get_setting_checked(db, "agent.attachment.image_max_bytes").await {
        Ok(Some(raw)) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "vision_tools: 'agent.attachment.image_max_bytes' non parsabile, uso default {}",
                    IMAGE_MAX_BYTES_DEFAULT
                );
                IMAGE_MAX_BYTES_DEFAULT
            }
        },
        Ok(None) => IMAGE_MAX_BYTES_DEFAULT,
        Err(e) => {
            tracing::warn!(error = %e, "vision_tools: lettura settings fallita, uso default");
            IMAGE_MAX_BYTES_DEFAULT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_formato_completo_descrizione_e_ocr() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: un gatto\nOCR: ciao mondo");
        assert_eq!(desc, "un gatto");
        assert_eq!(ocr.as_deref(), Some("ciao mondo"));
    }

    #[test]
    fn parse_ocr_vuoto_diventa_none() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: solo immagine\nOCR:");
        assert_eq!(desc, "solo immagine");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_senza_marcatori_tutto_descrizione() {
        let (desc, ocr) = parse_vision_response("  testo libero senza formato  ");
        assert_eq!(desc, "testo libero senza formato");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_senza_ocr_solo_descrizione() {
        let (desc, ocr) = parse_vision_response("DESCRIZIONE: paesaggio montano");
        assert_eq!(desc, "paesaggio montano");
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_testo_vuoto() {
        let (desc, ocr) = parse_vision_response("");
        assert!(desc.is_empty());
        assert!(ocr.is_none());
    }

    #[test]
    fn parse_case_insensitive_sui_marcatori() {
        // I marcatori sono cercati case-insensitive (upper), ma il testo
        // restituito mantiene il case originale.
        let (desc, ocr) = parse_vision_response("descrizione: Logo Blu\nocr: NEXUS");
        assert_eq!(desc, "Logo Blu");
        assert_eq!(ocr.as_deref(), Some("NEXUS"));
    }
}
