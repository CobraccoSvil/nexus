//! Tool `nexus_inspect_attachment` — magic byte detection + hint estrazione.
//!
//! Vedi ADR 0011 (estensione 0010). L'agente lo chiama quando vede un
//! allegato con MIME ambiguo (`application/octet-stream`, `.make`, `.dat`,
//! `.bin`) per scoprire il vero formato dai magic bytes e ottenere il tool
//! corretto di estrazione (`nexus_list_archive_entries`, `nexus_extract_pdf_text`,
//! ecc.).
//!
//! Niente nomi hardcoded: i limiti (header read size) sono nel modulo come
//! costanti documentate, ma il comportamento del kind detection e' puramente
//! deterministico sui magic bytes — non c'e' configurazione mutabile.

use std::path::PathBuf;

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::AgentToolContext;

/// Bytes letti per la magic byte detection. 32 KB e' sufficiente per:
/// - ZIP/PDF/PNG/JPEG/GIF (firma in primi 8 byte)
/// - DOCX/XLSX/PPTX (ZIP con [Content_Types].xml nei primi KB)
/// - Figma binari (header proprietario nei primi KB)
const HEADER_READ_BYTES: usize = 32 * 1024;

/// Estensioni considerate "sospette" per scatenare l'inspector anche senza
/// MIME octet-stream. Mantieni l'elenco corto: l'utente del tool deve essere
/// l'agente quando ha gia' un sospetto, non un'attivazione automatica.
#[allow(dead_code)]
const SUSPICIOUS_EXTENSIONS: &[&str] = &[".make", ".dat", ".bin", ".pkg", ".fig", ".aux"];

/// Lookup di un allegato accessibile dal contesto agente corrente.
///
/// Filtra per `project_id` per impedire cross-project leak.
pub(super) struct AttachmentRecord {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub file_name: String,
    pub mime_type: String,
    pub size_bytes: i64,
}

/// Risolve un attachment_id passato come **nome file** (fallback per il bug
/// dei checkpoint pre-fix `enrich_attachments_with_ids`).
///
/// Strategia:
/// 1. Prima cerca tra gli allegati della SESSIONE corrente (`session_id` JOIN).
/// 2. Se non trovato e session_id non disponibile, cerca tra tutti gli allegati
///    del progetto, prendendo il piu' recente.
/// 3. Match case-sensitive sul nome esatto. Match parziale rifiutato per evitare
///    ambiguita' (es. "report.pdf" non risolve a "weekly_report.pdf").
pub(super) async fn resolve_attachment_id_by_name(
    db: &PgPool,
    file_name: &str,
    project_id: Uuid,
    session_id: Option<Uuid>,
) -> Result<Uuid, String> {
    if let Some(sid) = session_id {
        let row = sqlx::query(
            "SELECT cma.id FROM chat_message_attachments cma \
             JOIN chat_messages cm ON cma.message_id = cm.id \
             WHERE cma.file_name = $1 AND cma.project_id = $2 AND cm.session_id = $3 \
             ORDER BY cma.created_at DESC LIMIT 1",
        )
        .bind(file_name)
        .bind(project_id)
        .bind(sid)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query lookup by name fallita: {e}"))?;
        if let Some(r) = row {
            let id: Uuid = r.try_get("id").map_err(|e| e.to_string())?;
            return Ok(id);
        }
    }
    let row = sqlx::query(
        "SELECT id FROM chat_message_attachments \
         WHERE file_name = $1 AND project_id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(file_name)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query lookup by name fallback fallita: {e}"))?;
    let row = row.ok_or_else(|| {
        format!("nessun allegato con file_name='{file_name}' nel progetto corrente")
    })?;
    let id: Uuid = row.try_get("id").map_err(|e| e.to_string())?;
    Ok(id)
}

pub(super) async fn load_attachment(
    db: &PgPool,
    attachment_id: Uuid,
    project_id: Uuid,
) -> Result<AttachmentRecord, String> {
    let row = sqlx::query(
        "SELECT id, file_path, file_name, mime_type, size_bytes \
         FROM chat_message_attachments \
         WHERE id = $1 AND project_id = $2",
    )
    .bind(attachment_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query allegato fallita: {e}"))?;

    let row = row.ok_or_else(|| {
        format!("Allegato {attachment_id} non trovato nel progetto corrente o non accessibile")
    })?;

    let id: Uuid = row.try_get("id").map_err(|e| e.to_string())?;
    let file_path: String = row.try_get("file_path").map_err(|e| e.to_string())?;
    let file_name: String = row.try_get("file_name").unwrap_or_default();
    let mime_type: String = row.try_get("mime_type").unwrap_or_default();
    let size_bytes: i64 = row.try_get("size_bytes").unwrap_or(0);

    if file_path.is_empty() {
        return Err("file_path vuoto in DB per questo allegato".into());
    }

    Ok(AttachmentRecord {
        id,
        file_path: PathBuf::from(file_path),
        file_name,
        mime_type,
        size_bytes,
    })
}

/// Legge i primi `HEADER_READ_BYTES` byte del file allegato.
pub(super) async fn read_header(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("impossibile aprire '{}': {e}", path.display()))?;
    let mut buf = vec![0u8; HEADER_READ_BYTES];
    let n = file
        .read(&mut buf)
        .await
        .map_err(|e| format!("read fallita su '{}': {e}", path.display()))?;
    buf.truncate(n);
    Ok(buf)
}

/// Classifica un file dai magic bytes + estensione/MIME come fallback.
///
/// Ritorna `(kind, mime_reale, ext_reale)`.
pub fn detect_kind(
    header: &[u8],
    file_name: &str,
    declared_mime: &str,
) -> (String, String, String) {
    // 0) Fast-path ZIP-based: se inizia con PK\x03\x04, il sub-type lo decide
    //    `detect_zip_subtype` via string-search nelle entries (es. word/document.xml
    //    -> docx, canvas.fig -> figma). Più affidabile di `infer`, che richiede
    //    entries office-specific complete per riconoscere docx/xlsx/pptx e con
    //    header minimali (test/streaming) cade in "binary" perdendo il sub-type.
    if header.starts_with(b"PK\x03\x04") {
        let sub = detect_zip_subtype(header);
        let (mime, ext) = match sub {
            "docx" => (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "docx",
            ),
            "xlsx" => (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "xlsx",
            ),
            "pptx" => (
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "pptx",
            ),
            "figma" => ("application/octet-stream", "fig"),
            _ => ("application/zip", "zip"),
        };
        return (sub.to_string(), mime.to_string(), ext.to_string());
    }

    // 1) Magic bytes con `infer`.
    if let Some(kind) = infer::get(header) {
        let mt = kind.mime_type().to_string();
        let ext = kind.extension().to_string();
        let logical = mime_to_kind(&mt, header).to_string();
        return (logical, mt, ext);
    }

    // 2) Fallback: euristica testuale.
    if looks_like_text(header) {
        let (kind, mime, ext) = classify_text(header, file_name);
        return (kind.to_string(), mime.to_string(), ext.to_string());
    }

    // 3) Fallback finale: usa MIME dichiarato se utile, altrimenti binary.
    let mime = if declared_mime.is_empty() {
        "application/octet-stream"
    } else {
        declared_mime
    }
    .to_string();
    let ext = extension_from_name(file_name).unwrap_or_else(|| "bin".to_string());
    ("binary".to_string(), mime, ext)
}

/// Mappa MIME -> kind logico Nexus.
fn mime_to_kind(mime: &str, header: &[u8]) -> &'static str {
    match mime {
        "application/zip" => detect_zip_subtype(header),
        "application/x-tar" => "tar",
        "application/gzip" | "application/x-gzip" => "gzip",
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        m if m.starts_with("image/") => "image",
        m if m.starts_with("audio/") => "audio",
        m if m.starts_with("video/") => "video",
        m if m.starts_with("text/") => "text",
        _ => "binary",
    }
}

/// Discriminazione interna di uno ZIP: e' un docx, xlsx, pptx, figma, o
/// archivio generico? Analizza i primi 32KB cercando entries note.
fn detect_zip_subtype(header: &[u8]) -> &'static str {
    // ZIP magic = PK\x03\x04. Cerchiamo come stringa entries note nei primi
    // 32 KB (la signature locale di ogni entry contiene il nome leggibile).
    let s = String::from_utf8_lossy(header);
    let has = |needle: &str| s.contains(needle);

    if has("word/document.xml") {
        return "docx";
    }
    if has("xl/workbook.xml") || has("xl/worksheets/") {
        return "xlsx";
    }
    if has("ppt/presentation.xml") || has("ppt/slides/") {
        return "pptx";
    }
    // Figma file: lo ZIP esterno contiene tipicamente `canvas.fig` (formato
    // binario proprietario Figma).
    if has("canvas.fig") || has(".fig") {
        return "figma";
    }
    "zip"
}

/// Euristica: il blocco contiene una percentuale alta di byte stampabili?
fn looks_like_text(header: &[u8]) -> bool {
    if header.is_empty() {
        return false;
    }
    let printable = header
        .iter()
        .filter(|b| **b == b'\n' || **b == b'\r' || **b == b'\t' || (**b >= 0x20 && **b < 0x7F))
        .count();
    let ratio = printable as f64 / header.len() as f64;
    ratio > 0.85
}

/// Classifica un blocco testuale: json/xml/markdown/html/css/js/ts/py/rust/...
fn classify_text(header: &[u8], file_name: &str) -> (&'static str, &'static str, &'static str) {
    let s = String::from_utf8_lossy(header);
    let trimmed = s.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return ("json", "application/json", "json");
    }
    if trimmed.starts_with("<?xml") {
        return ("xml", "application/xml", "xml");
    }
    if trimmed.starts_with("<!DOCTYPE html") || trimmed.starts_with("<html") {
        return ("html", "text/html", "html");
    }
    // Fallback su estensione.
    let ext = extension_from_name(file_name)
        .unwrap_or_default()
        .to_lowercase();
    match ext.as_str() {
        "md" | "markdown" => ("markdown", "text/markdown", "md"),
        "html" | "htm" => ("html", "text/html", "html"),
        "css" => ("css", "text/css", "css"),
        "js" | "mjs" | "cjs" => ("javascript", "application/javascript", "js"),
        "ts" | "tsx" => ("typescript", "application/typescript", "ts"),
        "py" => ("python", "text/x-python", "py"),
        "rs" => ("rust", "text/x-rust", "rs"),
        "go" => ("go", "text/x-go", "go"),
        "java" => ("java", "text/x-java", "java"),
        "c" | "h" => ("c", "text/x-c", "c"),
        "cpp" | "cc" | "hpp" => ("cpp", "text/x-c++", "cpp"),
        "sql" => ("sql", "application/sql", "sql"),
        "toml" => ("toml", "application/toml", "toml"),
        "yaml" | "yml" => ("yaml", "application/yaml", "yaml"),
        "csv" => ("csv", "text/csv", "csv"),
        _ => ("text", "text/plain", "txt"),
    }
}

fn extension_from_name(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|e| !e.is_empty() && e.len() < 16)
}

/// Per ciascun kind, suggerisce i tool di estrazione corretti + hint italiano.
fn extraction_tools_for_kind(kind: &str) -> (Vec<&'static str>, &'static str) {
    match kind {
        "zip" => (
            vec!["nexus_list_archive_entries", "nexus_read_archive_entry"],
            "Archivio ZIP rilevato. Usa nexus_list_archive_entries per esplorare i contenuti, poi nexus_read_archive_entry per leggere singoli file.",
        ),
        "tar" | "gzip" => (
            vec!["nexus_list_archive_entries", "nexus_read_archive_entry"],
            "Archivio TAR/TAR.GZ rilevato. Usa nexus_list_archive_entries per esplorare, poi nexus_read_archive_entry per leggere una entry.",
        ),
        "pdf" => (
            vec!["nexus_extract_pdf_text"],
            "PDF rilevato. Usa nexus_extract_pdf_text per estrarre il testo (page_start/page_end opzionali).",
        ),
        "docx" => (
            vec!["nexus_extract_docx_text"],
            "Documento Word (DOCX) rilevato. Usa nexus_extract_docx_text per estrarre il testo dei paragrafi.",
        ),
        "xlsx" => (
            vec!["nexus_extract_xlsx_data"],
            "Foglio Excel (XLSX) rilevato. Usa nexus_extract_xlsx_data per estrarre righe e celle.",
        ),
        "pptx" => (
            vec!["nexus_list_archive_entries", "nexus_read_archive_entry"],
            "Presentazione PowerPoint (PPTX) rilevata. Esplora le slide con nexus_list_archive_entries (ppt/slides/).",
        ),
        "figma" => (
            vec!["nexus_extract_figma_structure", "nexus_list_archive_entries"],
            "File Figma rilevato. Usa nexus_extract_figma_structure per estrarre il payload canvas.fig + stringhe leggibili.",
        ),
        "png" | "jpeg" | "gif" | "webp" | "svg" | "image" => (
            vec![],
            "Immagine rilevata. Il framework instradera' la richiesta verso un modello vision se l'utente lo richiede; informa l'utente.",
        ),
        "json" | "xml" | "markdown" | "html" | "css" | "javascript" | "typescript" | "python"
        | "rust" | "go" | "java" | "c" | "cpp" | "sql" | "toml" | "yaml" | "csv" | "text" => (
            vec!["nexus_read_attachment"],
            "Contenuto testuale rilevato. Usa nexus_read_attachment (encoding=\"text\") per leggerlo.",
        ),
        _ => (
            vec!["nexus_read_attachment"],
            "Formato binario non riconosciuto. Come ultima risorsa usa nexus_read_attachment con encoding=\"base64\" per ispezionare i byte.",
        ),
    }
}

/// Decide se il `kind` rilevato corrisponde a contenuto testuale.
fn kind_is_text(kind: &str) -> bool {
    matches!(
        kind,
        "json"
            | "xml"
            | "markdown"
            | "html"
            | "css"
            | "javascript"
            | "typescript"
            | "python"
            | "rust"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "sql"
            | "toml"
            | "yaml"
            | "csv"
            | "text"
    )
}

/// Tool `nexus_inspect_attachment`.
///
/// Accetta `attachment_id` in due forme:
/// 1. UUID (canonico)
/// 2. nome file (fallback) — risolto con lookup `file_name = $1 AND project_id = $2`
///    sulla sessione corrente. Indispensabile per i checkpoint LangGraph dei thread
///    pre-fix `enrich_attachments_with_ids` dove il blocco `<allegati>` non
///    esponeva l'UUID e il modello e' costretto a guessare (osservato 30/05/2026:
///    Vertex passava sia il filename "PL.make" sia un UUID allucinato).
pub(super) async fn tool_nexus_inspect_attachment(ctx: &AgentToolContext, input: &Value) -> String {
    let raw_id = match input.get("attachment_id").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => {
            return json!({
                "error": "Parametro 'attachment_id' obbligatorio (UUID valido o nome file)."
            })
            .to_string();
        }
    };

    let resolved_id = if let Ok(uuid) = Uuid::parse_str(raw_id) {
        uuid
    } else {
        match resolve_attachment_id_by_name(&ctx.db, raw_id, ctx.project_id, ctx.session_id).await {
            Ok(uuid) => uuid,
            Err(e) => {
                return json!({
                    "error": format!(
                        "Impossibile risolvere attachment '{raw_id}': {e}. Passa l'UUID dell'allegato \
                         (visibile nel blocco <allegati>) oppure il nome esatto del file."
                    )
                })
                .to_string();
            }
        }
    };

    let record = match load_attachment(&ctx.db, resolved_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    let header = match read_header(&record.file_path).await {
        Ok(h) => h,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    let (kind, mime_reale, ext_reale) = detect_kind(&header, &record.file_name, &record.mime_type);

    // FASE 1 resa Figma Make: se e' un Figma .make il cui ai_chat.json contiene
    // scritture file (fast_apply_tool), il code-snapshot React e' gia' dentro:
    // l'azione raccomandata diventa nexus_extract_figma_code, non solo structure.
    let figma_has_code = if kind == "figma" {
        figma_make_has_fast_apply(&record.file_path).await
    } else {
        false
    };

    let (tools, hint) = extraction_tools_for_kind(&kind);
    let next_action = next_action_recommended(&kind, &record.id, figma_has_code);

    json!({
        "id": record.id.to_string(),
        "name": record.file_name,
        "size_bytes": record.size_bytes,
        "mime_dichiarato": record.mime_type,
        "mime_reale": mime_reale,
        "kind": kind,
        "extension_reale": ext_reale,
        "is_text": kind_is_text(&kind),
        "extraction_tools": tools,
        "hint": hint,
        "next_action_recommended": next_action,
    })
    .to_string()
}

/// Byte massimi letti dall'inizio di ai_chat.json per rilevare la presenza di
/// scritture file (fast_apply_tool). 4 MB e' ampiamente sufficiente: le prime
/// scritture compaiono nei primissimi messaggi del thread.
const FIGMA_FAST_APPLY_PROBE_BYTES: u64 = 4 * 1024 * 1024;

/// Rileva a buon mercato se un .make Figma contiene un code-snapshot
/// (scritture `fast_apply_tool`/`write_tool`) dentro ai_chat.json. Apre lo ZIP
/// e legge un prefisso decompresso di ai_chat.json cercando la sottostringa.
/// Tollerante: qualunque errore I/O o ZIP ritorna `false` (nessun routing
/// speciale, fallback a structure).
async fn figma_make_has_fast_apply(path: &std::path::Path) -> bool {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let Ok(file) = std::fs::File::open(&path) else {
            return false;
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            return false;
        };
        // Trova l'indice di ai_chat.json.
        let mut ai_idx: Option<usize> = None;
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                let name = entry.name().to_lowercase();
                if name == "ai_chat.json" || name.ends_with("/ai_chat.json") {
                    ai_idx = Some(i);
                    break;
                }
            }
        }
        let Some(idx) = ai_idx else {
            return false;
        };
        let Ok(entry) = archive.by_index(idx) else {
            return false;
        };
        let mut buf = Vec::new();
        if entry
            .take(FIGMA_FAST_APPLY_PROBE_BYTES)
            .read_to_end(&mut buf)
            .is_err()
        {
            return false;
        }
        // I toolName compaiono come stringhe (eventualmente escaped) nel JSON.
        memchr_contains(&buf, b"fast_apply_tool")
            || memchr_contains(&buf, b"write_tool")
            || memchr_contains(&buf, b"create_file_tool")
    })
    .await
    .unwrap_or(false)
}

/// Ricerca di sottostringa byte-wise senza dipendenze extra.
fn memchr_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// FIX 1 (ADR 0012): decide il tool che il modello DEVE chiamare subito dopo
/// `nexus_inspect_attachment`, con parametri concreti gia' pronti.
///
/// L'array `extraction_tools` esistente lascia ambiguita': il modello puo'
/// pensare di leggere il binario raw a chunk (`nexus_read_archive_entry` con
/// offset crescenti). Il caso reale osservato in produzione (canvas.fig
/// dentro PL.make) ha saturato il context window. Questo campo elimina
/// l'ambiguita': c'e' UNA azione consigliata, con input pronto da copiare.
///
/// Ritorna `Value::Null` se il kind non ha un'azione strutturata sensata
/// (es. binary opaco): in quel caso il modello deve chiedere all'utente.
///
/// Il payload e' ottimizzato per chiamata diretta via `nexus_mcp_tool_call`
/// con `server_id="builtin"` (sistema lazy discovery): il modello non deve
/// avere il tool nel toolspec, basta che abbia `nexus_mcp_tool_call`. Vedi
/// dispatch in `execute_agent_tool`.
fn next_action_recommended(kind: &str, attachment_id: &Uuid, figma_has_code: bool) -> Value {
    let id_s = attachment_id.to_string();
    let id_arg = json!({ "attachment_id": id_s.clone() });
    let builtin = |tool_name: &str, args: Value, rationale: &str, expected: u32| -> Value {
        json!({
            "via": "nexus_mcp_tool_call",
            "input": {
                "server_id": "builtin",
                "tool_name": tool_name,
                "arguments": args,
            },
            "tool": tool_name,
            "rationale": rationale,
            "expected_tokens_output": expected,
        })
    };
    match kind {
        "figma" if figma_has_code => builtin(
            "nexus_extract_figma_code",
            id_arg.clone(),
            "Il .make contiene gia' il codice React/TypeScript completo dell'app \
             (scritture fast_apply_tool dentro ai_chat.json): estrailo su disco con \
             nexus_extract_figma_code invece di rigenerarlo da zero. Mantiene il design \
             fedele. Il tool scrive i file e ritorna solo un manifest (niente bloat di \
             contesto). Poi leggi i file con read_file.",
            2000,
        ),
        "figma" => builtin(
            "nexus_extract_figma_structure",
            id_arg.clone(),
            "Archivio contiene canvas.fig (Figma design). Letture raw del binario sono \
             inutili: usa l'estrattore strutturato che ritorna strings + metadata.",
            5000,
        ),
        "zip" => builtin(
            "nexus_list_archive_entries",
            id_arg.clone(),
            "Esplora prima i contenuti dell'archivio per decidere cosa leggere. \
             Solo dopo aver visto la lista, scegli una entry specifica con \
             nexus_read_archive_entry.",
            2000,
        ),
        "tar" | "gzip" => builtin(
            "nexus_list_archive_entries",
            id_arg.clone(),
            "Esplora prima i contenuti dell'archivio TAR per decidere cosa leggere.",
            2000,
        ),
        "pdf" => builtin(
            "nexus_extract_pdf_text",
            json!({ "attachment_id": id_s.clone(), "page_start": 1, "page_end": 10 }),
            "Estrai testo della prima decina di pagine. Successivamente puoi richiedere \
             pagine specifiche.",
            25000,
        ),
        "docx" => builtin(
            "nexus_extract_docx_text",
            id_arg.clone(),
            "Documento Word: usa l'estrattore strutturato per ottenere paragrafi puliti, \
             non il raw XML dello zip.",
            15000,
        ),
        "xlsx" => builtin(
            "nexus_extract_xlsx_data",
            id_arg.clone(),
            "Foglio Excel: usa l'estrattore per ottenere righe e celle gia' risolte \
             dalle sharedStrings.",
            10000,
        ),
        "pptx" => builtin(
            "nexus_list_archive_entries",
            id_arg.clone(),
            "PPTX e' uno zip di slide XML: esplora ppt/slides/ con \
             nexus_list_archive_entries prima di leggere una slide.",
            2000,
        ),
        "png" | "jpeg" | "gif" | "webp" | "svg" | "image" => builtin(
            "nexus_describe_image_attachment",
            id_arg.clone(),
            "Immagine: usa il tool vision dedicato. Non leggere base64 raw: e' inutile \
             e satura il context window.",
            1500,
        ),
        "json" | "xml" | "markdown" | "html" | "css" | "javascript" | "typescript" | "python"
        | "rust" | "go" | "java" | "c" | "cpp" | "sql" | "toml" | "yaml" | "csv" | "text" => {
            builtin(
                "nexus_read_attachment",
                json!({ "attachment_id": id_s, "encoding": "text" }),
                "Contenuto testuale: leggi come testo (encoding=text). Se vuoi una porzione \
             specifica usa offset/length.",
                15000,
            )
        }
        // binary opaco: niente raccomandazione automatica.
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_zip_via_magic() {
        // Magic ZIP: PK\x03\x04 + dummy entry header
        let header = b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x00\x00\x00\x00";
        let (kind, mime, ext) = detect_kind(header, "PL.make", "application/octet-stream");
        assert_eq!(kind, "zip");
        assert_eq!(mime, "application/zip");
        assert_eq!(ext, "zip");
    }

    #[test]
    fn detect_pdf_via_magic() {
        let header = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n";
        let (kind, mime, _) = detect_kind(header, "doc.pdf", "");
        assert_eq!(kind, "pdf");
        assert_eq!(mime, "application/pdf");
    }

    #[test]
    fn detect_png_via_magic() {
        let header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let (kind, mime, _) = detect_kind(header, "img.png", "");
        assert_eq!(kind, "png");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn detect_text_json() {
        let header = b"  {\"name\": \"test\", \"value\": 42}\n";
        let (kind, mime, _) = detect_kind(header, "config.json", "");
        assert_eq!(kind, "json");
        assert_eq!(mime, "application/json");
    }

    #[test]
    fn detect_text_rust_by_extension() {
        let header = b"fn main() { println!(\"hello\"); }\n";
        let (kind, _, _) = detect_kind(header, "main.rs", "");
        assert_eq!(kind, "rust");
    }

    #[test]
    fn detect_zip_subtype_docx() {
        // ZIP con entry word/document.xml nel header. Costruisce un local
        // file header conforme a ZIP (versione 0x0014, flags 0, method 8) +
        // file name 'word/document.xml' (17 byte), in modo che `infer` lo
        // classifichi come application/zip e detect_zip_subtype cerchi le
        // entries note nella string-search.
        let mut header = Vec::new();
        header.extend_from_slice(b"PK\x03\x04"); // signature
        header.extend_from_slice(&[0x14, 0x00]); // version needed
        header.extend_from_slice(&[0x00, 0x00]); // flags
        header.extend_from_slice(&[0x08, 0x00]); // method
        header.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // time/date
        header.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // crc32
        header.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // compressed size
        header.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // uncompressed size
        header.extend_from_slice(&[0x11, 0x00]); // file name length = 17
        header.extend_from_slice(&[0x00, 0x00]); // extra field length = 0
        header.extend_from_slice(b"word/document.xml");
        header.extend_from_slice(&[0u8; 256]); // payload dummy
        let (kind, _, _) = detect_kind(&header, "doc.docx", "");
        assert_eq!(kind, "docx", "atteso 'docx', ottenuto '{kind}'");
    }

    #[test]
    fn detect_zip_subtype_figma() {
        let mut header = vec![0u8; 0];
        header.extend_from_slice(b"PK\x03\x04");
        header.extend_from_slice(&[0; 26]);
        header.extend_from_slice(b"canvas.fig");
        header.extend_from_slice(&[0; 100]);
        let (kind, _, _) = detect_kind(&header, "PL.make", "application/octet-stream");
        assert_eq!(kind, "figma");
    }

    #[test]
    fn binary_unknown_fallback() {
        let header = b"\xff\xfe\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f";
        let (kind, _, _) = detect_kind(header, "weird.bin", "application/octet-stream");
        assert_eq!(kind, "binary");
    }
}
