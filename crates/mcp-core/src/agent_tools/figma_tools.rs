//! Tool `nexus_extract_figma_structure`.
//!
//! Supporta due formati:
//!
//! - **Figma Make** (estensione `.make`, ZIP che contiene `canvas.fig`,
//!   `ai_chat.json`, `meta.json`, `thumbnail.png`, `images/*`,
//!   `blob_store/*`, `make_binary_files/*`). Il contenuto **autoritativo** e'
//!   `ai_chat.json`: vi sono i messaggi user/assistant del thread con cui il
//!   design e' stato generato, quindi la specifica originale dell'app.
//! - **Figma legacy binario** (`.fig` puro o ZIP con solo `canvas.fig`).
//!   Nessun parser pubblico per il payload proprietario: fallback a
//!   estrazione strings ASCII + hint operativo.
//!
//! Pipeline dispatch (vedi ADR 0011 sezione "Figma Make handling"):
//!   1. apri ZIP (fallback: tratta il file intero come payload binario)
//!   2. cerca `ai_chat.json` + `meta.json` + `thumbnail.png` + `canvas.fig`
//!   3. se `ai_chat.json` presente → format=`figma_make`, parsing chat
//!   4. altrimenti se `canvas.fig` presente → format=`figma_binary_legacy`,
//!      strings fallback
//!   5. mai inghiottire errori in silenzio: ogni parse error e' segnalato.
//!
//! Niente nomi/limiti hardcoded: i parametri vivono in `attachment_settings`
//! (cache 60s, sorgente `settings` DB), vedi mig 0196.

use std::io::{Cursor, Read};

use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::load_attachment;
use super::attachment_settings::{self, AttachmentLimits};
use super::AgentToolContext;

/// Lunghezza minima di una stringa leggibile considerata "interessante"
/// nel fallback `figma_binary_legacy`.
const MIN_STRING_LEN: usize = 4;
/// Numero massimo di stringhe ritornate nel fallback.
const MAX_STRINGS: usize = 200;

/// `nexus_extract_figma_structure(attachment_id)`.
pub(super) async fn tool_nexus_extract_figma_structure(
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
    let limits = attachment_settings::current(&ctx.db).await;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };

    let result = tokio::task::spawn_blocking(move || extract_figma(&bytes, limits)).await;
    match result {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => json!({ "error": e }).to_string(),
        Err(e) => json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    }
}

/// Risultato della pipeline ZIP scan: indici delle entry chiave.
#[derive(Default)]
struct ArchiveIndex {
    ai_chat_idx: Option<usize>,
    meta_idx: Option<usize>,
    thumbnail_idx: Option<usize>,
    canvas_idx: Option<usize>,
    images_count: usize,
}

/// Punto di ingresso pipeline (vedi modulo doc).
fn extract_figma(bytes: &[u8], limits: AttachmentLimits) -> Result<Value, String> {
    // Caso 1: non-ZIP → fallback diretto su payload binario (file `.fig` raw).
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        let payload_max = limits.figma_max_bytes;
        let payload = &bytes[..bytes.len().min(payload_max)];
        return Ok(build_legacy_binary_result(payload));
    }

    // Caso 2: ZIP. Apri e indicizza entry note.
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let index = scan_archive(&mut archive);

    // Caso 2a: presenza di ai_chat.json → Figma Make.
    if let Some(ai_chat_idx) = index.ai_chat_idx {
        let meta = index
            .meta_idx
            .and_then(|i| read_entry_to_string(&mut archive, i, 64 * 1024).ok())
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());

        let ai_chat_bytes = read_entry_to_bytes(
            &mut archive,
            ai_chat_idx,
            limits.figma_make_ai_chat_max_load_bytes,
        )?;
        let ai_chat_truncated = ai_chat_bytes.truncated;

        let chat_result = parse_ai_chat(&ai_chat_bytes.bytes, &limits)?;

        let meta_summary = meta.as_ref().map(summarize_meta).unwrap_or(json!(null));
        let thumbnail_available = index.thumbnail_idx.is_some();
        let canvas_available = index.canvas_idx.is_some();

        Ok(json!({
            "format": "figma_make",
            "meta": meta_summary,
            "chat_messages": chat_result.messages,
            "chat_messages_count": chat_result.count,
            "chat_messages_truncated": chat_result.truncated,
            "ai_chat_truncated_at_load": ai_chat_truncated,
            "thumbnail_available": thumbnail_available,
            "thumbnail_hint": if thumbnail_available {
                Value::String(
                    "ZIP Figma Make contiene thumbnail.png ma non e' direttamente \
                     ispezionabile dai tool standard. Per analisi visiva chiedi \
                     all'utente di esportare il design da Figma come PNG separato \
                     e ricaricarlo come allegato immagine.".into(),
                )
            } else { Value::Null },
            "canvas_available": canvas_available,
            "images_count": index.images_count,
            "primary_content": "chat_messages",
            "hint": "Contenuto primario in 'chat_messages': qui c'e' la specifica \
                     originale dell'app data al generatore Figma Make (prompt user + \
                     risposte assistant). Usala come fonte di verita' per implementare \
                     l'applicazione richiesta. Il canvas.fig binario non e' parsabile \
                     pubblicamente; il thread chat e' la sorgente autoritativa.",
        }))
    } else if let Some(canvas_idx) = index.canvas_idx {
        // Caso 2b: solo canvas.fig → legacy binary fallback.
        let payload =
            read_entry_to_bytes(&mut archive, canvas_idx, limits.figma_max_bytes)?.bytes;
        let mut v = build_legacy_binary_result(&payload);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("extracted_strings_fallback".into(), Value::Bool(true));
        }
        Ok(v)
    } else {
        Err("Archivio Figma non riconosciuto: nessun ai_chat.json ne' canvas.fig al suo interno".into())
    }
}

/// Indicizza le entry rilevanti dell'archivio Figma Make.
fn scan_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> ArchiveIndex {
    let mut idx = ArchiveIndex::default();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_lowercase();
        if name == "ai_chat.json" || name.ends_with("/ai_chat.json") {
            idx.ai_chat_idx = Some(i);
        } else if name == "meta.json" || name.ends_with("/meta.json") {
            idx.meta_idx = Some(i);
        } else if name == "thumbnail.png" || name.ends_with("/thumbnail.png") {
            idx.thumbnail_idx = Some(i);
        } else if name == "canvas.fig" || name.ends_with("/canvas.fig") {
            idx.canvas_idx = Some(i);
        } else if name.starts_with("images/")
            || name.contains("/images/")
        {
            // conta solo file, non directory
            if !name.ends_with('/') {
                idx.images_count += 1;
            }
        }
    }
    idx
}

struct LoadedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_entry_to_bytes<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    idx: usize,
    cap: usize,
) -> Result<LoadedBytes, String> {
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| format!("apertura entry {idx} fallita: {e}"))?;
    let declared = entry.size() as usize;
    let truncated = declared > cap;
    let mut buf = Vec::with_capacity(cap.min(declared.saturating_add(1)));
    let mut reader = (&mut entry).take(cap as u64);
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("read entry {idx} fallita: {e}"))?;
    Ok(LoadedBytes { bytes: buf, truncated })
}

fn read_entry_to_string<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    idx: usize,
    cap: usize,
) -> Result<String, String> {
    let bytes = read_entry_to_bytes(archive, idx, cap)?.bytes;
    String::from_utf8(bytes).map_err(|e| format!("entry {idx} non UTF-8: {e}"))
}

/// Riassume `meta.json` (file_name, exported_at, render_coordinates) in un
/// oggetto JSON compatto. Tollera campi mancanti.
fn summarize_meta(meta: &Value) -> Value {
    let file_name = meta.get("file_name").cloned().unwrap_or(Value::Null);
    let exported_at = meta.get("exported_at").cloned().unwrap_or(Value::Null);
    let dimensions = meta
        .get("client_meta")
        .and_then(|cm| cm.get("render_coordinates"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "file_name": file_name,
        "exported_at": exported_at,
        "dimensions": dimensions,
    })
}

/// Messaggio chat AI Figma Make estratto.
#[derive(Debug, Clone, serde::Serialize)]
struct ChatMessage {
    role: String,
    text: String,
}

struct ChatParseResult {
    messages: Vec<ChatMessage>,
    count: usize,
    truncated: bool,
}

/// Parse di `ai_chat.json`. Struttura attesa:
/// `{ "threads": [ { "messages": [ { "role": "user|assistant",
///    "parts": [ { "partType": "text", "contentJson": "{\"text\":\"...\"}" } ] } ] } ] }`.
/// Tutti i livelli sono tollerati a mancare; un parse error top-level e' propagato.
fn parse_ai_chat(
    bytes: &[u8],
    limits: &AttachmentLimits,
) -> Result<ChatParseResult, String> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("parse ai_chat.json fallito: {e}"))?;

    let max_count = limits.figma_make_chat_messages_max_count;
    let max_chars = limits.figma_make_chat_messages_max_chars;
    let assistant_cap = limits.figma_make_assistant_message_max_chars;

    let mut out: Vec<ChatMessage> = Vec::new();
    let mut cumulative_chars: usize = 0;
    let mut truncated = false;

    let threads = root
        .get("threads")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    'outer: for thread in threads {
        let messages = thread
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for msg in messages {
            let role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if role != "user" && role != "assistant" {
                continue;
            }
            let parts = msg
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            let mut buf = String::new();
            for part in parts {
                let part_type = part
                    .get("partType")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if part_type != "text" {
                    continue;
                }
                let content_json = part.get("contentJson").and_then(Value::as_str);
                let Some(content_json) = content_json else {
                    continue;
                };
                // Il campo contentJson e' una stringa che a sua volta e'
                // JSON; estrai il sotto-campo `.text`.
                let inner: Result<Value, _> = serde_json::from_str(content_json);
                if let Ok(inner) = inner {
                    if let Some(text) = inner.get("text").and_then(Value::as_str) {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(text);
                    }
                }
            }

            if buf.is_empty() {
                continue;
            }

            // Truncatura per-messaggio: solo assistant (i user prompt sono
            // la sorgente autoritativa, non vanno mai persi a meta').
            if role == "assistant" && buf.chars().count() > assistant_cap {
                let truncated_text: String = buf.chars().take(assistant_cap).collect();
                buf = format!("{truncated_text}\n[... messaggio assistant troncato ...]");
            }

            let added = buf.len();
            if cumulative_chars.saturating_add(added) > max_chars {
                truncated = true;
                break 'outer;
            }
            cumulative_chars += added;
            out.push(ChatMessage { role, text: buf });

            if out.len() >= max_count {
                truncated = true;
                break 'outer;
            }
        }
    }

    let count = out.len();
    Ok(ChatParseResult {
        messages: out,
        count,
        truncated,
    })
}

/// Output JSON per fallback `figma_binary_legacy`.
fn build_legacy_binary_result(payload: &[u8]) -> Value {
    let strings = extract_readable_strings(payload, MIN_STRING_LEN, MAX_STRINGS);
    json!({
        "format": "figma_binary_legacy",
        "size_bytes": payload.len(),
        "extracted_strings": strings,
        "primary_content": "extracted_strings",
        "hint": "Formato Figma binario proprietario senza ai_chat.json. Per ottenere \
                 componenti/frame strutturati usa l'export Figma API \
                 (https://www.figma.com/developers/api) o un plugin Figma 'Figma to \
                 Code' / 'Figma to JSON'. Le stringhe estratte aiutano a inferire i \
                 nomi dei layer e degli stili presenti.",
    })
}

/// Pre-extract inline (FIX 3 ADR 0012): produce un testo formattato da
/// includere nel prompt iniziale, gia' tradotto in markdown leggibile.
/// La firma e' invariata per backward-compat con `chat_messages.rs`.
///
/// Per Figma Make: emette il thread chat (priorita' assoluta).
/// Per Figma legacy: emette le strings estratte.
pub async fn extract_figma_strings_inline(
    file_path: &std::path::Path,
    max_chars: usize,
) -> Result<String, String> {
    let bytes = tokio::fs::read(file_path)
        .await
        .map_err(|e| format!("read figma '{}' fallita: {e}", file_path.display()))?;

    // Limiti safe per la pre-extract: indipendenti dal DB (la pre-extract
    // gira in hot path al primo messaggio, non vogliamo aggiungere round-trip
    // a settings). Usiamo i safe_defaults della cache: valori identici a
    // quanto la cache servirebbe se il DB e' down.
    let limits = AttachmentLimits::safe_defaults();

    let result =
        tokio::task::spawn_blocking(move || extract_figma(&bytes, limits))
            .await
            .map_err(|e| format!("spawn_blocking fallita: {e}"))??;

    let text = render_inline(&result);
    Ok(truncate_to(text, max_chars))
}

/// Render testuale del result JSON per inclusione nel prompt.
fn render_inline(value: &Value) -> String {
    let format = value
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = String::new();

    match format {
        "figma_make" => {
            let meta = value.get("meta").cloned().unwrap_or(Value::Null);
            let file_name = meta
                .get("file_name")
                .and_then(Value::as_str)
                .unwrap_or("(senza nome)");
            let exported_at = meta
                .get("exported_at")
                .and_then(Value::as_str)
                .unwrap_or("(data sconosciuta)");
            out.push_str(&format!(
                "[FIGMA MAKE - file: {file_name} - esportato: {exported_at}]\n\n"
            ));
            out.push_str("Specifica originale dell'app (dal thread chat AI Figma Make):\n\n");

            let empty = Vec::new();
            let messages = value
                .get("chat_messages")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            if messages.is_empty() {
                out.push_str("(thread chat vuoto)\n");
            } else {
                for m in messages {
                    let role = m
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_uppercase();
                    let text = m.get("text").and_then(Value::as_str).unwrap_or("");
                    out.push_str(&format!("[{role}]\n{text}\n\n"));
                }
            }
            if value
                .get("chat_messages_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                out.push_str("[... thread chat troncato per budget ...]\n\n");
            }
            out.push_str("Note:\n");
            if value
                .get("thumbnail_available")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                out.push_str(
                    "- Thumbnail PNG presente nello ZIP (non direttamente ispezionabile: \
                     per analisi visiva chiedi all'utente di esportare il design come \
                     PNG separato e ricaricarlo).\n",
                );
            }
            let images = value
                .get("images_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if images > 0 {
                out.push_str(&format!("- {images} immagini asset nello ZIP.\n"));
            }
        }
        "figma_binary_legacy" | "figma_binary" => {
            out.push_str("[FIGMA legacy binario - canvas.fig opaco]\n\n");
            out.push_str("Stringhe leggibili estratte dal payload binario:\n\n");
            let empty = Vec::new();
            let strings = value
                .get("extracted_strings")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            for s in strings {
                if let Some(s) = s.as_str() {
                    out.push_str(s);
                    out.push('\n');
                }
            }
            if strings.is_empty() {
                out.push_str("(nessuna stringa leggibile)\n");
            }
        }
        _ => {
            out.push_str("(formato Figma sconosciuto)\n");
        }
    }
    out
}

fn truncate_to(mut s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s;
    }
    // Trunca a confine UTF-8.
    let mut cut = max_chars;
    while !s.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[... pre-extract Figma troncata ...]");
    s
}

/// Estrae sequenze di byte ASCII stampabili (>= `min_len`).
fn extract_readable_strings(bytes: &[u8], min_len: usize, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for &b in bytes {
        let printable = b == b'\n' || b == b'\t' || (b >= 0x20 && b < 0x7F);
        if printable {
            current.push(b as char);
        } else if current.len() >= min_len {
            out.push(std::mem::take(&mut current));
            if out.len() >= max {
                break;
            }
        } else {
            current.clear();
        }
    }
    if current.len() >= min_len && out.len() < max {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_ai_chat_zip(ai_chat_body: &str, include_meta: bool, include_thumb: bool) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: SimpleFileOptions = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("ai_chat.json", opts).unwrap();
            zw.write_all(ai_chat_body.as_bytes()).unwrap();
            if include_meta {
                zw.start_file("meta.json", opts).unwrap();
                zw.write_all(
                    br#"{"client_meta":{"render_coordinates":{"w":1024,"h":768}},"file_name":"PL","exported_at":"2026-05-27T10:00:00Z"}"#,
                )
                .unwrap();
            }
            if include_thumb {
                zw.start_file("thumbnail.png", opts).unwrap();
                zw.write_all(&[0x89, 0x50, 0x4E, 0x47]).unwrap();
            }
            zw.start_file("canvas.fig", opts).unwrap();
            zw.write_all(b"fig-makej\x00binary opaque content here").unwrap();
            zw.start_file("images/cover.png", opts).unwrap();
            zw.write_all(&[0xFF; 16]).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    fn limits() -> AttachmentLimits {
        AttachmentLimits::safe_defaults()
    }

    #[test]
    fn figma_make_parses_user_and_assistant_messages() {
        let body = r#"{
            "threads": [{
                "id": "t1",
                "messages": [
                    {"role":"user","parts":[{"partType":"text","contentJson":"{\"text\":\"Voglio app di booking\"}"}]},
                    {"role":"assistant","parts":[{"partType":"text","contentJson":"{\"text\":\"Ok, ti propongo X\"}"}]},
                    {"role":"system","parts":[{"partType":"text","contentJson":"{\"text\":\"ignored\"}"}]}
                ]
            }]
        }"#;
        let zip = make_ai_chat_zip(body, true, true);
        let v = extract_figma(&zip, limits()).expect("extract");
        assert_eq!(v["format"], "figma_make");
        assert_eq!(v["chat_messages_count"], 2);
        assert_eq!(v["chat_messages"][0]["role"], "user");
        assert_eq!(v["chat_messages"][0]["text"], "Voglio app di booking");
        assert_eq!(v["chat_messages"][1]["role"], "assistant");
        assert_eq!(v["thumbnail_available"], true);
        assert_eq!(v["canvas_available"], true);
        assert_eq!(v["images_count"], 1);
        assert_eq!(v["meta"]["file_name"], "PL");
    }

    #[test]
    fn figma_make_truncates_by_count() {
        let mut msgs = String::new();
        for i in 0..40 {
            msgs.push_str(&format!(
                r#"{{"role":"user","parts":[{{"partType":"text","contentJson":"{{\"text\":\"msg{i}\"}}"}}]}},"#
            ));
        }
        msgs.pop();
        let body = format!(r#"{{"threads":[{{"messages":[{msgs}]}}]}}"#);
        let zip = make_ai_chat_zip(&body, false, false);
        let mut l = limits();
        l.figma_make_chat_messages_max_count = 5;
        let v = extract_figma(&zip, l).expect("extract");
        assert_eq!(v["chat_messages_count"], 5);
        assert_eq!(v["chat_messages_truncated"], true);
    }

    #[test]
    fn figma_make_assistant_message_capped() {
        let long = "x".repeat(5000);
        let body = format!(
            r#"{{"threads":[{{"messages":[
                {{"role":"assistant","parts":[{{"partType":"text","contentJson":"{{\"text\":\"{long}\"}}"}}]}}
            ]}}]}}"#
        );
        let zip = make_ai_chat_zip(&body, false, false);
        let mut l = limits();
        l.figma_make_assistant_message_max_chars = 100;
        let v = extract_figma(&zip, l).expect("extract");
        let text = v["chat_messages"][0]["text"].as_str().unwrap();
        assert!(text.len() < 500, "assistant truncated, got {} chars", text.len());
        assert!(text.contains("troncato"));
    }

    #[test]
    fn legacy_binary_no_ai_chat() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: SimpleFileOptions =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("canvas.fig", opts).unwrap();
            zw.write_all(b"PROPRIETARY_BINARY_HEADER_with_readable_text_here").unwrap();
            zw.finish().unwrap();
        }
        let v = extract_figma(&buf, limits()).expect("extract");
        assert_eq!(v["format"], "figma_binary_legacy");
        assert_eq!(v["extracted_strings_fallback"], true);
        assert!(v["extracted_strings"].as_array().unwrap().len() >= 1);
    }

    #[test]
    fn raw_non_zip_falls_back() {
        let raw = b"NOT_A_ZIP_just_raw_payload_with_strings_visible_inside";
        let v = extract_figma(raw, limits()).expect("extract");
        assert_eq!(v["format"], "figma_binary_legacy");
    }

    #[test]
    fn empty_archive_errors() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            zw.start_file(
                "irrelevant.txt",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zw.write_all(b"hi").unwrap();
            zw.finish().unwrap();
        }
        let err = extract_figma(&buf, limits()).unwrap_err();
        assert!(err.contains("Archivio Figma non riconosciuto"));
    }

    #[test]
    fn render_inline_figma_make_includes_user_prompt() {
        let body = r#"{
            "threads":[{"messages":[
                {"role":"user","parts":[{"partType":"text","contentJson":"{\"text\":\"prompt utente\"}"}]}
            ]}]
        }"#;
        let zip = make_ai_chat_zip(body, true, false);
        let v = extract_figma(&zip, limits()).expect("extract");
        let text = render_inline(&v);
        assert!(text.contains("FIGMA MAKE"));
        assert!(text.contains("[USER]"));
        assert!(text.contains("prompt utente"));
        assert!(text.contains("file: PL"));
    }
}
