//! Tool ZIP/TAR/TAR.GZ per esplorazione + lettura singola entry.
//!
//! Operazioni CPU-bound (decompressione) eseguite in `spawn_blocking`. I file
//! sono letti in memoria per evitare la complessita' di reader async sopra
//! zip/tar (i crate sono sync-only).

use std::io::{Cursor, Read};

use base64::Engine;
use serde_json::{json, Value};
use uuid::Uuid;

use super::attachment_inspector::load_attachment;
use super::attachment_settings;
use super::read_cache::{self, ReadCacheKey, ReadKind};
use super::AgentToolContext;

/// Formato archivio rilevato dai magic bytes.
#[derive(Debug, Clone, Copy)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    Unknown,
}

fn detect_format(bytes: &[u8]) -> ArchiveFormat {
    if bytes.len() >= 4 && &bytes[0..4] == b"PK\x03\x04" {
        ArchiveFormat::Zip
    } else if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        ArchiveFormat::TarGz
    } else if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        ArchiveFormat::Tar
    } else {
        ArchiveFormat::Unknown
    }
}

/// `nexus_list_archive_entries(attachment_id)`.
pub(super) async fn tool_nexus_list_archive_entries(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let attachment_id = match input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({ "error": "Parametro 'attachment_id' obbligatorio (UUID)." }).to_string(),
    };

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    let limits = attachment_settings::current(&ctx.db).await;
    let max_entries = limits.archive_max_entries;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };

    // Operazione potenzialmente CPU-bound -> spawn_blocking.
    let format = detect_format(&bytes);
    let result = tokio::task::spawn_blocking(move || match format {
        ArchiveFormat::Zip => list_zip_entries(&bytes, max_entries),
        ArchiveFormat::Tar => list_tar_entries(&bytes, max_entries, /*gz=*/ false),
        ArchiveFormat::TarGz => list_tar_entries(&bytes, max_entries, /*gz=*/ true),
        ArchiveFormat::Unknown => Err("formato archivio non riconosciuto (atteso ZIP/TAR/TAR.GZ)".into()),
    })
    .await;

    match result {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => json!({ "error": e }).to_string(),
        Err(e) => json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
    }
}

fn list_zip_entries(bytes: &[u8], max_entries: usize) -> Result<Value, String> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let total = archive.len();
    let mut entries: Vec<Value> = Vec::with_capacity(total.min(max_entries));
    for i in 0..total.min(max_entries) {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("entry {i} non leggibile: {e}"))?;
        entries.push(json!({
            "name": entry.name(),
            "size": entry.size(),
            "compressed_size": entry.compressed_size(),
            "is_dir": entry.is_dir(),
        }));
    }
    let mut out = json!({
        "format": "zip",
        "total_entries": total,
        "shown": entries.len(),
        "entries": entries,
    });
    if total > max_entries {
        out["truncated_message"] = json!(format!(
            "archivio molto grande: mostro le prime {max_entries} entries su {total}"
        ));
    }
    Ok(out)
}

fn list_tar_entries(bytes: &[u8], max_entries: usize, gz: bool) -> Result<Value, String> {
    let inner: Box<dyn Read> = if gz {
        Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes)))
    } else {
        Box::new(Cursor::new(bytes))
    };
    let mut ar = tar::Archive::new(inner);
    let mut entries: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for entry in ar.entries().map_err(|e| format!("apertura TAR fallita: {e}"))? {
        let entry = entry.map_err(|e| format!("entry non leggibile: {e}"))?;
        total += 1;
        if entries.len() >= max_entries {
            continue;
        }
        let header = entry.header();
        let path = entry
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let size = header.size().unwrap_or(0);
        let is_dir = header.entry_type().is_dir();
        entries.push(json!({
            "name": path,
            "size": size,
            "is_dir": is_dir,
        }));
    }
    let mut out = json!({
        "format": if gz { "tar.gz" } else { "tar" },
        "total_entries": total,
        "shown": entries.len(),
        "entries": entries,
    });
    if total > max_entries {
        out["truncated_message"] = json!(format!(
            "archivio molto grande: mostro le prime {max_entries} entries su {total}"
        ));
    }
    Ok(out)
}

/// `nexus_read_archive_entry(attachment_id, entry_path, encoding?)`.
pub(super) async fn tool_nexus_read_archive_entry(
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
    let entry_path = match input.get("entry_path").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({ "error": "Parametro 'entry_path' obbligatorio." }).to_string(),
    };
    let encoding_req = input
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_lowercase();
    if !matches!(encoding_req.as_str(), "auto" | "text" | "base64") {
        return json!({ "error": "encoding deve essere uno di: auto|text|base64" }).to_string();
    }

    let record = match load_attachment(&ctx.db, attachment_id, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }).to_string(),
    };

    let limits = attachment_settings::current(&ctx.db).await;
    let max_bytes = limits.archive_entry_max_bytes;

    let bytes = match tokio::fs::read(&record.file_path).await {
        Ok(b) => b,
        Err(e) => return json!({ "error": format!("read fallita: {e}") }).to_string(),
    };
    // FIX 2 (ADR 0012): deduplica via read_cache. La chiave include
    // attachment_id, entry_path, encoding e una lunghezza fissa (max_bytes
    // applicato dopo). Se la stessa identica richiesta arriva > 1 volta,
    // la cache aggiunge `from_cache`+`hint` per suggerire di cambiare strategia.
    let cache_key = ReadCacheKey {
        attachment_id,
        kind: ReadKind::ArchiveEntry,
        entry_path: Some(entry_path.clone()),
        offset: 0,
        length: max_bytes as u64,
        encoding: encoding_req.clone(),
    };
    let db = ctx.db.clone();
    let entry_path_for_compute = entry_path.clone();
    let encoding_for_compute = encoding_req.clone();
    read_cache::get_or_compute(&ctx.db, cache_key, move || async move {
        let format = detect_format(&bytes);
        let entry_clone = entry_path_for_compute.clone();
        let result = tokio::task::spawn_blocking(move || match format {
            ArchiveFormat::Zip => extract_zip_entry(&bytes, &entry_clone, max_bytes),
            ArchiveFormat::Tar => extract_tar_entry(&bytes, &entry_clone, max_bytes, false),
            ArchiveFormat::TarGz => extract_tar_entry(&bytes, &entry_clone, max_bytes, true),
            ArchiveFormat::Unknown => Err("formato archivio non riconosciuto".into()),
        })
        .await;

        let (payload, total_size) = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return json!({ "error": e }).to_string(),
            Err(e) => return json!({ "error": format!("spawn_blocking fallita: {e}") }).to_string(),
        };

        let _ = db; // shut up unused
        encode_payload(&entry_path_for_compute, payload, total_size, &encoding_for_compute)
    })
    .await
}

fn extract_zip_entry(
    bytes: &[u8],
    entry_path: &str,
    max_bytes: usize,
) -> Result<(Vec<u8>, u64), String> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("apertura ZIP fallita: {e}"))?;
    let mut entry = archive
        .by_name(entry_path)
        .map_err(|e| format!("entry '{entry_path}' non trovata: {e}"))?;
    let total = entry.size();
    let mut buf = Vec::with_capacity(max_bytes.min(total as usize + 1));
    let mut reader = (&mut entry).take(max_bytes as u64);
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("read entry fallita: {e}"))?;
    Ok((buf, total))
}

fn extract_tar_entry(
    bytes: &[u8],
    entry_path: &str,
    max_bytes: usize,
    gz: bool,
) -> Result<(Vec<u8>, u64), String> {
    let inner: Box<dyn Read> = if gz {
        Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes)))
    } else {
        Box::new(Cursor::new(bytes))
    };
    let mut ar = tar::Archive::new(inner);
    for entry in ar.entries().map_err(|e| format!("apertura TAR fallita: {e}"))? {
        let mut entry = entry.map_err(|e| format!("entry non leggibile: {e}"))?;
        let path = entry
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if path == entry_path {
            let total = entry.header().size().unwrap_or(0);
            let mut buf = Vec::with_capacity(max_bytes.min(total as usize + 1));
            let mut reader = (&mut entry).take(max_bytes as u64);
            reader
                .read_to_end(&mut buf)
                .map_err(|e| format!("read entry fallita: {e}"))?;
            return Ok((buf, total));
        }
    }
    Err(format!("entry '{entry_path}' non trovata nell'archivio TAR"))
}

fn encode_payload(entry_path: &str, payload: Vec<u8>, total_size: u64, encoding: &str) -> String {
    let is_text_like = encoding == "text"
        || (encoding == "auto" && super::attachment_inspector::detect_kind(&payload, entry_path, "").0
            == "text"
            || (encoding == "auto" && is_likely_text(&payload)));
    let read_bytes = payload.len();
    let (content, encoding_label) = if is_text_like {
        match String::from_utf8(payload.clone()) {
            Ok(s) => (s, "text"),
            Err(_) => (
                base64::engine::general_purpose::STANDARD.encode(&payload),
                "base64",
            ),
        }
    } else {
        (
            base64::engine::general_purpose::STANDARD.encode(&payload),
            "base64",
        )
    };

    let truncated = (read_bytes as u64) < total_size;
    json!({
        "entry_path": entry_path,
        "encoding": encoding_label,
        "total_size": total_size,
        "length": read_bytes,
        "truncated": truncated,
        "content": content,
    })
    .to_string()
}

fn is_likely_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let printable = bytes
        .iter()
        .filter(|b| **b == b'\n' || **b == b'\r' || **b == b'\t' || (**b >= 0x20 && **b < 0x7F))
        .count();
    (printable as f64 / bytes.len() as f64) > 0.85
}
