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
use crate::projects::resolve_workspace_target;

/// Lunghezza minima di una stringa leggibile considerata "interessante"
/// nel fallback `figma_binary_legacy`.
const MIN_STRING_LEN: usize = 4;

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

/// Subdir di default sotto la project_root dove scrivere il code-snapshot
/// estratto. Tenuta separata da eventuale scaffold per non collidere.
const DEFAULT_FIGMA_EXPORT_SUBDIR: &str = "figma_export";

/// `nexus_extract_figma_code(attachment_id, target_subdir?)`.
///
/// Estrae il code-snapshot finale dal .make e lo scrive su disco sotto la
/// project_root (default `figma_export/`). Ritorna SOLO un manifest JSON con
/// metadati: niente contenuto file, per non saturare il contesto del modello.
pub(super) async fn tool_nexus_extract_figma_code(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    if !ctx.can_write {
        return json!({
            "error": "Permesso di scrittura non concesso su questo progetto: \
                      impossibile estrarre il code-snapshot Figma su disco."
        })
        .to_string();
    }

    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({ "error": "Parametro 'attachment_id' obbligatorio." }).to_string(),
    };

    let target_subdir = input
        .get("target_subdir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_FIGMA_EXPORT_SUBDIR)
        .trim_matches('/')
        .to_string();

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };
    let limits = attachment_settings::current(&ctx.db).await;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };

    // Parsing pesante in spawn_blocking: produce la mappa path->content.
    let snapshot = match tokio::task::spawn_blocking(move || extract_code_from_make(&bytes, limits))
        .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return json!({ "error": e }).to_string(),
        Err(e) => return json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    };

    if snapshot.files.is_empty() {
        return json!({
            "format": "figma_make_code",
            "total_files": 0,
            "files_written": [],
            "target_dir": target_subdir,
            "partial": snapshot.partial,
            "notes": "Nessuna scrittura file (fast_apply_tool/write_tool) trovata nel \
                      thread ai_chat.json: questo .make non contiene un code-snapshot \
                      ricostruibile. Usa nexus_extract_figma_structure per la specifica \
                      chat e implementa l'app dalla descrizione.",
        })
        .to_string();
    }

    // Calcolo dipendenze ed entrypoint prima di scrivere (sui content in RAM).
    let detected_dependencies = detect_dependencies(&snapshot.files);
    let entrypoints = detect_entrypoints(&snapshot.files);

    // Scrittura su disco con guardia path-safety del workspace.
    // Politica "mai troncare-e-buttare": scriviamo TUTTI i file estratti,
    // qualunque sia la dimensione totale (nessun cap sui byte scritti).
    let mut files_written: Vec<Value> = Vec::new();
    let mut total_bytes: usize = 0;
    let mut rejected_paths = false;

    for (rel_path, content) in &snapshot.files {
        let content_len = content.len();

        let joined = format!("{target_subdir}/{rel_path}");
        let (clean_rel, abs_target) = match resolve_workspace_target(&ctx.root_path, &joined) {
            Ok(p) => p,
            Err(_) => {
                // Path rifiutato dalla guardia path-safety: lo salto e segnalo
                // il risultato come parziale.
                rejected_paths = true;
                continue;
            }
        };

        if let Some(parent) = abs_target.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return json!({
                    "error": format!("creazione directory '{}' fallita: {e}", parent.display())
                })
                .to_string();
            }
        }

        if let Err(e) = tokio::fs::write(&abs_target, content).await {
            return json!({
                "error": format!("scrittura '{clean_rel}' fallita: {e}")
            })
            .to_string();
        }

        nexus_events::dispatcher::emit(
            &ctx.project_channels,
            ctx.project_id,
            nexus_events::event::ProjectEvent::FileChanged {
                path: clean_rel.clone(),
                op: "created".to_string(),
            },
        );

        total_bytes += content_len;
        files_written.push(json!({ "path": clean_rel, "bytes": content_len }));
    }

    let partial = snapshot.partial || rejected_paths;
    let mut notes = String::from(
        "Code-snapshot React/TS estratto dal .make e scritto su disco: NON e' \
         incluso nel contesto (solo metadati qui). Leggi i file con read_file \
         quando ti servono. Genera package.json da detected_dependencies.",
    );
    if rejected_paths {
        notes.push_str(
            " ATTENZIONE: alcuni path sono stati rifiutati dalla guardia di \
             path-safety del workspace e non sono stati scritti.",
        );
    }
    if snapshot.partial {
        notes.push_str(
            " ai_chat.json ha superato la guardia anti-OOM al load (file \
             patologico): il code-snapshot potrebbe essere incompleto.",
        );
    }

    json!({
        "format": "figma_make_code",
        "files_written": files_written,
        "total_files": files_written.len(),
        "total_bytes": total_bytes,
        "target_dir": target_subdir,
        "entrypoints": entrypoints,
        "detected_dependencies": detected_dependencies,
        "partial": partial,
        "notes": notes,
    })
    .to_string()
}

/// Apre il .make, carica `ai_chat.json` (rispettando il limite di load),
/// e ricostruisce il code-snapshot. Errori ZIP/entry sono propagati.
fn extract_code_from_make(bytes: &[u8], limits: AttachmentLimits) -> Result<CodeSnapshot, String> {
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return Err(
            "Il file non e' uno ZIP: un .make Figma valido e' un archivio ZIP che \
             contiene ai_chat.json."
                .into(),
        );
    }
    let reader = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let index = scan_archive(&mut archive);

    let ai_chat_idx = index.ai_chat_idx.ok_or_else(|| {
        "Archivio Figma senza ai_chat.json: nessun code-snapshot estraibile.".to_string()
    })?;

    let loaded = read_entry_to_bytes(
        &mut archive,
        ai_chat_idx,
        limits.figma_make_ai_chat_max_load_bytes,
    )?;

    // Parsing tollerante: se il JSON e' troncato (load cap raggiunto),
    // serde fallisce sul documento intero. In quel caso tentiamo comunque di
    // estrarre i file gia' completi prima del troncamento via parsing parziale.
    match serde_json::from_slice::<Value>(&loaded.bytes) {
        Ok(root) => Ok(extract_make_code_snapshot(&root, loaded.truncated)),
        Err(_) if loaded.truncated => {
            // JSON troncato: estrazione best-effort dai contentJson trovabili.
            Ok(extract_snapshot_from_truncated(&loaded.bytes))
        }
        Err(e) => Err(format!("parse ai_chat.json fallito: {e}")),
    }
}

/// Estrazione best-effort da un `ai_chat.json` troncato (non JSON-valido nel
/// suo complesso). Non potendo parsare il documento, isoliamo i singoli
/// `contentJson` di tipo scrittura file cercando i marcatori dei tool noti e
/// ricostruiamo i file che riusciamo a leggere interamente. Sempre `partial`.
fn extract_snapshot_from_truncated(bytes: &[u8]) -> CodeSnapshot {
    let mut snap = CodeSnapshot {
        partial: true,
        ..Default::default()
    };
    let text = String::from_utf8_lossy(bytes);

    // Avvolgiamo i frammenti riconoscibili: ogni messaggio di scrittura ha la
    // forma escaped {\"toolName\":\"fast_apply_tool\",...,\"resultJson\":\"...\"}.
    // Cerchiamo le occorrenze di resultJson e proviamo a parsare il singolo
    // oggetto outer che le contiene. Strategia conservativa: se non riusciamo,
    // semplicemente lasciamo i file gia' raccolti.
    for tool in FILE_WRITE_TOOL_NAMES {
        let marker = format!("\\\"toolName\\\":\\\"{tool}\\\"");
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find(&marker) {
            let abs = search_from + rel;
            // Cerca l'inizio dell'oggetto outer (la graffa escaped precedente).
            // I contentJson sono stringhe escaped: tentiamo un un-escape locale
            // di una finestra ampia e un parse dell'oggetto.
            let window_start = text[..abs].rfind("\"{").map(|p| p + 1).unwrap_or(abs);
            let window_end = (abs + 200_000).min(text.len());
            let window = &text[window_start..window_end];
            if let Some(content_json) = recover_content_json(window) {
                if let Ok(outer) = serde_json::from_str::<Value>(&content_json) {
                    apply_outer_tool_write(&mut snap, &outer);
                }
            }
            search_from = abs + marker.len();
        }
    }
    snap
}

/// Tenta di isolare e un-escapare una stringa `contentJson` da una finestra di
/// testo grezzo. Ritorna il JSON interno (gia' un-escaped) se la finestra
/// contiene una stringa JSON completa terminata correttamente.
fn recover_content_json(window: &str) -> Option<String> {
    // La finestra parte (idealmente) da `{\"toolCallId\...}` escaped dentro una
    // stringa JSON. Troviamo la prima `{` e ricostruiamo bilanciando le graffe
    // tenendo conto dell'escaping. Approccio semplice: un-escape `\"`->`"` e
    // `\\`->`\`, poi bilanciamo le graffe sul testo un-escaped.
    let start = window.find('{')?;
    let candidate = &window[start..];
    let unescaped = candidate.replace("\\\"", "\"").replace("\\\\", "\\");

    let mut depth = 0usize;
    let mut in_str = false;
    let mut prev_escape = false;
    for (i, ch) in unescaped.char_indices() {
        match ch {
            '"' if !prev_escape => in_str = !in_str,
            '{' if !in_str => depth += 1,
            '}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(unescaped[..=i].to_string());
                }
            }
            _ => {}
        }
        prev_escape = ch == '\\' && !prev_escape;
    }
    None
}

/// Applica un oggetto outer (gia' parsato) di tipo scrittura file allo
/// snapshot, condividendo la logica con `extract_make_code_snapshot`.
fn apply_outer_tool_write(snap: &mut CodeSnapshot, outer: &Value) {
    let Some((raw_path, content)) = extract_write_from_outer(outer) else {
        return;
    };
    let Some(norm_path) = normalize_snapshot_path(&raw_path) else {
        return;
    };
    snap.files.insert(norm_path, content);
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
    // Politica "mai troncare-e-buttare": processiamo l'INTERO payload.
    if bytes.len() < 4 || &bytes[0..4] != b"PK\x03\x04" {
        return Ok(build_legacy_binary_result(bytes));
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

        let chat_result = parse_ai_chat(&ai_chat_bytes.bytes)?;

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
        // Politica "mai troncare-e-buttare": leggiamo l'INTERA entry; il cap
        // passato e' solo la guardia anti-OOM estrema (non un cap di contenuto).
        let payload =
            read_entry_to_bytes(&mut archive, canvas_idx, limits.figma_make_ai_chat_max_load_bytes)?
                .bytes;
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
fn parse_ai_chat(bytes: &[u8]) -> Result<ChatParseResult, String> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("parse ai_chat.json fallito: {e}"))?;

    // Politica "mai troncare-e-buttare": estraiamo TUTTI i messaggi (user +
    // assistant) per intero, nessun cap su numero/caratteri.
    let mut out: Vec<ChatMessage> = Vec::new();

    let threads = root
        .get("threads")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for thread in threads {
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

            // Nessun cap: il messaggio (user o assistant) viene preso per intero.
            out.push(ChatMessage { role, text: buf });
        }
    }

    let count = out.len();
    Ok(ChatParseResult {
        messages: out,
        count,
        // Politica "mai troncare-e-buttare": il thread non viene mai troncato.
        truncated: false,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// FASE 1 "resa Figma Make" — estrazione del code-snapshot finale.
//
// Un .make NON contiene solo la specifica chat: dentro `ai_chat.json` ci sono
// gia' tutte le scritture file dell'app React/TS/Tailwind, salvate come
// sequenza di chiamate-tool nei messaggi (`fast_apply_tool`, `write_tool`,
// ecc.). Ricostruendo l'ultima versione per ogni path si ottiene il
// filesystem finale dell'app. Lo scriviamo su disco (non nel contesto del
// modello) per non saturare la context window con ~289 KB di codice.
// ─────────────────────────────────────────────────────────────────────────

/// Code-snapshot ricostruito dal thread: path relativo -> contenuto finale.
/// L'ordine dei messaggi nel JSON e' cronologico, quindi l'ultima scrittura
/// per ogni path vince (edit log incrementale).
#[derive(Default)]
struct CodeSnapshot {
    /// path normalizzato (senza leading "/") -> contenuto.
    files: std::collections::BTreeMap<String, String>,
    /// true se il parsing si e' fermato perche' ai_chat era troncato al load
    /// o per limiti: il manifest segnala il risultato come parziale.
    partial: bool,
}

/// `toolName` noti che rappresentano una scrittura file con `resultJson.content`.
const FILE_WRITE_TOOL_NAMES: &[&str] = &[
    "fast_apply_tool",
    "edit_tool",
    "write_tool",
    "create_file_tool",
    "write_file_tool",
    "edit_file_tool",
];

/// Estrae il path dal campo `message` tipo
/// "Successfully updated the file at /src/app/X.tsx." -> "/src/app/X.tsx".
/// Riconosce piu' marker emessi dai tool Figma Make:
/// "file at ", "created file ", "rewrote file ", "updated file ", "wrote file ".
/// Ritorna `None` se nessun pattern e' presente o il path appare sporco.
fn parse_path_from_message(message: &str) -> Option<String> {
    // Marker noti (case-insensitive). "file at " resta prioritario per
    // compatibilita' col formato fast_apply_tool gia' gestito.
    const MARKERS: &[&str] = &[
        "file at ",
        "created file ",
        "rewrote file ",
        "updated file ",
        "wrote file ",
    ];
    let lower = message.to_lowercase();
    // Scegli il marker il cui contenuto-path inizia per primo nel messaggio.
    let mut after_end: Option<usize> = None;
    for marker in MARKERS {
        if let Some(pos) = lower.find(marker) {
            let end = pos + marker.len();
            if after_end.is_none_or(|b| end < b) {
                after_end = Some(end);
            }
        }
    }
    let after_pos = after_end?;
    let after = &message[after_pos..];
    // Il path termina al primo newline; togliamo eventuale punto finale e spazi.
    let raw = after.lines().next().unwrap_or(after).trim();
    let raw = raw.trim_end_matches('.').trim();
    if raw.is_empty() {
        return None;
    }
    // Scarta path sporchi: spazi interni (i path validi non ne hanno), frasi
    // di fallback note, caratteri di controllo.
    if raw.contains(' ')
        || raw.contains('\t')
        || raw.to_lowercase().contains("make sure")
        || raw.to_lowercase().contains("fall back")
        || raw.chars().any(|c| c.is_control())
    {
        return None;
    }
    Some(raw.to_string())
}

/// Estrae `(raw_path, content)` da un oggetto outer (gia' parsato) che
/// rappresenta una scrittura file in un thread Figma Make. Tenta in ordine:
///
/// - **Formato A** (`fast_apply_tool`, `edit_tool`): outer ha `resultJson`
///   (stringa JSON) = `{success, message, content}`; path dal `message`,
///   contenuto da `content`.
/// - **Formato B** (`write_tool`, `create_file_tool`): outer NON ha
///   `resultJson` ma `argsJson` (stringa JSON) = `{path, file_text}`.
///
/// La normalizzazione del path resta a carico del chiamante.
fn extract_write_from_outer(outer: &Value) -> Option<(String, String)> {
    let tool_name = outer.get("toolName").and_then(Value::as_str).unwrap_or("");
    if !FILE_WRITE_TOOL_NAMES.contains(&tool_name) {
        return None;
    }

    // Formato A: resultJson.{success, message, content}.
    if let Some(result_str) = outer.get("resultJson").and_then(Value::as_str) {
        if let Ok(result) = serde_json::from_str::<Value>(result_str) {
            // success: se presente deve essere true; se assente, tolleriamo.
            let not_failed = result.get("success").and_then(Value::as_bool) != Some(false);
            if not_failed {
                let content = result.get("content").and_then(Value::as_str).unwrap_or("");
                let message = result.get("message").and_then(Value::as_str).unwrap_or("");
                if !content.trim().is_empty() {
                    if let Some(raw_path) = parse_path_from_message(message) {
                        return Some((raw_path, content.to_string()));
                    }
                }
            }
        }
    }

    // Formato B: argsJson.{path|file|filename, file_text|content|text}.
    if let Some(args_str) = outer.get("argsJson").and_then(Value::as_str) {
        if let Ok(args) = serde_json::from_str::<Value>(args_str) {
            let raw_path = ["path", "file", "filename"]
                .iter()
                .filter_map(|k| args.get(*k).and_then(Value::as_str))
                .find(|s| !s.trim().is_empty());
            let content = ["file_text", "content", "text"]
                .iter()
                .filter_map(|k| args.get(*k).and_then(Value::as_str))
                .find(|s| !s.trim().is_empty());
            if let (Some(raw_path), Some(content)) = (raw_path, content) {
                return Some((raw_path.to_string(), content.to_string()));
            }
        }
    }

    None
}

/// Normalizza un path: rimuove leading "/" e backslash, mantiene la struttura
/// relativa. Ritorna `None` se diventa vuoto o tenta path traversal.
fn normalize_snapshot_path(raw: &str) -> Option<String> {
    let cleaned = raw.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches('/').trim();
    if cleaned.is_empty() {
        return None;
    }
    // Rifiuta traversal: la guardia di scrittura lo bloccherebbe comunque, ma
    // scartarlo qui tiene il manifest pulito.
    if cleaned.split('/').any(|seg| seg == ".." || seg == ".") {
        return None;
    }
    Some(cleaned.to_string())
}

/// Itera i messaggi del thread e ricostruisce il filesystem finale.
/// Tollerante a JSON parziale: ogni livello mancante viene saltato senza
/// errore (un `ai_chat.json` troncato al load produce comunque i file
/// completi gia' incontrati prima del troncamento).
fn extract_make_code_snapshot(ai_chat: &Value, ai_chat_truncated: bool) -> CodeSnapshot {
    let mut snap = CodeSnapshot {
        partial: ai_chat_truncated,
        ..Default::default()
    };

    let Some(threads) = ai_chat.get("threads").and_then(Value::as_array) else {
        return snap;
    };

    for thread in threads {
        let Some(messages) = thread.get("messages").and_then(Value::as_array) else {
            continue;
        };
        for msg in messages {
            let Some(parts) = msg.get("parts").and_then(Value::as_array) else {
                continue;
            };
            for part in parts {
                let Some(content_json) = part.get("contentJson").and_then(Value::as_str) else {
                    continue;
                };
                // contentJson e' una STRINGA che contiene JSON.
                let Ok(outer) = serde_json::from_str::<Value>(content_json) else {
                    continue;
                };
                // Estrai (path, content) provando Formato A (resultJson) poi
                // Formato B (argsJson). La normalizzazione resta qui.
                let Some((raw_path, content)) = extract_write_from_outer(&outer) else {
                    continue;
                };
                let Some(norm_path) = normalize_snapshot_path(&raw_path) else {
                    continue;
                };
                // L'ultima occorrenza vince (insert sovrascrive).
                snap.files.insert(norm_path, content);
            }
        }
    }

    snap
}

/// Scansiona i `content` estratti per individuare i package npm esterni
/// importati. Esclude import relativi (`./`, `../`) e bare path locali.
fn detect_dependencies(files: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut deps: BTreeSet<String> = BTreeSet::new();

    for content in files.values() {
        for line in content.lines() {
            let trimmed = line.trim_start();
            // Match grezzo ma robusto su `from "<pkg>"` / `from '<pkg>'`.
            if let Some(pkg) = extract_import_source(trimmed) {
                if pkg.starts_with('.') || pkg.starts_with('/') {
                    continue;
                }
                if let Some(name) = package_root_name(&pkg) {
                    deps.insert(name);
                }
            }
        }
    }
    deps.into_iter().collect()
}

/// Estrae la sorgente di un import/require da una riga: il contenuto tra
/// virgolette dopo `from` o dentro `require(...)` / `import(...)`.
fn extract_import_source(line: &str) -> Option<String> {
    let needle = if line.contains(" from ") {
        " from "
    } else if line.starts_with("import ") && line.contains('"') || line.contains('\'') {
        // forme `import "pkg"` / dynamic import / require
        ""
    } else {
        return None;
    };

    let rest = if needle.is_empty() {
        line
    } else {
        line.split(needle).nth(1)?
    };
    // Trova la prima stringa tra ' o ".
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] as char != quote {
                j += 1;
            }
            if j <= bytes.len() {
                return Some(rest[start..j].to_string());
            }
        }
        i += 1;
    }
    None
}

/// Riduce un import specifier al nome del package npm radice.
/// `lucide-react/icons` -> `lucide-react`; `@scope/pkg/sub` -> `@scope/pkg`.
fn package_root_name(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let parts: Vec<&str> = spec.split('/').collect();
    let name = if spec.starts_with('@') {
        if parts.len() >= 2 {
            format!("{}/{}", parts[0], parts[1])
        } else {
            parts[0].to_string()
        }
    } else {
        parts[0].to_string()
    };
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Individua gli entrypoint noti tra i path estratti.
fn detect_entrypoints(files: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    const ENTRY_BASENAMES: &[&str] = &[
        "app.tsx",
        "app.jsx",
        "main.tsx",
        "main.jsx",
        "index.tsx",
        "index.jsx",
        "routes.tsx",
        "routes.jsx",
        "index.html",
    ];
    let mut out: Vec<String> = Vec::new();
    for path in files.keys() {
        let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        if ENTRY_BASENAMES.contains(&base.as_str()) {
            out.push(path.clone());
        }
    }
    out
}

/// Output JSON per fallback `figma_binary_legacy`.
fn build_legacy_binary_result(payload: &[u8]) -> Value {
    // Politica "mai troncare-e-buttare": estraiamo TUTTE le stringhe leggibili.
    let strings = extract_readable_strings(payload, MIN_STRING_LEN);
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

/// Estrazione inline per indicizzazione RAG: produce un testo formattato in
/// markdown leggibile dal contenuto Figma.
///
/// Per Figma Make: emette il thread chat (priorita' assoluta).
/// Per Figma legacy: emette le strings estratte.
///
/// Politica "mai troncare-e-buttare": restituisce il render INTEGRALE; il
/// chunking lato RAG indicizza tutto il contenuto.
pub async fn extract_figma_strings_inline(
    file_path: &std::path::Path,
) -> Result<String, String> {
    let bytes = tokio::fs::read(file_path)
        .await
        .map_err(|e| format!("read figma '{}' fallita: {e}", file_path.display()))?;

    // Parametri safe indipendenti dal DB (hot path al primo messaggio): usiamo i
    // safe_defaults, che contengono solo la guardia anti-OOM (non cap contenuto).
    let limits = AttachmentLimits::safe_defaults();

    let result = tokio::task::spawn_blocking(move || extract_figma(&bytes, limits))
        .await
        .map_err(|e| format!("spawn_blocking fallita: {e}"))??;

    Ok(render_inline(&result))
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

/// Estrae sequenze di byte ASCII stampabili (>= `min_len`).
/// Politica "mai troncare-e-buttare": restituisce TUTTE le stringhe leggibili,
/// nessun cap sul numero.
fn extract_readable_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for &b in bytes {
        let printable = b == b'\n' || b == b'\t' || (b >= 0x20 && b < 0x7F);
        if printable {
            current.push(b as char);
        } else if current.len() >= min_len {
            out.push(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    if current.len() >= min_len {
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
    fn figma_make_extracts_all_messages_no_count_cap() {
        // Politica "mai troncare-e-buttare": tutti i 40 messaggi devono uscire,
        // mai un cap di conteggio.
        let mut msgs = String::new();
        for i in 0..40 {
            msgs.push_str(&format!(
                r#"{{"role":"user","parts":[{{"partType":"text","contentJson":"{{\"text\":\"msg{i}\"}}"}}]}},"#
            ));
        }
        msgs.pop();
        let body = format!(r#"{{"threads":[{{"messages":[{msgs}]}}]}}"#);
        let zip = make_ai_chat_zip(&body, false, false);
        let v = extract_figma(&zip, limits()).expect("extract");
        assert_eq!(v["chat_messages_count"], 40);
        assert_eq!(v["chat_messages_truncated"], false);
    }

    #[test]
    fn figma_make_assistant_message_not_capped() {
        // Politica "mai troncare-e-buttare": il messaggio assistant lungo deve
        // uscire INTEGRALE, nessuna truncatura per-messaggio.
        let long = "x".repeat(5000);
        let body = format!(
            r#"{{"threads":[{{"messages":[
                {{"role":"assistant","parts":[{{"partType":"text","contentJson":"{{\"text\":\"{long}\"}}"}}]}}
            ]}}]}}"#
        );
        let zip = make_ai_chat_zip(&body, false, false);
        let v = extract_figma(&zip, limits()).expect("extract");
        let text = v["chat_messages"][0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 5000, "assistant integrale, niente cap");
        assert!(!text.contains("troncato"));
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

    // ── FASE 1: estrazione code-snapshot ──────────────────────────────────

    /// Costruisce il `contentJson` (stringa JSON) di una scrittura file
    /// fast_apply_tool, con `resultJson` (stringa JSON) annidata.
    fn file_write_part(tool: &str, path: &str, content: &str, success: bool) -> String {
        let result = serde_json::json!({
            "success": success,
            "message": format!("Successfully updated the file at {path}."),
            "content": content,
        })
        .to_string();
        let outer = serde_json::json!({
            "toolCallId": "tc-1",
            "toolName": tool,
            "resultJson": result,
        })
        .to_string();
        // Il part wrappa contentJson come stringa.
        serde_json::json!({ "contentJson": outer }).to_string()
    }

    fn make_code_ai_chat() -> String {
        // Ordine cronologico: v1 di App.tsx, poi BookingPage, poi v2 di App.tsx
        // (deve vincere), poi un path sporco (da scartare), poi content vuoto.
        let p1 = file_write_part(
            "fast_apply_tool",
            "/src/app/App.tsx",
            "export default function App(){ return null }",
            true,
        );
        let p2 = file_write_part(
            "fast_apply_tool",
            "/src/app/pages/BookingPage.tsx",
            "import { useState } from \"react\";\nimport { toast } from \"sonner\";\nexport function BookingPage(){}",
            true,
        );
        let p3 = file_write_part(
            "write_tool",
            "/src/app/App.tsx",
            "import React from \"react\";\nimport { Routes } from \"./routes\";\nexport default function App(){ return <Routes/> }",
            true,
        );
        let dirty = file_write_part(
            "fast_apply_tool",
            "/src/app/Bad.tsx Make sure to fall back",
            "should be discarded",
            true,
        );
        let empty = file_write_part("fast_apply_tool", "/src/app/Empty.tsx", "   ", true);
        format!(
            r#"{{"threads":[{{"messages":[
                {{"parts":[{p1}]}},
                {{"parts":[{p2}]}},
                {{"parts":[{p3}]}},
                {{"parts":[{dirty}]}},
                {{"parts":[{empty}]}}
            ]}}]}}"#
        )
    }

    #[test]
    fn extract_snapshot_keeps_last_version_and_discards_dirty() {
        let body = make_code_ai_chat();
        let root: Value = serde_json::from_str(&body).expect("ai_chat valido");
        let snap = extract_make_code_snapshot(&root, false);

        // App.tsx presente con la SECONDA versione (write_tool), BookingPage
        // presente, path sporco e content vuoto scartati.
        assert_eq!(snap.files.len(), 2, "attesi 2 file, trovati {:?}", snap.files.keys().collect::<Vec<_>>());
        let app = snap.files.get("src/app/App.tsx").expect("App.tsx presente");
        assert!(app.contains("Routes"), "deve vincere la v2 (write_tool)");
        assert!(snap.files.contains_key("src/app/pages/BookingPage.tsx"));
        assert!(!snap.partial);
    }

    #[test]
    fn detect_dependencies_excludes_relative() {
        let body = make_code_ai_chat();
        let root: Value = serde_json::from_str(&body).expect("ai_chat valido");
        let snap = extract_make_code_snapshot(&root, false);
        let deps = detect_dependencies(&snap.files);
        // react e sonner sono esterni; "./routes" e' relativo -> escluso.
        assert!(deps.contains(&"react".to_string()), "deps={deps:?}");
        assert!(deps.contains(&"sonner".to_string()), "deps={deps:?}");
        assert!(!deps.iter().any(|d| d.starts_with('.')), "deps={deps:?}");
    }

    #[test]
    fn detect_entrypoints_finds_app() {
        let body = make_code_ai_chat();
        let root: Value = serde_json::from_str(&body).expect("ai_chat valido");
        let snap = extract_make_code_snapshot(&root, false);
        let entry = detect_entrypoints(&snap.files);
        assert!(entry.contains(&"src/app/App.tsx".to_string()), "entry={entry:?}");
    }

    #[test]
    fn extract_code_from_make_zip_roundtrip() {
        // ai_chat con code-snapshot dentro uno ZIP .make sintetico.
        let body = make_code_ai_chat();
        let zip = make_ai_chat_zip(&body, true, true);
        let snap = extract_code_from_make(&zip, limits()).expect("estrazione code");
        assert_eq!(snap.files.len(), 2);
        assert!(snap.files.contains_key("src/app/App.tsx"));
    }

    #[test]
    fn package_root_name_handles_scoped() {
        assert_eq!(package_root_name("lucide-react/icons").as_deref(), Some("lucide-react"));
        assert_eq!(package_root_name("@radix-ui/react-dialog/sub").as_deref(), Some("@radix-ui/react-dialog"));
        assert_eq!(package_root_name("react").as_deref(), Some("react"));
    }

    #[test]
    fn parse_path_from_message_rejects_dirty() {
        assert_eq!(
            parse_path_from_message("Successfully updated the file at /src/app/X.tsx.").as_deref(),
            Some("/src/app/X.tsx")
        );
        assert!(parse_path_from_message("Successfully updated the file at /src/X.tsx Make sure to fall back").is_none());
        assert!(parse_path_from_message("nothing here").is_none());
    }

    #[test]
    fn parse_path_from_message_recognizes_created_file_marker() {
        assert_eq!(
            parse_path_from_message("Successfully created file /src/app/Y.tsx").as_deref(),
            Some("/src/app/Y.tsx")
        );
        assert_eq!(
            parse_path_from_message("Successfully rewrote file /src/app/Z.tsx.").as_deref(),
            Some("/src/app/Z.tsx")
        );
        assert_eq!(
            parse_path_from_message("wrote file /src/main.ts").as_deref(),
            Some("/src/main.ts")
        );
    }

    #[test]
    fn extract_make_snapshot_handles_format_b_argsjson() {
        // Formato B: write_tool con argsJson (path + file_text), nessun resultJson.
        let outer = r#"{"toolName":"write_tool","argsJson":"{\"path\":\"/src/app/X.tsx\",\"file_text\":\"export const X = 1;\"}"}"#;
        let body = format!(
            r#"{{"threads":[{{"messages":[
                {{"role":"assistant","parts":[
                    {{"partType":"text","contentJson":{}}}
                ]}}
            ]}}]}}"#,
            serde_json::to_string(outer).unwrap()
        );
        let root: Value = serde_json::from_str(&body).expect("ai_chat valido");
        let snap = extract_make_code_snapshot(&root, false);
        assert_eq!(
            snap.files.get("src/app/X.tsx").map(String::as_str),
            Some("export const X = 1;"),
            "il file scritto con write_tool (Formato B) deve essere estratto"
        );
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
