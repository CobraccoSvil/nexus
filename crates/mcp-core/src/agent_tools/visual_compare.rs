//! Tool `nexus_visual_compare` — FASE 2 "resa Figma Make": verifica visiva.
//!
//! Screenshotta un URL locale dell'app costruita e lo confronta col design di
//! riferimento Figma (thumbnail.png dentro un .make, oppure un'immagine
//! allegata) usando un modello vision. Ritorna un'analisi strutturata degli
//! scostamenti di design (palette, tipografia, spaziature, layout, componenti)
//! e una stima di similarita' 0-100, cosi' l'agente in modalita' Continuo puo'
//! iterare e ridurre la distanza visiva.
//!
//! Riuso dell'infrastruttura esistente (niente nuova dipendenza headless):
//!   - Screenshot: driver Playwright gia' installato nel progetto, pilotato
//!     da uno script Node inline (stesso runtime di run_playwright_tests /
//!     browser_check). Nessun nuovo crate.
//!   - Reference .make: stesso accesso allegati di figma_tools /
//!     attachment_inspector (load_attachment + apertura ZIP).
//!   - Modello vision: risolto SOLO dal DB (regola G CLAUDE.md) via il purpose
//!     'visual_compare' lato brain (POST /vision/compare, mig 0214).
//!
//! Niente magic number: viewport, wait, timeout e soglia vivono in `settings`
//! (key `agent.visual_compare.*`, mig 0214). Niente panic: ogni fallimento
//! (Playwright assente, URL irraggiungibile, vision down) ritorna un JSON di
//! errore strutturato che non blocca l'agente.

use std::io::{Cursor, Read};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

use super::attachment_inspector::{detect_kind, load_attachment, read_header, AttachmentRecord};
use super::AgentToolContext;
use crate::projects::resolve_workspace_target;
use crate::settings;

// ── Default safe (usati solo se il setting manca o non e' parsabile; il DB e'
//    la fonte di verita', mig 0214). Documentati, non "magic fallback". ──────
const DEFAULT_VIEWPORT_WIDTH: u32 = 1280;
const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;
const DEFAULT_WAIT_MS: u64 = 1500;
const DEFAULT_SCREENSHOT_TIMEOUT_SECS: u64 = 45;
/// Timeout HTTP verso il brain per il confronto vision (cold start possibile).
const VISION_HTTP_TIMEOUT_SECS: u64 = 90;
/// Subdir sotto la project_root dove salvare gli screenshot del tool.
const SCREENSHOT_SUBDIR: &str = ".nexus/visual_compare";

/// Parametri operativi risolti dal DB.
struct CompareSettings {
    viewport_width: u32,
    viewport_height: u32,
    wait_ms: u64,
    screenshot_timeout_secs: u64,
}

/// `nexus_visual_compare(url, reference?, viewport?, wait_ms?)`.
pub(super) async fn tool_nexus_visual_compare(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return err(
            "Permesso di scrittura non concesso: impossibile salvare lo screenshot su disco.",
        );
    }

    // 1) URL obbligatorio + validazione basilare (solo http/https locale).
    let url = match input.get("url").and_then(Value::as_str).map(str::trim) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => return err("Parametro 'url' obbligatorio (es. http://localhost:29348/)."),
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return err("Il parametro 'url' deve iniziare con http:// o https://.");
    }

    // 2) Risolvi i parametri (DB + override per-chiamata).
    let mut cfg = load_settings(&ctx.db).await;
    if let Some(vp) = input.get("viewport") {
        if let Some(w) = vp.get("width").and_then(Value::as_u64) {
            if w > 0 {
                cfg.viewport_width = w.min(7680) as u32;
            }
        }
        if let Some(h) = vp.get("height").and_then(Value::as_u64) {
            if h > 0 {
                cfg.viewport_height = h.min(4320) as u32;
            }
        }
    }
    if let Some(w) = input.get("wait_ms").and_then(Value::as_u64) {
        cfg.wait_ms = w.min(60_000);
    }

    // 3) Recupera l'immagine di riferimento (opzionale).
    let reference = match input
        .get("reference")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some(r) if !r.is_empty() => match resolve_reference(ctx, r).await {
            Ok(Some(ref_img)) => Some(ref_img),
            Ok(None) => {
                return err(
                    "Impossibile usare 'reference': il .make non contiene thumbnail.png e \
                     l'allegato non e' un'immagine. Carica un'immagine di riferimento o \
                     ometti 'reference'.",
                );
            }
            Err(e) => return err(&format!("recupero reference fallito: {e}")),
        },
        _ => None,
    };

    // 4) Screenshot dell'URL via Playwright (script Node inline).
    let shot = match capture_screenshot(&ctx.root_path, &url, &cfg).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return json!({
                "error": format!("cattura screenshot fallita: {e}"),
                "hint": "Verifica che il dev server sia avviato e raggiungibile all'url indicato, \
                         e che Playwright sia installato nel progetto (npx playwright install chromium). \
                         Non insistere in loop: risolvi la causa o procedi senza la verifica visiva.",
            })
            .to_string();
        }
    };

    // 5) Salva lo screenshot su disco (path-safe nel workspace).
    let screenshot_path = match save_screenshot(&ctx.root_path, &shot).await {
        Ok(p) => p,
        Err(e) => return err(&format!("salvataggio screenshot fallito: {e}")),
    };

    // 6) Se non c'e' reference, non chiamiamo la vision: ritorniamo il path.
    let Some(reference) = reference else {
        return json!({
            "screenshot_path": screenshot_path,
            "reference_source": Value::Null,
            "note": "Screenshot catturato e salvato su disco. Nessun 'reference' fornito: \
                     impossibile calcolare similarity_score/differences. Passa l'attachment_id \
                     del .make o di un'immagine di riferimento per il confronto vision.",
        })
        .to_string();
    };

    // 7) Confronto vision via brain.
    let shot_b64 = B64.encode(&shot);
    let ref_b64 = B64.encode(&reference.bytes);
    let compare =
        compare_via_brain(&ctx.db, &shot_b64, "image/png", &ref_b64, &reference.mime).await;

    match compare {
        Ok(v) => json!({
            "similarity_score": v.get("similarity_score").cloned().unwrap_or(Value::Null),
            "differences": v.get("differences").cloned().unwrap_or(json!([])),
            "screenshot_path": screenshot_path,
            "reference_source": reference.source,
            "model_used": v.get("model_used").cloned().unwrap_or(Value::Null),
            "parse_error": v.get("parse_error").cloned().unwrap_or(Value::Null),
        })
        .to_string(),
        Err(e) => json!({
            "error": format!("confronto vision fallito: {e}"),
            "screenshot_path": screenshot_path,
            "reference_source": reference.source,
            "hint": "Lo screenshot e' stato salvato. La vision non e' disponibile o non e' \
                     configurata (nexus_purpose_model.visual_compare, mig 0214). Non insistere \
                     in loop: segnala il problema e procedi.",
        })
        .to_string(),
    }
}

/// Immagine di riferimento risolta + provenienza.
struct ReferenceImage {
    bytes: Vec<u8>,
    mime: String,
    /// "thumbnail" (estratta dal .make) oppure "attachment" (immagine allegata).
    source: String,
}

fn err(msg: &str) -> String {
    json!({ "error": msg }).to_string()
}

/// Carica i parametri dal DB con default safe documentati.
async fn load_settings(db: &sqlx::PgPool) -> CompareSettings {
    CompareSettings {
        viewport_width: setting_u64(
            db,
            "agent.visual_compare.viewport_width",
            DEFAULT_VIEWPORT_WIDTH as u64,
        )
        .await as u32,
        viewport_height: setting_u64(
            db,
            "agent.visual_compare.viewport_height",
            DEFAULT_VIEWPORT_HEIGHT as u64,
        )
        .await as u32,
        wait_ms: setting_u64(db, "agent.visual_compare.wait_ms", DEFAULT_WAIT_MS).await,
        screenshot_timeout_secs: setting_u64(
            db,
            "agent.visual_compare.screenshot_timeout_secs",
            DEFAULT_SCREENSHOT_TIMEOUT_SECS,
        )
        .await,
    }
}

/// Legge un setting numerico; default safe se assente/non parsabile/DB down.
async fn setting_u64(db: &sqlx::PgPool, key: &str, default: u64) -> u64 {
    match settings::get_setting(db, key).await {
        Ok(Some(raw)) => match raw.trim().parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!(key = %key, raw = %raw, default, "visual_compare: setting non parsabile, uso default");
                default
            }
        },
        Ok(None) => default,
        Err(e) => {
            tracing::warn!(key = %key, error = %e, default, "visual_compare: lettura setting fallita, uso default");
            default
        }
    }
}

/// Risolve l'immagine di riferimento dall'attachment_id passato in `reference`.
///
/// - `.make` (kind figma) -> estrae thumbnail.png dallo ZIP;
/// - immagine -> usa i byte direttamente.
///
/// Ritorna `Ok(None)` se l'allegato non e' utilizzabile come riferimento
/// (es. .make senza thumbnail e non immagine).
async fn resolve_reference(
    ctx: &AgentToolContext,
    reference: &str,
) -> Result<Option<ReferenceImage>, String> {
    let attachment_id = Uuid::parse_str(reference)
        .map_err(|_| "'reference' non e' un attachment_id (UUID) valido".to_string())?;

    let record = load_attachment(&ctx.db, attachment_id, ctx.project_id).await?;
    let header = read_header(&record.file_path).await?;
    let (kind, mime_reale, _ext) = detect_kind(&header, &record.file_name, &record.mime_type);

    // Caso immagine diretta.
    if is_image_kind(&kind) {
        let bytes = tokio::fs::read(&record.file_path)
            .await
            .map_err(|e| format!("read immagine reference fallita: {e}"))?;
        return Ok(Some(ReferenceImage {
            bytes,
            mime: mime_reale,
            source: "attachment".to_string(),
        }));
    }

    // Caso .make Figma: estrai thumbnail.png dallo ZIP.
    if kind == "figma" {
        return extract_make_thumbnail(&record).await;
    }

    Ok(None)
}

/// Estrae `thumbnail.png` da un archivio .make. Ritorna `Ok(None)` se assente.
async fn extract_make_thumbnail(
    record: &AttachmentRecord,
) -> Result<Option<ReferenceImage>, String> {
    let bytes = tokio::fs::read(&record.file_path)
        .await
        .map_err(|e| format!("read .make fallita: {e}"))?;

    let thumb = tokio::task::spawn_blocking(move || extract_thumbnail_bytes(&bytes))
        .await
        .map_err(|e| format!("spawn_blocking fallita: {e}"))??;

    Ok(thumb.map(|bytes| ReferenceImage {
        bytes,
        mime: "image/png".to_string(),
        source: "thumbnail".to_string(),
    }))
}

/// Apre lo ZIP e legge `thumbnail.png` se presente. Errori ZIP propagati;
/// thumbnail assente -> `Ok(None)`.
fn extract_thumbnail_bytes(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return Err("il .make non e' uno ZIP valido".into());
    }
    let reader = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let mut idx: Option<usize> = None;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_lowercase();
            if name == "thumbnail.png" || name.ends_with("/thumbnail.png") {
                idx = Some(i);
                break;
            }
        }
    }
    let Some(idx) = idx else {
        return Ok(None);
    };
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| format!("apertura thumbnail fallita: {e}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("lettura thumbnail fallita: {e}"))?;
    Ok(Some(buf))
}

fn is_image_kind(kind: &str) -> bool {
    matches!(kind, "png" | "jpeg" | "gif" | "webp" | "image")
}

/// Cattura uno screenshot dell'URL via Playwright (driver gia' installato nel
/// progetto), pilotato da uno script Node inline. Riusa lo stesso runtime di
/// run_playwright_tests: nessuna nuova dipendenza headless.
///
/// Lo script stampa i byte PNG su stdout (in base64, per evitare problemi di
/// encoding binario sui pipe) preceduti da un marcatore.
async fn capture_screenshot(
    root: &Path,
    url: &str,
    cfg: &CompareSettings,
) -> Result<Vec<u8>, String> {
    const MARKER: &str = "NEXUS_SHOT_B64:";
    let script = format!(
        r#"
const {{ chromium }} = require('playwright');
(async () => {{
  let browser;
  try {{
    browser = await chromium.launch({{ headless: true, args: ['--no-sandbox'] }});
    const page = await browser.newPage({{ viewport: {{ width: {w}, height: {h} }} }});
    await page.goto({url}, {{ waitUntil: 'networkidle', timeout: {nav_timeout} }});
    await page.waitForTimeout({wait});
    const buf = await page.screenshot({{ fullPage: false }});
    process.stdout.write('{marker}' + buf.toString('base64'));
    await browser.close();
  }} catch (e) {{
    if (browser) {{ try {{ await browser.close(); }} catch (_) {{}} }}
    process.stderr.write('SHOT_ERROR:' + (e && e.message ? e.message : String(e)));
    process.exit(2);
  }}
}})();
"#,
        w = cfg.viewport_width,
        h = cfg.viewport_height,
        url = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string()),
        nav_timeout = cfg.screenshot_timeout_secs.saturating_mul(1000),
        wait = cfg.wait_ms,
        marker = MARKER,
    );

    let mut cmd = Command::new("node");
    cmd.arg("-e")
        .arg(&script)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("avvio node fallito ({e}): assicurati che node e il pacchetto playwright siano installati nel progetto"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Timeout complessivo = navigazione + wait + margine.
    let overall = Duration::from_secs(cfg.screenshot_timeout_secs.saturating_add(15))
        .saturating_add(Duration::from_millis(cfg.wait_ms));

    let status = tokio::time::timeout(overall, child.wait()).await;
    let status = match status {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("attesa node fallita: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "timeout {}s nello screenshot (load/render troppo lenti o url non raggiungibile)",
                overall.as_secs()
            ));
        }
    };

    let mut out = String::new();
    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut out).await;
    }
    let mut errbuf = String::new();
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut errbuf).await;
    }

    if !status.success() {
        let detail = errbuf
            .lines()
            .find(|l| l.contains("SHOT_ERROR:"))
            .map(|l| l.replace("SHOT_ERROR:", "").trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| errbuf.lines().last().map(|s| s.trim().to_string()))
            .unwrap_or_else(|| "errore sconosciuto".to_string());
        return Err(format!("script screenshot fallito: {detail}"));
    }

    let b64 = out
        .find(MARKER)
        .map(|pos| out[pos + MARKER.len()..].trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "lo script non ha prodotto un'immagine (marcatore assente)".to_string())?;

    B64.decode(b64)
        .map_err(|e| format!("decodifica screenshot base64 fallita: {e}"))
}

/// Salva lo screenshot PNG sotto la project_root (path-safe) e ritorna il path
/// relativo pulito.
async fn save_screenshot(root: &Path, bytes: &[u8]) -> Result<String, String> {
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
    let rel = format!("{SCREENSHOT_SUBDIR}/screenshot_{ts}.png");
    let (clean_rel, abs_target) = resolve_workspace_target(root, &rel)
        .map_err(|_| "path screenshot non valido".to_string())?;

    if let Some(parent) = abs_target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creazione dir screenshot fallita: {e}"))?;
    }
    tokio::fs::write(&abs_target, bytes)
        .await
        .map_err(|e| format!("scrittura screenshot fallita: {e}"))?;
    Ok(clean_rel)
}

/// POST al brain `/vision/compare` con le due immagini base64.
async fn compare_via_brain(
    db: &sqlx::PgPool,
    screenshot_b64: &str,
    screenshot_mime: &str,
    reference_b64: &str,
    reference_mime: &str,
) -> Result<Value, String> {
    let brain_url = resolve_brain_url(db).await;
    let endpoint = format!("{}/vision/compare", brain_url.trim_end_matches('/'));
    let payload = json!({
        "screenshot_base64": screenshot_b64,
        "screenshot_mime": screenshot_mime,
        "reference_base64": reference_b64,
        "reference_mime": reference_mime,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(VISION_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP: {e}"))?;

    let response = client
        .post(&endpoint)
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            format!(
                "chiamata vision compare fallita verso {endpoint}: {e}. Verifica che il brain \
                 sia attivo e che nexus_purpose_model.visual_compare sia configurato (mig 0214)."
            )
        })?;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "vision compare ha risposto HTTP {}: {}",
            status.as_u16(),
            body_text
        ));
    }
    serde_json::from_str::<Value>(&body_text)
        .map_err(|e| format!("risposta vision compare non e' JSON valido: {e}; body={body_text}"))
}

/// Resolve URL brain: prima settings.brain_rest_url, poi env var, poi default
/// locale. Allineato a vision_tools::resolve_brain_url.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip_with(thumb: Option<&[u8]>) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: SimpleFileOptions =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("ai_chat.json", opts).unwrap();
            zw.write_all(br#"{"threads":[]}"#).unwrap();
            if let Some(t) = thumb {
                zw.start_file("thumbnail.png", opts).unwrap();
                zw.write_all(t).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn thumbnail_extracted_when_present() {
        let png = b"\x89PNG\r\n\x1a\n_fake_png_body";
        let zip = make_zip_with(Some(png));
        let got = extract_thumbnail_bytes(&zip).expect("estrazione ok");
        assert_eq!(got.as_deref(), Some(&png[..]));
    }

    #[test]
    fn thumbnail_none_when_absent() {
        let zip = make_zip_with(None);
        let got = extract_thumbnail_bytes(&zip).expect("estrazione ok");
        assert!(got.is_none());
    }

    #[test]
    fn thumbnail_errors_on_non_zip() {
        let raw = b"not a zip at all";
        let err = extract_thumbnail_bytes(raw).unwrap_err();
        assert!(err.contains("ZIP"));
    }

    #[test]
    fn is_image_kind_classifies() {
        assert!(is_image_kind("png"));
        assert!(is_image_kind("jpeg"));
        assert!(is_image_kind("image"));
        assert!(!is_image_kind("figma"));
        assert!(!is_image_kind("pdf"));
    }

    /// Serializzazione del manifest di output (successo): i campi attesi dallo
    /// schema FASE 2 devono essere tutti presenti e ben formati.
    #[test]
    fn output_manifest_success_shape() {
        let vision = json!({
            "similarity_score": 78,
            "differences": [
                {"category": "colore", "severity": "alta", "description": "palette diversa", "suggested_fix": "usa bg-slate-900"}
            ],
            "model_used": "google/gemini-2.0-flash-exp",
        });
        let out = json!({
            "similarity_score": vision.get("similarity_score").cloned().unwrap_or(Value::Null),
            "differences": vision.get("differences").cloned().unwrap_or(json!([])),
            "screenshot_path": ".nexus/visual_compare/screenshot_x.png",
            "reference_source": "thumbnail",
            "model_used": vision.get("model_used").cloned().unwrap_or(Value::Null),
        });

        // Round-trip stringa -> Value per simulare il contratto del tool.
        let s = out.to_string();
        let parsed: Value = serde_json::from_str(&s).expect("manifest JSON valido");
        assert_eq!(parsed["similarity_score"], 78);
        assert_eq!(parsed["reference_source"], "thumbnail");
        assert_eq!(parsed["model_used"], "google/gemini-2.0-flash-exp");
        assert_eq!(parsed["differences"][0]["category"], "colore");
        assert_eq!(parsed["differences"][0]["severity"], "alta");
        assert!(parsed["screenshot_path"]
            .as_str()
            .unwrap()
            .ends_with(".png"));
    }

    /// Il manifest di errore non deve contenere le immagini base64 e deve
    /// preservare il path dello screenshot se gia' salvato.
    #[test]
    fn output_manifest_error_has_no_images() {
        let out = json!({
            "error": "confronto vision fallito: brain down",
            "screenshot_path": ".nexus/visual_compare/screenshot_y.png",
            "reference_source": "attachment",
            "hint": "non insistere in loop",
        });
        let s = out.to_string();
        assert!(!s.contains("base64"));
        assert!(s.contains("screenshot_path"));
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert!(parsed.get("error").is_some());
        assert_eq!(parsed["reference_source"], "attachment");
    }
}
