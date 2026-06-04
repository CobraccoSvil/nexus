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
//!   4) Legge il file, codifica in base64 e POST al brain
//!      <brain_rest_url>/vision/describe.
//!   5) Restituisce description + ocr_text + model_used al modello.
//!
//! Niente hardcoded: il brain URL viene letto da settings.brain_rest_url
//! (fallback env var BRAIN_REST_URL solo per emergenza) e il limite size
//! viene da settings.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header};
use super::AgentToolContext;
use crate::settings;

/// Timeout HTTP verso il brain. Vision puo' essere lento (cold start, immagini grandi).
const VISION_HTTP_TIMEOUT_SECS: u64 = 60;
/// Default safe se il setting agent.attachment.image_max_bytes non e' impostato.
const IMAGE_MAX_BYTES_DEFAULT: usize = 2 * 1024 * 1024;

pub(super) async fn tool_nexus_describe_image_attachment(
    ctx: &AgentToolContext,
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

    // 4) Leggi e codifica base64.
    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };
    let image_base64 = B64.encode(&bytes);

    // 5) POST al brain.
    let brain_url = resolve_brain_url(&ctx.db).await;
    let endpoint = format!("{}/vision/describe", brain_url.trim_end_matches('/'));
    let mut payload = json!({
        "image_base64": image_base64,
        "mime_type": mime_reale,
    });
    if let Some(q) = &question {
        payload["question"] = json!(q);
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(VISION_HTTP_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return json!({ "error": format!("impossibile costruire client HTTP: {e}") })
                .to_string();
        }
    };

    let response = match client.post(&endpoint).json(&payload).send().await {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "error": format!(
                    "chiamata vision fallita verso {endpoint}: {e}. Verifica che il brain sia attivo e che 'nexus_purpose_model.vision_describe' sia configurato."
                )
            })
            .to_string();
        }
    };

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return json!({
            "error": format!("vision endpoint ha risposto HTTP {}: {}", status.as_u16(), body_text)
        })
        .to_string();
    }

    // 6) Forward del body al modello (descrizione + ocr + model_used).
    match serde_json::from_str::<Value>(&body_text) {
        Ok(v) => json!({
            "attachment_id": record.id.to_string(),
            "file_name": record.file_name,
            "mime_type": mime_reale,
            "description": v.get("description").cloned().unwrap_or(Value::Null),
            "ocr_text": v.get("ocr_text").cloned().unwrap_or(Value::Null),
            "model_used": v.get("model_used").cloned().unwrap_or(Value::Null),
        })
        .to_string(),
        Err(e) => json!({
            "error": format!("risposta vision non e' JSON valido: {e}"),
            "raw_body": body_text,
        })
        .to_string(),
    }
}

fn is_image_kind(kind: &str) -> bool {
    matches!(kind, "png" | "jpeg" | "gif" | "webp" | "svg" | "image")
}

/// Legge agent.attachment.image_max_bytes da settings. Se mancante o DB
/// down, ritorna il default safe documentato (2 MB) e logga WARN.
async fn image_max_bytes(db: &sqlx::PgPool) -> usize {
    match settings::get_setting(db, "agent.attachment.image_max_bytes").await {
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

/// Resolve URL brain: prima settings.brain_rest_url, poi env var, poi default
/// locale. Coerente con brain_agent_client::brain_rest_url ma con accesso al DB.
async fn resolve_brain_url(db: &sqlx::PgPool) -> String {
    if let Ok(Some(v)) = settings::get_setting(db, "brain_rest_url").await {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("BRAIN_REST_URL")
        .or_else(|_| std::env::var("NEURAL_CORE_REST_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string())
}
