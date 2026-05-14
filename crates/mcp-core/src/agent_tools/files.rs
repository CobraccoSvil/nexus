//! Tool di operazioni su file: lettura, scrittura, lista, ricerca, edit, delete, rename.

use super::*;

pub(super) async fn tool_read_file(ctx: &AgentToolContext, input: &Value) -> String {
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    let total_lines = content.lines().count();
    if total_lines <= READ_FILE_MAX_LINES {
        // File piccolo: restituisci tutto
        return content;
    }

    // File grande: restituisci la mappa strutturale + prime 80 righe (import/dichiarazioni)
    let header: String = content
        .lines()
        .take(80)
        .collect::<Vec<_>>()
        .join("\n");

    // Costruisci mappa strutturale (funzioni/classi con numeri di riga)
    let structure = extract_file_structure(&content);
    let structure_map: String = if structure.is_empty() {
        "  (nessuna struttura rilevata automaticamente)".to_string()
    } else {
        structure
            .iter()
            .map(|(ln, desc)| format!("  riga {:>4} — {}", ln, desc))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "[FILE GRANDE — {total_lines} righe totali]\n\
        ⚠ NON richiamare read_file su questo file.\n\
        → Per trovare informazioni specifiche: usa search_file_semantic(\"{path_str}\", \"cosa stai cercando\")\n\
        → Per leggere una sezione nota: usa read_file_lines(\"{path_str}\", start_line, end_line) (max 400 righe per chiamata)\n\
        \n\
        === STRUTTURA DEL FILE ({struct_count} definizioni trovate) ===\n\
        {structure_map}\n\
        \n\
        === PRIME 80 RIGHE (import e dichiarazioni) ===\n\
        {header}",
        total_lines = total_lines,
        path_str = path_str,
        struct_count = structure.len(),
        structure_map = structure_map,
        header = header,
    )
}

pub(super) async fn tool_read_file_lines(ctx: &AgentToolContext, input: &Value) -> String {
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };

    // Accetta sia start_line/end_line (parametri corretti) sia offset/limit (alias
    // usati erroneamente da alcune istruzioni del supervisor — mappati automaticamente
    // per evitare che errori nei prompt causino loop di re-lettura).
    let start_line: usize = if let Some(n) = input.get("start_line").and_then(Value::as_u64) {
        if n < 1 { return "[Errore: 'start_line' deve essere un intero >= 1]".to_string(); }
        n as usize
    } else if let Some(n) = input.get("offset").and_then(Value::as_u64) {
        if n < 1 { return "[Errore: 'offset' deve essere un intero >= 1]".to_string(); }
        n as usize
    } else {
        return "[Errore: parametro 'start_line' mancante (oppure 'offset' come alias)]".to_string();
    };

    let end_line: usize = if let Some(n) = input.get("end_line").and_then(Value::as_u64) {
        if n < start_line as u64 { return "[Errore: 'end_line' deve essere >= start_line]".to_string(); }
        n as usize
    } else if let Some(limit) = input.get("limit").and_then(Value::as_u64) {
        // offset + limit - 1 → end_line inclusa
        (start_line as u64).saturating_add(limit).saturating_sub(1) as usize
    } else {
        return "[Errore: parametro 'end_line' mancante (oppure 'limit' come alias)]".to_string();
    };

    // Limita il range massimo per evitare di caricare troppe righe
    let end_line = end_line.min(start_line + READ_FILE_LINES_MAX - 1);

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    let total_lines = content.lines().count();
    if start_line > total_lines {
        return format!(
            "[Errore: start_line {} supera il numero totale di righe del file ({})]",
            start_line, total_lines
        );
    }

    let end_line = end_line.min(total_lines);
    let selected: String = content
        .lines()
        .enumerate()
        .filter(|(i, _)| {
            let line_num = i + 1; // 1-based
            line_num >= start_line && line_num <= end_line
        })
        .map(|(i, line)| format!("{:>4} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "// {} — righe {}-{} (totale: {} righe)\n{}{}",
        path_str,
        start_line,
        end_line,
        total_lines,
        selected,
        if end_line < total_lines {
            format!("\n\n// ... righe {}-{} non mostrate. Usa read_file_lines(\"{}\", {}, {}) per continuare.",
                end_line + 1, total_lines, path_str, end_line + 1, (end_line + READ_FILE_LINES_MAX).min(total_lines))
        } else {
            String::new()
        }
    )
}

pub(super) async fn tool_write_file(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(path_str) {
            return format!("[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente. Modifica manualmente se necessario.]", path_str, pattern);
        }
    }
    let content = match input.get("content").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'content' mancante]".to_string(),
    };

    // Calcola il path assoluto con sicurezza path-traversal
    let clean = path_str.trim().trim_start_matches(['\\', '/']);
    let target = ctx.root_path.join(clean);
    // Verifica che sia dentro la root (anche per path non esistenti)
    let normalized = target
        .components()
        .collect::<Vec<_>>()
        .iter()
        .fold(PathBuf::new(), |mut acc, c| {
            acc.push(c);
            acc
        });
    if !normalized.starts_with(&ctx.root_path) {
        return "[Errore: percorso non autorizzato (fuori dalla root del progetto)]".to_string();
    }

    // Crea directory intermedie se necessario
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return format!("[Errore creazione directory: {}]", e);
        }
    }
    match tokio::fs::write(&target, content).await {
        Ok(()) => {
            // Re-indicizza il file nel code index + eventuale auto-scan qualità (in background)
            let db_bg = ctx.db.clone();
            let neural_bg = ctx.neural.clone();
            let project_id_bg = ctx.project_id;
            let root_bg = ctx.root_path.clone();
            let target_bg = target.clone();
            let path_str_bg = path_str.to_string();
            let content_bg = content.to_string();
            tokio::spawn(async move {
                let _ = crate::projects::reindex_single_file(&db_bg, &neural_bg, project_id_bg, &root_bg, &target_bg).await;
                crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &target_bg).await;
                // Hook M2: se il file e' un .md di documentazione, registra in project_documents
                let _ = upsert_project_document_if_doc(&db_bg, project_id_bg, &path_str_bg, &content_bg).await;
            });
            format!("File '{}' scritto con successo ({} byte)", path_str, content.len())
        }
        Err(e) => format!("[Errore scrittura '{}': {}]", path_str, e),
    }
}

/// Hook M2: rileva se un file appena scritto e' documentazione del progetto e lo registra in `project_documents`.
/// Tipi rilevati: PRD, README, ARCHITECTURE, CHANGELOG, CONTRIBUTING, SPEC, generic markdown sotto specs/ o docs/.
/// Idempotente: se esiste gia una riga con stesso (project_id, file_path), aggiorna updated_at e version increment patch.
async fn upsert_project_document_if_doc(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    rel_path: &str,
    content: &str,
) -> Result<(), sqlx::Error> {
    let lower = rel_path.to_lowercase();
    if !lower.ends_with(".md") && !lower.ends_with(".markdown") {
        return Ok(());
    }
    // Mappa al check constraint del DB (project_documents_doc_type_check):
    //   functional_analysis, technical_analysis, er_diagram, project_management, release_notes
    let doc_type = if lower.contains("prd")
        || lower.starts_with("specs/")
        || lower.contains("/specs/")
        || lower.contains("functional")
    {
        "functional_analysis"
    } else if lower.ends_with("readme.md")
        || lower.contains("architecture")
        || lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.contains("technical")
    {
        "technical_analysis"
    } else if lower.contains("erd") || lower.contains("schema_diagram") || lower.contains("er_diagram") {
        "er_diagram"
    } else if lower.contains("changelog") || lower.contains("release_notes") {
        "release_notes"
    } else if lower.contains("contributing") || lower.contains("project_management") || lower.contains("roadmap") {
        "project_management"
    } else {
        return Ok(());
    };
    // Titolo: prima riga "# ..." oppure nome file senza estensione
    let title = content
        .lines()
        .find_map(|l| {
            let t = l.trim();
            t.strip_prefix("# ").map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| {
            std::path::Path::new(rel_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(rel_path)
                .to_string()
        });
    let title = title.chars().take(255).collect::<String>();

    sqlx::query(
        r#"
        INSERT INTO project_documents (project_id, doc_type, title, file_path, status, metadata, structure_json)
        VALUES ($1, $2, $3, $4, 'draft', jsonb_build_object('source', 'agent_write_file'), '{}'::jsonb)
        ON CONFLICT (project_id, file_path) DO UPDATE
          SET title = EXCLUDED.title,
              doc_type = EXCLUDED.doc_type,
              updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(doc_type)
    .bind(&title)
    .bind(rel_path)
    .execute(db)
    .await?;
    Ok(())
}

pub(super) async fn tool_list_files(ctx: &AgentToolContext, input: &Value) -> String {
    let dir_str = input
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = if dir_str.is_empty() {
        ctx.root_path.clone()
    } else {
        match resolve_relative_path(&ctx.root_path, dir_str) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    };

    let mut entries = match tokio::fs::read_dir(&target).await {
        Ok(rd) => rd,
        Err(e) => return format!("[Errore listing '{}': {}]", dir_str, e),
    };

    let mut lines = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let kind = if entry.path().is_dir() { "/" } else { "" };
                lines.push(format!("{name}{kind}"));
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    lines.sort();
    if lines.is_empty() {
        format!("Directory '{}' vuota o non trovata.", dir_str)
    } else {
        lines.join("\n")
    }
}

pub(super) async fn tool_search_in_files(ctx: &AgentToolContext, input: &Value) -> String {
    let pattern = match input.get("pattern").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'pattern' mancante]".to_string(),
    };
    let search_path: &Path = if let Some(p) = input.get("path").and_then(Value::as_str) {
        &ctx.root_path.join(p.trim_start_matches(['\\', '/']))
    } else {
        &ctx.root_path
    };

    let output = Command::new("grep")
        .arg("-rn")
        .arg("--include=*")
        .arg("--max-count=50")
        .arg("-I") // ignora file binari
        .arg(pattern)
        .arg(search_path)
        .output()
        .await;

    // Limite massimo di output: 500KB. Risultati piu' grandi causano
    // RESOURCE_EXHAUSTED gRPC (limite 16MB client Python) e consumano
    // troppi token di contesto per l'LLM. 500KB ~ 10k righe di codice.
    const MAX_OUTPUT_BYTES: usize = 500 * 1024;
    const MAX_OUTPUT_LINES: usize = 2000;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if stdout.is_empty() && !stderr.is_empty() {
                format!("[grep error: {}]", stderr.trim())
            } else if stdout.is_empty() {
                format!("Nessun risultato per '{pattern}'.")
            } else {
                // Rendi i path relativi alla root per leggibilita'
                let lines: Vec<String> = stdout
                    .lines()
                    .map(|line| {
                        line.replacen(
                            &ctx.root_path.to_string_lossy().as_ref(),
                            "",
                            1,
                        )
                        .trim_start_matches(['/', '\\'])
                        .to_string()
                    })
                    .collect();
                let total_lines = lines.len();
                // Troncamento: limita per numero righe e per dimensione bytes
                let mut result = String::new();
                let mut count = 0;
                for line in &lines {
                    if count >= MAX_OUTPUT_LINES || result.len() + line.len() > MAX_OUTPUT_BYTES {
                        let msg = format!(
                            "\n\n[Risultato troncato: mostrate {} di {} righe. Usa un pattern piu' specifico o limita il path.]",
                            count, total_lines
                        );
                        result.push_str(&msg);
                        break;
                    }
                    if count > 0 {
                        result.push('\n');
                    }
                    result.push_str(line);
                    count += 1;
                }
                result
            }
        }
        Err(e) => format!("[Impossibile eseguire grep: {}]", e),
    }
}

pub(super) async fn tool_delete_file(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    let recursive = input.get("recursive").and_then(Value::as_bool).unwrap_or(false);

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };

    if target.is_dir() {
        if recursive {
            match tokio::fs::remove_dir_all(&target).await {
                Ok(()) => format!("Directory '{}' eliminata ricorsivamente con successo", path_str),
                Err(e) => format!("[Errore eliminazione directory '{}': {}]", path_str, e),
            }
        } else {
            match tokio::fs::remove_dir(&target).await {
                Ok(()) => format!("Directory '{}' eliminata con successo", path_str),
                Err(e) => format!("[Errore eliminazione directory '{}': {} (se non e' vuota usa recursive:true)]", path_str, e),
            }
        }
    } else {
        match tokio::fs::remove_file(&target).await {
            Ok(()) => format!("File '{}' eliminato con successo", path_str),
            Err(e) => format!("[Errore eliminazione '{}': {}]", path_str, e),
        }
    }
}

pub(super) async fn tool_rename_file(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let from_str = match input.get("from").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'from' mancante]".to_string(),
    };
    let to_str = match input.get("to").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'to' mancante]".to_string(),
    };

    let from = match resolve_relative_path(&ctx.root_path, from_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso sorgente: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };

    let to_clean = to_str.trim().trim_start_matches(['\\', '/']);
    let to = ctx.root_path.join(to_clean);
    let normalized_to = to
        .components()
        .collect::<Vec<_>>()
        .iter()
        .fold(PathBuf::new(), |mut acc, c| { acc.push(c); acc });
    if !normalized_to.starts_with(&ctx.root_path) {
        return "[Errore: destinazione non autorizzata (fuori dalla root del progetto)]".to_string();
    }

    if let Some(parent) = to.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return format!("[Errore creazione directory destinazione: {}]", e);
        }
    }

    match tokio::fs::rename(&from, &to).await {
        Ok(()) => format!("Rinominato '{}' → '{}'", from_str, to_str),
        Err(e) => format!("[Errore rinomina '{}' → '{}': {}]", from_str, to_str, e),
    }
}

pub(super) async fn tool_edit_file(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(path_str) {
            return format!("[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente.]", path_str, pattern);
        }
    }
    let old_string = match input.get("old_string").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'old_string' mancante]".to_string(),
    };
    let new_string = match input.get("new_string").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'new_string' mancante]".to_string(),
    };

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };

    let raw_content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    // Normalizza CRLF → LF per matching consistente (il code block nel prompt è sempre LF).
    // Se il file ha CRLF, old_string costruito dall'AI (LF) non matcherebbe altrimenti.
    // IMPORTANTE: ricordo se il file originale era CRLF, cosi' alla scrittura ripristino
    // gli EOL originali (altrimenti l'edit converte CRLF→LF tutto il file, generando
    // diff git enormi anche per modifiche minime — bug 14 del test E2E).
    let was_crlf = raw_content.contains("\r\n");
    let content = raw_content.replace("\r\n", "\n");
    let old_string_lf = old_string.replace("\r\n", "\n");
    let new_string_lf = new_string.replace("\r\n", "\n");

    let count = content.matches(old_string_lf.as_str()).count();
    match count {
        0 => {
            // Cerca la riga approssimativa usando il primo token non vuoto di old_string,
            // per dare un'ancora di navigazione anche senza chiamare read_file_lines.
            let first_token = old_string_lf
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim())
                .unwrap_or("");

            let approx_hint = if !first_token.is_empty() {
                // Cerca la prima riga del file che contenga il primo token (senza case sensitivity)
                let first_token_lower = first_token.to_lowercase();
                content
                    .lines()
                    .enumerate()
                    .find(|(_, l)| l.to_lowercase().contains(&first_token_lower))
                    .map(|(i, _)| format!(" Prima riga simile trovata ~riga {}.", i + 1))
                    .unwrap_or_else(|| " Nessuna riga contiene il primo token di old_string.".to_string())
            } else {
                String::new()
            };

            // Includi le prime 80 righe del file con numerazione, così l'agente può
            // confrontare il suo old_string con il contenuto reale SENZA chiamare
            // read_file_lines (che potrebbe essere bloccato dal loop-detector).
            let total_lines = content.lines().count();
            let preview_end = 80.min(total_lines);
            let preview: String = content
                .lines()
                .enumerate()
                .take(preview_end)
                .map(|(i, line)| format!("{:>4} | {}", i + 1, line))
                .collect::<Vec<_>>()
                .join("\n");
            let more_hint = if total_lines > preview_end {
                format!(
                    "\n// ... {} righe non mostrate. Usa read_file_lines(\"{}\", {}, {}) se devi vedere altre sezioni.",
                    total_lines - preview_end, path_str, preview_end + 1, (preview_end + 80).min(total_lines)
                )
            } else {
                String::new()
            };

            format!(
                "[Errore: old_string non trovato nel file '{}'.{approx_hint}\n\
                ⚠ NON chiamare read_file o read_file_lines — il contenuto del file è già incluso qui sotto.\n\
                Confronta il tuo old_string con le righe reali e correggi spazi, newline o testo che differiscono:\n\n\
                {preview}{more_hint}]",
                path_str
            )
        }
        n if n > 1 => format!(
            "[Errore: old_string trovato {} volte in '{}'. Deve essere unico: aggiungi piu' contesto (righe circostanti) per renderlo univoco.]",
            n, path_str
        ),
        _ => {
            let new_content_lf = content.replacen(old_string_lf.as_str(), new_string_lf.as_str(), 1);
            // Ripristina gli EOL originali del file (CRLF se l'originale era CRLF).
            // Senza questo, ogni edit di un file Windows convertirebbe l'intero file
            // in LF generando un diff git rumoroso (bug 14).
            let new_content = if was_crlf {
                new_content_lf.replace('\n', "\r\n")
            } else {
                new_content_lf
            };
            match tokio::fs::write(&target, &new_content).await {
                Ok(()) => {
                    // Re-indicizza il file nel code index + eventuale auto-scan qualità (in background)
                    let db_bg = ctx.db.clone();
                    let neural_bg = ctx.neural.clone();
                    let project_id_bg = ctx.project_id;
                    let root_bg = ctx.root_path.clone();
                    let target_bg = target.clone();
                    tokio::spawn(async move {
                        let _ = crate::projects::reindex_single_file(&db_bg, &neural_bg, project_id_bg, &root_bg, &target_bg).await;
                        crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &target_bg).await;
                    });
                    format!(
                        "File '{}' modificato con successo ({} byte → {} byte)",
                        path_str,
                        content.len(),
                        new_content.len()
                    )
                }
                Err(e) => format!("[Errore scrittura '{}': {}]", path_str, e),
            }
        }
    }
}

/// Crea una directory con semantica `-p` (idempotente, crea genitori).
pub(super) async fn tool_fs_mkdir(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    if target.is_dir() {
        return format!("Directory '{}' esiste gia'", path_str);
    }
    match tokio::fs::create_dir_all(&target).await {
        Ok(()) => format!("Directory '{}' creata con successo", path_str),
        Err(e) => format!("[Errore creazione directory '{}': {}]", path_str, e),
    }
}

/// Copia un file o una directory (ricorsiva) dentro la root del progetto.
pub(super) async fn tool_fs_copy(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let from_str = match input.get("from").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'from' mancante]".to_string(),
    };
    let to_str = match input.get("to").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'to' mancante]".to_string(),
    };
    let overwrite = input.get("overwrite").and_then(Value::as_bool).unwrap_or(false);

    let from = match resolve_relative_path(&ctx.root_path, from_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso sorgente: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    let to = match resolve_relative_path(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso destinazione: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };

    if !from.exists() {
        return format!("[Errore: sorgente '{}' non esiste]", from_str);
    }

    if to.exists() && !overwrite {
        return format!(
            "[Errore: destinazione '{}' esiste gia'. Usa overwrite:true per sovrascrivere]",
            to_str
        );
    }

    if from.is_file() {
        // Crea directory genitore se non esiste
        if let Some(parent) = to.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return format!("[Errore creazione directory destinazione: {}]", e);
            }
        }
        match tokio::fs::copy(&from, &to).await {
            Ok(bytes) => format!("File copiato '{}' -> '{}' ({} byte)", from_str, to_str, bytes),
            Err(e) => format!("[Errore copia file: {}]", e),
        }
    } else if from.is_dir() {
        match copy_dir_recursive(&from, &to).await {
            Ok(count) => format!("Directory copiata '{}' -> '{}' ({} file)", from_str, to_str, count),
            Err(e) => format!("[Errore copia directory: {}]", e),
        }
    } else {
        format!("[Errore: '{}' non e' un file ne' una directory]", from_str)
    }
}

/// Helper ricorsivo per copia directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<usize, String> {
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;

    let mut entries = tokio::fs::read_dir(src)
        .await
        .map_err(|e| format!("readdir {}: {}", src.display(), e))?;

    let mut count = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            count += Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path)
                .await
                .map_err(|e| format!("copy {}: {}", src_path.display(), e))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Sposta (rinomina) un file o una directory. Atomico se sullo stesso filesystem.
pub(super) async fn tool_fs_move(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso su questo progetto]".to_string();
    }
    let from_str = match input.get("from").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'from' mancante]".to_string(),
    };
    let to_str = match input.get("to").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'to' mancante]".to_string(),
    };

    let from = match resolve_relative_path(&ctx.root_path, from_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso sorgente: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    let to = match resolve_relative_path(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso destinazione: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };

    if !from.exists() {
        return format!("[Errore: sorgente '{}' non esiste]", from_str);
    }
    if to.exists() {
        return format!("[Errore: destinazione '{}' esiste gia']", to_str);
    }

    // Crea directory genitore se non esiste
    if let Some(parent) = to.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return format!("[Errore creazione directory destinazione: {}]", e);
        }
    }

    match tokio::fs::rename(&from, &to).await {
        Ok(()) => format!("Spostato '{}' -> '{}'", from_str, to_str),
        Err(e) => format!("[Errore spostamento: {}]", e),
    }
}
