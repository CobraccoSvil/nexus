//! Tool di estrazione documenti: PDF, DOCX, XLSX.
//!
//! Tutte le operazioni sono CPU-bound: eseguite in `spawn_blocking`.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::load_attachment;
use super::attachment_settings;
use super::AgentToolContext;

// ──────────────────────────────────────────────────────────────────────────
// Helper inline (FIX 3 ADR 0012)
// ──────────────────────────────────────────────────────────────────────────
// Usati dalla pre-extraction automatica in chat_messages.rs. Non sono tool
// agente: ritornano direttamente la stringa estratta (truncata a max_chars)
// o un Err con il motivo. Riusano la logica dei tool ma senza il wrapping JSON.

/// Pre-extract testo da un PDF gia' su filesystem (path assoluto). Tronca a
/// `max_chars` caratteri. Usa spawn_blocking perche' pdf-extract e' sync.
pub async fn extract_pdf_text_inline(file_path: &std::path::Path, max_chars: usize)
    -> Result<String, String>
{
    let bytes = tokio::fs::read(file_path).await
        .map_err(|e| format!("read PDF '{}' fallita: {e}", file_path.display()))?;
    let result = tokio::task::spawn_blocking(move || {
        pdf_extract::extract_text_from_mem(&bytes)
            .map_err(|e| format!("estrazione PDF fallita: {e}"))
    })
    .await
    .map_err(|e| format!("spawn_blocking fallita: {e}"))??;
    Ok(truncate_chars(result, max_chars))
}

/// Pre-extract testo da un DOCX (ZIP + word/document.xml). Tronca a `max_chars`.
pub async fn extract_docx_text_inline(file_path: &std::path::Path, max_chars: usize)
    -> Result<String, String>
{
    let bytes = tokio::fs::read(file_path).await
        .map_err(|e| format!("read DOCX '{}' fallita: {e}", file_path.display()))?;
    let result = tokio::task::spawn_blocking(move || extract_docx(&bytes)).await
        .map_err(|e| format!("spawn_blocking fallita: {e}"))??;
    let text = result.get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(truncate_chars(text, max_chars))
}

fn truncate_chars(mut s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    // Tronca su char boundary.
    let end_byte = s.char_indices()
        .nth(max_chars)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.truncate(end_byte);
    s.push_str("\n[... contenuto pre-extracted troncato ...]");
    s
}

// ──────────────────────────────────────────────────────────────────────────
// PDF
// ──────────────────────────────────────────────────────────────────────────

/// `nexus_extract_pdf_text(attachment_id, page_start?, page_end?)`.
pub(super) async fn tool_nexus_extract_pdf_text(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({ "error": "Parametro 'attachment_id' obbligatorio." }).to_string(),
    };
    let page_start = input.get("page_start").and_then(Value::as_u64);
    let page_end = input.get("page_end").and_then(Value::as_u64);

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let limits = attachment_settings::current(&ctx.db).await;
    let max_text = limits.pdf_max_text_bytes;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };

    let result = tokio::task::spawn_blocking(move || extract_pdf(&bytes, page_start, page_end, max_text)).await;

    match result {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => json!({ "error": e }).to_string(),
        Err(e) => json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    }
}

fn extract_pdf(
    bytes: &[u8],
    page_start: Option<u64>,
    page_end: Option<u64>,
    max_text: usize,
) -> Result<Value, String> {
    // pdf-extract espone `extract_text_from_mem` — utile ma non gestisce range.
    // Per il MVP estraiamo tutto poi tagliamo: la maggioranza dei PDF utente
    // sta sotto qualche centinaio di KB di testo.
    let text = pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| format!("estrazione PDF fallita: {e}"))?;

    // Conta pagine via euristica: pdf-extract usa form feed (\u{000C}) tra pagine.
    let pages: Vec<&str> = text.split('\u{000C}').collect();
    let total_pages = pages.len();
    let start_idx = page_start.unwrap_or(1).saturating_sub(1) as usize;
    let end_idx = page_end
        .map(|v| v as usize)
        .unwrap_or(total_pages)
        .min(total_pages);
    if start_idx >= end_idx {
        return Err(format!(
            "range pagine invalido: start={} end={} totale={}",
            start_idx + 1,
            end_idx,
            total_pages
        ));
    }

    let mut extracted = String::new();
    for page in &pages[start_idx..end_idx] {
        if extracted.len() >= max_text {
            break;
        }
        extracted.push_str(page);
        extracted.push('\n');
    }
    let truncated_size = extracted.len() > max_text;
    if truncated_size {
        extracted.truncate(max_text);
    }

    // Heuristica PDF scansionato: pochissimo testo / pagina.
    let is_scanned = !extracted.is_empty()
        && extracted.trim().len() < 50 * (end_idx - start_idx).max(1);
    let mut out = json!({
        "total_pages": total_pages,
        "pages_extracted": end_idx - start_idx,
        "truncated_by_size": truncated_size,
        "text": extracted,
    });
    if is_scanned {
        out["is_scanned_pdf"] = json!(true);
        out["hint"] =
            json!("PDF probabilmente scansionato (poco testo estratto). Usa un modello vision/OCR.");
    }
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────────────
// DOCX
// ──────────────────────────────────────────────────────────────────────────

/// `nexus_extract_docx_text(attachment_id)`.
pub(super) async fn tool_nexus_extract_docx_text(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({ "error": "Parametro 'attachment_id' obbligatorio." }).to_string(),
    };

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };

    let result = tokio::task::spawn_blocking(move || extract_docx(&bytes)).await;
    match result {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => json!({ "error": e }).to_string(),
        Err(e) => json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    }
}

fn extract_docx(bytes: &[u8]) -> Result<Value, String> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("apertura DOCX (zip) fallita: {e}"))?;
    let mut doc_xml = Vec::new();
    {
        let mut entry = archive
            .by_name("word/document.xml")
            .map_err(|e| format!("word/document.xml non trovato: {e}"))?;
        entry
            .read_to_end(&mut doc_xml)
            .map_err(|e| format!("lettura document.xml fallita: {e}"))?;
    }

    let mut xml = Reader::from_reader(doc_xml.as_slice());
    xml.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                if name.as_ref() == b"w:t" {
                    in_text = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                if name.as_ref() == b"w:t" {
                    in_text = false;
                } else if name.as_ref() == b"w:p" {
                    paragraphs.push(std::mem::take(&mut current));
                }
            }
            Ok(Event::Text(t)) if in_text => {
                if let Ok(s) = t.unescape() {
                    current.push_str(&s);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("parse XML fallito: {e}")),
            _ => {}
        }
        buf.clear();
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    let full_text = paragraphs.join("\n\n");
    Ok(json!({
        "paragraphs_count": paragraphs.len(),
        "text": full_text,
    }))
}

// ──────────────────────────────────────────────────────────────────────────
// XLSX
// ──────────────────────────────────────────────────────────────────────────

/// `nexus_extract_xlsx_data(attachment_id, sheet_name?)`.
pub(super) async fn tool_nexus_extract_xlsx_data(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({ "error": "Parametro 'attachment_id' obbligatorio." }).to_string(),
    };
    let sheet_name = input
        .get("sheet_name")
        .and_then(Value::as_str)
        .map(str::to_string);

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let limits = attachment_settings::current(&ctx.db).await;
    let max_rows = limits.xlsx_max_rows;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };
    let result = tokio::task::spawn_blocking(move || extract_xlsx(&bytes, sheet_name, max_rows)).await;
    match result {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => json!({ "error": e }).to_string(),
        Err(e) => json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    }
}

fn extract_xlsx(bytes: &[u8], sheet_name: Option<String>, max_rows: usize) -> Result<Value, String> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("apertura XLSX fallita: {e}"))?;

    // 1) sharedStrings (opzionale).
    let shared_strings = if let Ok(mut entry) = archive.by_name("xl/sharedStrings.xml") {
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("read sharedStrings fallita: {e}"))?;
        parse_shared_strings(&buf)?
    } else {
        Vec::new()
    };

    // 2) Scegli sheet.
    let sheet_path = sheet_name
        .as_deref()
        .map(|n| format!("xl/worksheets/{}.xml", n))
        .unwrap_or_else(|| "xl/worksheets/sheet1.xml".to_string());

    let mut sheet_xml = Vec::new();
    {
        let mut entry = archive
            .by_name(&sheet_path)
            .map_err(|e| format!("sheet '{sheet_path}' non trovato: {e}"))?;
        entry
            .read_to_end(&mut sheet_xml)
            .map_err(|e| format!("read sheet fallita: {e}"))?;
    }
    let rows = parse_worksheet(&sheet_xml, &shared_strings, max_rows)?;
    Ok(json!({
        "sheet": sheet_path,
        "rows_count": rows.len(),
        "rows": rows,
        "truncated": rows.len() >= max_rows,
    }))
}

fn parse_shared_strings(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut xml = Reader::from_reader(bytes);
    xml.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_t = false;
    loop {
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"t" => in_t = true,
            Ok(Event::End(e)) => {
                let name = e.name();
                if name.as_ref() == b"t" {
                    in_t = false;
                } else if name.as_ref() == b"si" {
                    out.push(std::mem::take(&mut current));
                }
            }
            Ok(Event::Text(t)) if in_t => {
                if let Ok(s) = t.unescape() {
                    current.push_str(&s);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("parse sharedStrings: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn parse_worksheet(
    bytes: &[u8],
    shared_strings: &[String],
    max_rows: usize,
) -> Result<Vec<Vec<String>>, String> {
    let mut xml = Reader::from_reader(bytes);
    xml.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut cell_type: Option<String> = None;
    let mut in_value = false;
    let mut value_buf = String::new();

    loop {
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"row" => current_row.clear(),
                b"c" => {
                    cell_type = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"t")
                        .and_then(|a| String::from_utf8(a.value.into_owned()).ok());
                }
                b"v" | b"t" => {
                    in_value = true;
                    value_buf.clear();
                }
                _ => {}
            },
            Ok(Event::Text(t)) if in_value => {
                if let Ok(s) = t.unescape() {
                    value_buf.push_str(&s);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"v" | b"t" => {
                    in_value = false;
                    let resolved = if cell_type.as_deref() == Some("s") {
                        value_buf
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared_strings.get(i).cloned())
                            .unwrap_or_else(|| value_buf.clone())
                    } else {
                        value_buf.clone()
                    };
                    current_row.push(resolved);
                }
                b"row" => {
                    if rows.len() >= max_rows {
                        // Continua a parsare senza accumulare per non perdere
                        // il conteggio totale (non strettamente necessario qui,
                        // ma e' utile per il client).
                    } else {
                        rows.push(std::mem::take(&mut current_row));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("parse worksheet: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(rows)
}
