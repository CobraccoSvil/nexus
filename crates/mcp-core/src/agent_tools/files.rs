//! Tool di operazioni su file: lettura, scrittura, lista, ricerca, edit, delete, rename.

use super::*;

/// Risultato del preflight build graph (ADR 0020) applicato a write/edit.
/// Variant `Block(msg)` blocca la scrittura (es. file generato); `Warn(msg)`
/// lascia passare ma aggiunge un avviso testuale alla risposta del tool.
enum BuildGraphPreflight {
    Allow,
    Warn(String),
    Block(String),
}

/// Risolve il path di SCRITTURA fornito dall'LLM in un path assoluto confinato
/// alla `root`, de-duplicando la root se il modello l'ha gia' inclusa nel path.
///
/// Delega al PUNTO UNICO `nexus_types::workspace_paths::normalize_into_root`
/// (regola L), lo STESSO usato dalla lettura (`resolve_relative_path`): cosi'
/// lettura e scrittura risolvono i path in modo identico. Storicamente la
/// de-duplicazione viveva solo qui, percio' `read_file` falliva sui file che
/// `edit_file` scriveva quando l'LLM includeva la project_root nel path.
/// A differenza della lettura, questa NON richiede che il file esista
/// (i file nuovi non passerebbero `canonicalize`): normalizza e confina soltanto.
fn resolve_write_target(root: &std::path::Path, path_str: &str) -> Result<PathBuf, String> {
    let clean = nexus_types::workspace_paths::normalize_into_root(root, path_str)
        .map_err(|e| e.message().to_string())?;
    if clean.is_empty() {
        return Err("percorso vuoto".to_string());
    }
    Ok(root.join(&clean))
}

/// Esegue il preflight ADR 0020 su `path_str`. Ritorna `Allow` se il file e'
/// nel build graph o entry point o linguaggio non riconosciuto; `Warn` se
/// e' fuori dal build graph (warning non bloccante); `Block` se in directory
/// generata (es. node_modules, target, dist).
async fn run_build_graph_preflight(ctx: &AgentToolContext, path_str: &str) -> BuildGraphPreflight {
    // Estensioni codice rilevanti: l'enforcement parte solo per file
    // sorgente, non per md/json/yaml/config.
    let ext = std::path::Path::new(path_str)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let is_code = matches!(
        ext.as_deref(),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "rb")
    );
    if !is_code {
        return BuildGraphPreflight::Allow;
    }

    let rel = std::path::Path::new(path_str.trim_start_matches(['\\', '/']));
    let membership = match crate::build_graph::is_in_build_graph(ctx.project_id, rel).await {
        Ok(m) => m,
        Err(e) => {
            // Cache non disponibile o resolver fallito: lascio passare ma loggo.
            tracing::debug!(
                project_id = %ctx.project_id,
                path = %path_str,
                error = %e,
                "build_graph.preflight: errore lookup, allow (best-effort)"
            );
            return BuildGraphPreflight::Allow;
        }
    };
    match membership {
        crate::build_graph::BuildGraphMembership::Generated { reason } => {
            BuildGraphPreflight::Block(format!(
                "Scrittura rifiutata: '{}' e' un file generato ({}). I file generati dalla build non vanno modificati manualmente.",
                path_str, reason
            ))
        }
        crate::build_graph::BuildGraphMembership::OutOfGraph { reason } => {
            // Recupera info per messaggio diagnostico.
            let info_msg = match crate::build_graph::BuildGraphCache::global() {
                Some(cache) => match cache.get_or_compute(ctx.project_id).await {
                    Ok(info) => format!(
                        " Build graph derivato da: {}. Include patterns: {}.",
                        info.sources.join(", "),
                        info.include_globs.join(", ")
                    ),
                    Err(_) => String::new(),
                },
                None => String::new(),
            };
            BuildGraphPreflight::Warn(format!(
                "ATTENZIONE: '{}' NON e' nel build graph del progetto ({}). I file fuori dal build graph non vengono compilati ne eseguiti.{} Se il tuo obiettivo e' modificare codice di produzione, usa `nexus_build_graph_info` per verificare quale path e' nel build graph.",
                path_str, reason, info_msg
            ))
        }
        crate::build_graph::BuildGraphMembership::Unknown { .. }
        | crate::build_graph::BuildGraphMembership::InGraph { .. }
        | crate::build_graph::BuildGraphMembership::Entrypoint { .. } => BuildGraphPreflight::Allow,
    }
}

pub(super) async fn tool_read_file(ctx: &AgentToolContext, input: &Value) -> String {
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };
    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };
    // Cap-byte difensivo (governance fs/read_max_bytes): un file enorme
    // (bundle, dump, lock) caricato integralmente satura il contesto e la
    // memoria. Soglia DB-driven (regola G); 0/assente = nessun cap.
    let read_max_bytes: u64 = crate::settings::get_setting(&ctx.db, "agent.fs.read_max_bytes")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(2_097_152);
    if read_max_bytes > 0 {
        if let Ok(meta) = tokio::fs::metadata(&target).await {
            if meta.len() > read_max_bytes {
                return format!(
                    "[Errore lettura '{}': file troppo grande ({} byte > limite {} byte). \
                     Usa read_file_lines(path, start_line, end_line) per leggere una porzione, \
                     o search_file_semantic per trovare le sezioni rilevanti.]",
                    path_str,
                    meta.len(),
                    read_max_bytes
                );
            }
        }
    }

    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    let total_lines = content.lines().count();
    if total_lines <= READ_FILE_STRUCTURE_HINT_LINES {
        // File piccolo: restituisci tutto
        return content;
    }

    // File grande: anteponiamo una mappa strutturale per orientare l'agente,
    // MA restituiamo SEMPRE il contenuto INTEGRALE subito dopo (politica "mai
    // troncare-e-buttare": nessuna riga viene persa, nessun rimando obbligato a
    // read_file_lines).
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
        "[FILE GRANDE — {total_lines} righe totali — contenuto integrale incluso sotto]\n\
        → Per saltare a una sezione nota usa la mappa strutturale qui sotto.\n\
        → Per una ricerca mirata: search_file_semantic(\"{path_str}\", \"cosa stai cercando\").\n\
        \n\
        === STRUTTURA DEL FILE ({struct_count} definizioni trovate) ===\n\
        {structure_map}\n\
        \n\
        === CONTENUTO INTEGRALE ({total_lines} righe) ===\n\
        {content}",
        total_lines = total_lines,
        path_str = path_str,
        struct_count = structure.len(),
        structure_map = structure_map,
        content = content,
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
        if n < 1 {
            return "[Errore: 'start_line' deve essere un intero >= 1]".to_string();
        }
        n as usize
    } else if let Some(n) = input.get("offset").and_then(Value::as_u64) {
        if n < 1 {
            return "[Errore: 'offset' deve essere un intero >= 1]".to_string();
        }
        n as usize
    } else {
        return "[Errore: parametro 'start_line' mancante (oppure 'offset' come alias)]"
            .to_string();
    };

    let end_line: usize = if let Some(n) = input.get("end_line").and_then(Value::as_u64) {
        if n < start_line as u64 {
            return "[Errore: 'end_line' deve essere >= start_line]".to_string();
        }
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
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
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

    // Governance risorse in scrittura (porte ADR 0010 + URL interni), punto
    // unico con audit: su violazione registra in nexus_resource_audit e
    // ritorna il rifiuto. Catalogo policy: nexus_resource_policies (mig 0397).
    if let Some(msg) = crate::security::resource_governance::enforce_on_write(
        ctx,
        "write_file",
        path_str,
        content,
    )
    .await
    {
        return msg;
    }

    // Preflight build graph (ADR 0020): blocca file generati, avvisa OOG.
    let bg_warning = match run_build_graph_preflight(ctx, path_str).await {
        BuildGraphPreflight::Block(msg) => return format!("[Errore: {}]", msg),
        BuildGraphPreflight::Warn(msg) => Some(msg),
        BuildGraphPreflight::Allow => None,
    };

    // Risoluzione path con de-duplicazione della root + confinamento (regola L).
    // Corregge il bug "<root>/<root>/file": un path che gia' contiene la root
    // (assoluto o relativo) viene normalizzato a relativo prima del join.
    let target = match resolve_write_target(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore: {e}]"),
    };

    // Crea directory intermedie se necessario
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return format!("[Errore creazione directory: {}]", e);
        }
    }
    let existed_before = target.exists();
    // Tracking ripristinabile (mig 0349): legge lo stato corrente PRIMA della
    // scrittura e registra (project_id, session_id, file_path, before, after)
    // in `file_mutations`. Cosi' un revert successivo riporta il file allo
    // stato attuale. Best-effort: se la registrazione fallisce loggiamo ma non
    // blocchiamo la scrittura (l'agente non puo' restare bloccato per un bug
    // della tabella di audit).
    let before_for_track: Option<String> = if existed_before {
        tokio::fs::read_to_string(&target).await.ok()
    } else {
        None
    };
    if let Err(e) = crate::file_mutations::record_mutation(
        &ctx.db,
        ctx.project_id,
        ctx.session_id,
        Some(ctx.user_id),
        path_str,
        "write_file",
        before_for_track.as_deref(),
        Some(content),
    )
    .await
    {
        tracing::warn!(
            project_id = %ctx.project_id, path = %path_str,
            "file_mutations::record_mutation fallita (write_file): {e}"
        );
    }
    let autocommit_op = if existed_before { "modify" } else { "create" };
    match tokio::fs::write(&target, content).await {
        Ok(()) => {
            // Dispatcher: notifica Explorer/Editor in tempo reale
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::FileChanged {
                    path: path_str.to_string(),
                    op: if existed_before {
                        "modified".to_string()
                    } else {
                        "created".to_string()
                    },
                },
            );

            // Auto-commit per sessione su branch dedicato: rete di sicurezza
            // sopra file_mutations. Se non e' un git repo / setting disabilitato
            // / session_id assente, il modulo fa no-op silenzioso (vedi modulo).
            let ac_db = ctx.db.clone();
            let ac_root = ctx.root_path.clone();
            let ac_is_git = ctx.is_git_repo;
            let ac_sid = ctx.session_id;
            let ac_path = path_str.to_string();
            let ac_op = autocommit_op.to_string();
            tokio::spawn(async move {
                crate::session_autocommit::snapshot_after_mutation(
                    &ac_db, &ac_root, ac_is_git, ac_sid, &ac_op, &ac_path,
                )
                .await;
            });

            // Re-indicizza il file nel code index + eventuale auto-scan qualità (in background)
            let db_bg = ctx.db.clone();
            let neural_bg = ctx.neural.clone();
            let project_id_bg = ctx.project_id;
            let root_bg = ctx.root_path.clone();
            let target_bg = target.clone();
            let path_str_bg = path_str.to_string();
            let content_bg = content.to_string();
            tokio::spawn(async move {
                let _ = crate::projects::reindex_single_file(
                    &db_bg,
                    &neural_bg,
                    project_id_bg,
                    &root_bg,
                    &target_bg,
                )
                .await;
                crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &target_bg).await;
                // Ri-valuta le violazioni di governance risorse sul file appena scritto:
                // se l'edit ha rimosso la porta/URL hardcoded, la diagnosi policy_violation
                // viene chiusa e sparisce dal pannello Problemi (regola H: niente residui).
                crate::security::resource_linter::revalidate_file_violations(
                    &db_bg,
                    project_id_bg,
                    &root_bg.to_string_lossy(),
                    &target_bg,
                )
                .await;
                // Hook M2: se il file e' un .md di documentazione, registra in project_documents
                let _ = upsert_project_document_if_doc(
                    &db_bg,
                    project_id_bg,
                    &path_str_bg,
                    &content_bg,
                )
                .await;
            });
            let base = format!(
                "File '{}' scritto con successo ({} byte)",
                path_str,
                content.len()
            );
            let mut msg = base;
            if let Some(w) = bg_warning {
                msg = format!("{}\n\n{}", msg, w);
            }
            // B2: se e' una config critica (.env, vite.config, package.json, ...),
            // SEGNALA (non prescrive, mig 0438) che i servizi gia' in ascolto non
            // applicheranno le modifiche finche' non vengono riavviati. Evita il
            // caso (incidente Beauty-Book) in cui l'agente cambia il .env del proxy
            // ma non riavvia il frontend, e la verifica gira sulla vecchia config.
            if is_critical_config(path_str) {
                msg = format!(
                    "{}\n\nNota: questo e' un file di CONFIGURAZIONE. Un servizio gia' \
                     in esecuzione non applichera' le modifiche finche' non viene \
                     riavviato (es. Vite/Next leggono .env e config solo all'avvio). \
                     Se un servizio del progetto e' attivo, riavvialo prima di \
                     verificarne il comportamento.",
                    msg
                );
            }
            msg
        }
        Err(e) => format!("[Errore scrittura '{}': {}]", path_str, e),
    }
}

/// Vero se `path` e' un file di CONFIGURAZIONE critica le cui modifiche
/// richiedono il riavvio dei servizi gia' in ascolto per avere effetto (Vite,
/// Next, ecc. leggono questi file solo all'avvio; il .env governa proxy/porte).
/// Lista conservativa per evitare falsi positivi (B2). Funzione pura/testabile.
pub(crate) fn is_critical_config(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    const EXACT: &[&str] = &["package.json", "cargo.toml", "dockerfile"];
    if EXACT.contains(&name.as_str()) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "vite.config.",
        "next.config.",
        "nuxt.config.",
        "astro.config.",
        "svelte.config.",
        "vue.config.",
        "tsconfig",
        "docker-compose",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
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
    } else if lower.contains("erd")
        || lower.contains("schema_diagram")
        || lower.contains("er_diagram")
    {
        "er_diagram"
    } else if lower.contains("changelog") || lower.contains("release_notes") {
        "release_notes"
    } else if lower.contains("contributing")
        || lower.contains("project_management")
        || lower.contains("roadmap")
    {
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
    let dir_str = input.get("directory").and_then(Value::as_str).unwrap_or("");
    let target = if dir_str.is_empty() {
        ctx.root_path.clone()
    } else {
        match resolve_relative_path(&ctx.root_path, dir_str) {
            Ok(p) => p,
            Err(e) => {
                return format!(
                    "[Errore percorso: {}]",
                    e.1["error"].as_str().unwrap_or("path error")
                )
            }
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
    // Punto unico (regola L): de-duplica la root se l'agente l'ha inclusa nel
    // path e blocca il traversal ".." (resolve_relative_path -> normalize_into_root).
    let search_path: PathBuf = if let Some(p) = input.get("path").and_then(Value::as_str) {
        match resolve_relative_path(&ctx.root_path, p) {
            Ok(path) => path,
            Err(e) => {
                return format!(
                    "[Errore percorso: {}]",
                    e.1["error"].as_str().unwrap_or("path error")
                )
            }
        }
    } else {
        ctx.root_path.clone()
    };

    let output = Command::new("grep")
        .arg("-rn")
        .arg("--include=*")
        .arg("--max-count=50")
        .arg("-I") // ignora file binari
        .arg(pattern)
        .arg(&search_path)
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
                        line.replacen(ctx.root_path.to_string_lossy().as_ref(), "", 1)
                            .trim_start_matches(['/', '\\'])
                            .to_string()
                    })
                    .collect();
                let total_lines = lines.len();
                // Troncamento: limita per numero righe e per dimensione bytes
                let mut result = String::new();
                for (count, line) in lines.iter().enumerate() {
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
    let recursive = input
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };

    if target.is_dir() {
        if recursive {
            match tokio::fs::remove_dir_all(&target).await {
                Ok(()) => format!(
                    "Directory '{}' eliminata ricorsivamente con successo",
                    path_str
                ),
                Err(e) => format!("[Errore eliminazione directory '{}': {}]", path_str, e),
            }
        } else {
            match tokio::fs::remove_dir(&target).await {
                Ok(()) => format!("Directory '{}' eliminata con successo", path_str),
                Err(e) => format!(
                    "[Errore eliminazione directory '{}': {} (se non e' vuota usa recursive:true)]",
                    path_str, e
                ),
            }
        }
    } else {
        match tokio::fs::remove_file(&target).await {
            Ok(()) => {
                nexus_events::dispatcher::emit(
                    &ctx.project_channels,
                    ctx.project_id,
                    nexus_events::event::ProjectEvent::FileChanged {
                        path: path_str.to_string(),
                        op: "deleted".to_string(),
                    },
                );
                format!("File '{}' eliminato con successo", path_str)
            }
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
        Err(e) => {
            return format!(
                "[Errore percorso sorgente: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };

    // Destinazione: file nuovo (non ancora esistente), quindi resolve_write_target
    // (non canonicalizza) e non resolve_relative_path. Punto unico (regola L):
    // de-duplica la root come per la sorgente e blocca traversal/uscita dalla root.
    let to = match resolve_write_target(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso destinazione: {e}]"),
    };

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

/// Estrae un prefisso "ancora" dalla prima riga di `old_string` da usare per
/// trovare la posizione approssimativa nel file. Tronca a 32 caratteri o al
/// primo separatore "strong" (`{`, `=`, `:`, `,`, `;`) — cosi' un old_string
/// stantio nel CORPO ma corretto nella TESTA della riga (es. firma di funzione
/// invariata, body cambiato) trova comunque l'ancora giusta nel file reale.
///
/// Esempi:
///   "pub fn target_function(arg: u32) -> u32 { arg + 2 }"
///     -> "pub fn target_function(arg" (taglio a 32 char)
///   "let foo = bar;"
///     -> "let foo " (taglio al primo `=`)
fn anchor_prefix(line: &str) -> &str {
    const MAX: usize = 32;
    const STOP_CHARS: &[char] = &['{', '=', ':', ',', ';'];
    let cut = line
        .char_indices()
        .find(|(_, c)| STOP_CHARS.contains(c))
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let cut = cut.min(MAX).min(line.len());
    // Allinea al boundary char piu' vicino per evitare di tagliare un char
    // multibyte UTF-8 a meta'.
    let mut safe = cut;
    while safe > 0 && !line.is_char_boundary(safe) {
        safe -= 1;
    }
    line[..safe].trim_end()
}

/// Render NUMERATO di una finestra di righe `[start, end)` con cap per byte.
///
/// PUNTO UNICO (regola L) del rendering "estratto numerato del file reale":
/// usato sia dal ramo "old_string non trovato" sia dal ramo "old_string
/// ambiguo (N occorrenze)", cosi' l'agente vede SEMPRE lo stesso formato
/// `NNNN | testo` e puo' copiarne l'old_string esatto. Tronca in fondo se
/// supera `max_bytes` (la testa, di solito piu' utile, resta visibile).
/// Ritorna `(excerpt_senza_newline_finale, indice_ultima_riga_resa)`.
fn render_numbered_window(
    lines: &[&str],
    start: usize,
    end: usize,
    max_bytes: usize,
) -> (String, usize) {
    let mut excerpt = String::new();
    let mut bytes = 0usize;
    let mut last_rendered_idx = start;
    for (offset, line) in lines[start..end].iter().enumerate() {
        let line_number = start + offset + 1;
        let rendered = format!("{:>4} | {}\n", line_number, line);
        if bytes + rendered.len() > max_bytes {
            break;
        }
        bytes += rendered.len();
        excerpt.push_str(&rendered);
        last_rendered_idx = start + offset;
    }
    if excerpt.ends_with('\n') {
        excerpt.pop();
    }
    (excerpt, last_rendered_idx)
}

/// Indici (0-based) di riga in cui INIZIA ciascuna occorrenza di `needle` in
/// `content` (LF-normalizzato), limitate a `max_hits`. Una occorrenza che inizia
/// su una riga ma si estende su piu' righe e' contata una sola volta (alla riga
/// d'inizio). Usato dal ramo "old_string ambiguo" per mostrare il contesto delle
/// prime N occorrenze, cosi' l'agente sceglie quella univoca.
fn occurrence_start_lines(content: &str, needle: &str, max_hits: usize) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<usize> = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = content[search_from..].find(needle) {
        let abs = search_from + rel;
        // Riga d'inizio = numero di '\n' prima dell'offset assoluto.
        let line_idx = content[..abs].bytes().filter(|b| *b == b'\n').count();
        hits.push(line_idx);
        if hits.len() >= max_hits {
            break;
        }
        // Avanza di almeno 1 byte per evitare loop su match a lunghezza zero
        // (gia' escluso da needle non vuoto, ma per overlap progressivo).
        search_from = abs + needle.len().max(1);
        if search_from >= content.len() {
            break;
        }
    }
    hits
}

/// Costruisce il messaggio di errore quando `edit_file` trova `old_string` PIU'
/// volte (deve essere univoco). Ramo reso actionable come il "non trovato":
/// mostra l'ESTRATTO NUMERATO attorno alle prime occorrenze, cosi' l'agente puo'
/// aggiungere righe di contesto e rendere l'old_string univoco SENZA chiamare
/// read_file (il contenuto e' gia' qui). Riusa il punto unico
/// [`render_numbered_window`].
fn build_old_string_ambiguous_message(
    content: &str,
    old_string_lf: &str,
    path_str: &str,
    count: usize,
) -> String {
    const WINDOW_BEFORE: usize = 3;
    const WINDOW_AFTER: usize = 6;
    const MAX_HITS_SHOWN: usize = 3;
    const MAX_BYTES_PER_HIT: usize = 900;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let hit_lines = occurrence_start_lines(content, old_string_lf, MAX_HITS_SHOWN);

    // Fallback difensivo: se per qualche motivo non localizziamo le occorrenze
    // (es. old_string che attraversa confini in modo inatteso), restiamo sul
    // messaggio testuale storico — meglio che un estratto vuoto.
    if hit_lines.is_empty() {
        return format!(
            "[Errore: old_string trovato {} volte in '{}'. Deve essere unico: aggiungi piu' contesto (righe circostanti) per renderlo univoco.]",
            count, path_str
        );
    }

    let mut blocks = String::new();
    for (n, &hit) in hit_lines.iter().enumerate() {
        let start = hit.saturating_sub(WINDOW_BEFORE);
        let end = (hit + WINDOW_AFTER + 1).min(total_lines);
        let (excerpt, _) = render_numbered_window(&lines, start, end, MAX_BYTES_PER_HIT);
        blocks.push_str(&format!(
            "Occorrenza {} (~riga {}):\n{}\n\n",
            n + 1,
            hit + 1,
            excerpt
        ));
    }
    let more = if count > hit_lines.len() {
        format!(
            " (mostrate le prime {} di {} occorrenze)",
            hit_lines.len(),
            count
        )
    } else {
        String::new()
    };
    // Rimuove i due newline finali per pulizia.
    let blocks = blocks.trim_end().to_string();

    format!(
        "[Errore: old_string trovato {count} volte in '{path}' \u{2014} deve essere UNICO.{more}\n\
        \u{26a0} NON chiamare read_file: il contesto delle occorrenze e' gia' qui sotto.\n\
        Aggiungi al tuo old_string abbastanza righe circostanti (prese dall'estratto numerato) \
        da identificare UNA SOLA occorrenza, poi riprova:\n\n\
        {blocks}]",
        count = count,
        path = path_str,
        more = more,
        blocks = blocks,
    )
}

/// Costruisce il messaggio di errore quando `edit_file` non trova l'old_string.
///
/// Strategia anti-loop: oltre a indicare la riga approssimativa (token-match
/// case-insensitive), include un ESTRATTO NUMERATO del contenuto attuale del
/// file ATTORNO a quella riga (default +/- 15 righe, max 40 righe totali,
/// hard-cap ~2 KB) — cosi' l'agente puo' riformulare l'old_string esatto nello
/// stesso turno senza chiamare read_file (che potrebbe essere bloccato dal
/// loop-detector e che comunque sprecherebbe un tool-call).
///
/// Se il primo token di `old_string` non viene trovato, ripiega sulle prime
/// 40 righe del file (preview generica, comportamento storico ridotto).
///
/// Funzione pura per essere coperta da test unitari senza dipendenze runtime.
fn build_old_string_not_found_message(content: &str, old_string_lf: &str, path_str: &str) -> String {
    // Limiti dell'estratto (FIX hardening qualita' agentico):
    //  - WINDOW_BEFORE/AFTER controllano la finestra simmetrica attorno
    //    alla riga "simile"; valori conservativi per restare entro ~2 KB.
    //  - MAX_LINES e' un secondo hard-cap di sicurezza.
    //  - MAX_BYTES tronca per evitare di gonfiare il contesto su righe molto
    //    lunghe (minified, JSON serializzato, ecc.).
    const WINDOW_BEFORE: usize = 15;
    const WINDOW_AFTER: usize = 15;
    const MAX_LINES: usize = 40;
    const MAX_BYTES: usize = 2048;

    // Token-match: prima riga non vuota di old_string vs prima riga del file
    // che lo contiene (case-insensitive). E' un'ancora di navigazione, non
    // un match esatto: se manca anche questa, il file e' probabilmente
    // strutturalmente diverso da quello che l'agente immaginava.
    //
    // IMPORTANTE: troncare la prima riga ai primi ~32 char (o al primo
    // separatore strong: `{`, `=`, `(arg + `, ecc.) — altrimenti differenze
    // minime sul corpo (es. `arg + 1` vs `arg + 2` nell'old_string stantio)
    // farebbero fallire il match e ci ridurrebbero al fallback inizio-file,
    // perdendo proprio il valore di "estratto attorno alla riga giusta".
    let first_line = old_string_lf
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("");
    let first_token = anchor_prefix(first_line);

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let similar_line_idx: Option<usize> = if !first_token.is_empty() {
        let first_token_lower = first_token.to_lowercase();
        lines
            .iter()
            .position(|l| l.to_lowercase().contains(&first_token_lower))
    } else {
        None
    };

    let approx_hint = if first_token.is_empty() {
        String::new()
    } else if let Some(i) = similar_line_idx {
        format!(" Prima riga simile trovata ~riga {}.", i + 1)
    } else {
        " Nessuna riga contiene il primo token di old_string.".to_string()
    };

    // Calcola finestra: se abbiamo una riga ancora usa +/- WINDOW_BEFORE/AFTER;
    // altrimenti fallback alle prime righe (caso "file totalmente diverso").
    let (start, end): (usize, usize) = match similar_line_idx {
        Some(i) => {
            let s = i.saturating_sub(WINDOW_BEFORE);
            let e = (i + WINDOW_AFTER + 1).min(total_lines);
            // Cap a MAX_LINES anche dopo l'espansione (in caso di window grande).
            let capped_end = (s + MAX_LINES).min(e);
            (s, capped_end)
        }
        None => (0, MAX_LINES.min(total_lines)),
    };

    // Render numerato + cap per byte (taglia in fondo se sfora MAX_BYTES, in
    // modo che la testa dell'estratto — di solito quella piu' utile — resti
    // sempre visibile). Punto unico del rendering: render_numbered_window.
    let (excerpt, last_rendered_idx) = render_numbered_window(&lines, start, end, MAX_BYTES);

    let lines_shown_end = last_rendered_idx + 1;
    let header_label = match similar_line_idx {
        Some(i) => format!("Contenuto attuale attorno alla riga {} (righe {}..{})", i + 1, start + 1, lines_shown_end),
        None => format!("Contenuto attuale (righe {}..{})", start + 1, lines_shown_end),
    };

    let more_hint = if lines_shown_end < total_lines {
        format!(
            "\n// ... {} righe non mostrate. Usa read_file_lines(\"{}\", {}, {}) se devi vedere altre sezioni.",
            total_lines - lines_shown_end,
            path_str,
            lines_shown_end + 1,
            (lines_shown_end + WINDOW_AFTER).min(total_lines)
        )
    } else {
        String::new()
    };

    format!(
        "[Errore: old_string non trovato nel file '{}'.{approx_hint}\n\
        \u{26a0} NON chiamare read_file o read_file_lines \u{2014} il contenuto del file e' gia' incluso qui sotto.\n\
        Confronta il tuo old_string con le righe reali e correggi spazi, newline o testo che differiscono:\n\n\
        {header_label}:\n{excerpt}{more_hint}]",
        path_str
    )
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

    // Governance risorse in scrittura (porte + URL interni), punto unico con
    // audit: scansiona la nuova porzione e registra l'eventuale violazione.
    if let Some(msg) = crate::security::resource_governance::enforce_on_write(
        ctx,
        "edit_file",
        path_str,
        new_string,
    )
    .await
    {
        return msg;
    }

    // Preflight build graph (ADR 0020).
    let bg_warning = match run_build_graph_preflight(ctx, path_str).await {
        BuildGraphPreflight::Block(msg) => return format!("[Errore: {}]", msg),
        BuildGraphPreflight::Warn(msg) => Some(msg),
        BuildGraphPreflight::Allow => None,
    };

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
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
        0 => build_old_string_not_found_message(&content, &old_string_lf, path_str),
        n if n > 1 => {
            build_old_string_ambiguous_message(&content, &old_string_lf, path_str, n)
        }
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
            // Tracking ripristinabile (mig 0349): registra before/after PRIMA
            // della scrittura. raw_content e' il contenuto preesistente
            // (gia' letto sopra). best-effort: warn ma non blocca.
            if let Err(e) = crate::file_mutations::record_mutation(
                &ctx.db,
                ctx.project_id,
                ctx.session_id,
                Some(ctx.user_id),
                path_str,
                "edit_file",
                Some(&raw_content),
                Some(&new_content),
            )
            .await
            {
                tracing::warn!(
                    project_id = %ctx.project_id, path = %path_str,
                    "file_mutations::record_mutation fallita (edit_file): {e}"
                );
            }
            match tokio::fs::write(&target, &new_content).await {
                Ok(()) => {
                    nexus_events::dispatcher::emit(
                        &ctx.project_channels,
                        ctx.project_id,
                        nexus_events::event::ProjectEvent::FileChanged {
                            path: path_str.to_string(),
                            op: "modified".to_string(),
                        },
                    );
                    // Auto-commit per sessione (vedi tool_write_file).
                    let ac_db = ctx.db.clone();
                    let ac_root = ctx.root_path.clone();
                    let ac_is_git = ctx.is_git_repo;
                    let ac_sid = ctx.session_id;
                    let ac_path = path_str.to_string();
                    tokio::spawn(async move {
                        crate::session_autocommit::snapshot_after_mutation(
                            &ac_db, &ac_root, ac_is_git, ac_sid, "modify", &ac_path,
                        )
                        .await;
                    });

                    // Re-indicizza il file nel code index + eventuale auto-scan qualità (in background)
                    let db_bg = ctx.db.clone();
                    let neural_bg = ctx.neural.clone();
                    let project_id_bg = ctx.project_id;
                    let root_bg = ctx.root_path.clone();
                    let target_bg = target.clone();
                    tokio::spawn(async move {
                        let _ = crate::projects::reindex_single_file(&db_bg, &neural_bg, project_id_bg, &root_bg, &target_bg).await;
                        crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &target_bg).await;
                        // Ri-valuta le violazioni di governance risorse sul file appena
                        // modificato: chiude (e fa sparire dal pannello Problemi) le diagnosi
                        // policy_violation risolte dall'edit (regola H: niente residui).
                        crate::security::resource_linter::revalidate_file_violations(
                            &db_bg,
                            project_id_bg,
                            &root_bg.to_string_lossy(),
                            &target_bg,
                        )
                        .await;
                    });
                    let base = format!(
                        "File '{}' modificato con successo ({} byte → {} byte)",
                        path_str,
                        content.len(),
                        new_content.len()
                    );
                    if let Some(w) = bg_warning {
                        format!("{}\n\n{}", base, w)
                    } else {
                        base
                    }
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
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
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
    let overwrite = input
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let from = match resolve_relative_path(&ctx.root_path, from_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso sorgente: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };
    let to = match resolve_relative_path(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso destinazione: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
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
            Ok(bytes) => format!(
                "File copiato '{}' -> '{}' ({} byte)",
                from_str, to_str, bytes
            ),
            Err(e) => format!("[Errore copia file: {}]", e),
        }
    } else if from.is_dir() {
        match copy_dir_recursive(&from, &to).await {
            Ok(count) => format!(
                "Directory copiata '{}' -> '{}' ({} file)",
                from_str, to_str, count
            ),
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
        Err(e) => {
            return format!(
                "[Errore percorso sorgente: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };
    let to = match resolve_relative_path(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => {
            return format!(
                "[Errore percorso destinazione: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
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

// Test unitari sulla funzione pura `build_old_string_not_found_message`.
// Verifica il branch di errore "old_string non trovato" cosi' che eventuali
// regressioni sull'estratto numerato attorno alla riga simile siano colte.
#[cfg(test)]
mod tests {
    use super::build_old_string_ambiguous_message;
    use super::build_old_string_not_found_message;
    use super::is_critical_config;
    use super::occurrence_start_lines;

    #[test]
    fn is_critical_config_riconosce_i_file_di_config() {
        // B2: config che richiedono il riavvio del servizio per avere effetto.
        assert!(is_critical_config(".env"));
        assert!(is_critical_config("proj/.env.production"));
        assert!(is_critical_config("vite.config.ts"));
        assert!(is_critical_config("a/b/next.config.js"));
        assert!(is_critical_config("package.json"));
        assert!(is_critical_config("tsconfig.app.json"));
        assert!(is_critical_config("docker-compose.nexus.yml"));
        // Sorgenti normali: NON critici (niente hint inutile).
        assert!(!is_critical_config("src/app.ts"));
        assert!(!is_critical_config("src/components/Login.tsx"));
        assert!(!is_critical_config("README.md"));
        assert!(!is_critical_config("environment.ts")); // contiene "env" ma non e' .env
    }

    fn make_file(num_lines: usize) -> String {
        (1..=num_lines)
            .map(|i| match i {
                42 => "pub fn target_function(arg: u32) -> u32 { arg + 1 }".to_string(),
                _ => format!("// riga di riempimento numero {i}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn estratto_numerato_attorno_alla_riga_simile() {
        let content = make_file(120);
        // Old string del modello stantio: cerca la firma vecchia che NON
        // matchera' piu' (per simulare l'edit cieco).
        let old_string = "pub fn target_function(arg: u32) -> u32 { arg + 2 }";

        let msg = build_old_string_not_found_message(&content, old_string, "src/lib.rs");

        // 1. Messaggio originale conservato.
        assert!(msg.starts_with("[Errore: old_string non trovato nel file 'src/lib.rs'."),
            "header originale non preservato: {}", msg);
        assert!(msg.contains("NON chiamare read_file"),
            "warning anti-loop perso: {}", msg);

        // 2. Riga simile correttamente individuata (riga 42).
        assert!(msg.contains("Prima riga simile trovata ~riga 42."),
            "ancora di navigazione non presente: {}", msg);

        // 3. Header dell'estratto presente con riferimento alla riga 42.
        assert!(msg.contains("Contenuto attuale attorno alla riga 42"),
            "header dell'estratto attorno alla riga simile mancante: {}", msg);

        // 4. Estratto NUMERATO contiene la riga 42 ed alcune righe attorno
        //    (finestra +/- 15: dovrebbe coprire almeno 27 e 57).
        assert!(msg.contains("  42 | pub fn target_function"),
            "riga 42 numerata non presente nell'estratto: {}", msg);
        assert!(msg.contains("  30 | "),
            "riga 30 (window before) attesa nell'estratto: {}", msg);
        assert!(msg.contains("  55 | "),
            "riga 55 (window after) attesa nell'estratto: {}", msg);

        // 5. Limite di sicurezza: il messaggio totale deve restare contenuto
        //    (tetto ~2 KB sull'estratto + margine).
        assert!(msg.len() < 4096,
            "messaggio sopra il tetto ragionevole ({}B): {}", msg.len(), msg);
    }

    #[test]
    fn fallback_alle_prime_righe_se_nessun_token_simile() {
        let content = "alpha\nbeta\ngamma\ndelta\n".to_string();
        let old_string = "stringa_che_non_compare_da_nessuna_parte_xyz123";

        let msg = build_old_string_not_found_message(&content, old_string, "f.txt");

        assert!(msg.contains("Nessuna riga contiene il primo token di old_string."),
            "hint di assenza atteso: {}", msg);
        // L'estratto di fallback parte dalla riga 1.
        assert!(msg.contains("   1 | alpha"),
            "fallback alle prime righe non emesso: {}", msg);
    }

    #[test]
    fn occurrence_start_lines_localizza_le_occorrenze() {
        let content = "fn a() {}\nlet x = foo();\nfn b() {}\nlet y = foo();\n";
        let hits = occurrence_start_lines(content, "foo()", 5);
        // foo() compare a riga 2 (idx 1) e riga 4 (idx 3).
        assert_eq!(hits, vec![1, 3]);
        // Cap rispettato.
        let capped = occurrence_start_lines(content, "foo()", 1);
        assert_eq!(capped, vec![1]);
        // needle vuoto -> nessun hit (niente loop).
        assert!(occurrence_start_lines(content, "", 5).is_empty());
    }

    #[test]
    fn ramo_ambiguo_include_estratto_numerato() {
        // old_string presente 3 volte: il messaggio deve mostrare l'estratto
        // numerato delle prime occorrenze, NON solo testo generico.
        let content = "header\nval = compute();\nmid1\nmid2\nval = compute();\nmid3\nval = compute();\nfooter\n";
        let old_string = "val = compute();";

        let msg = build_old_string_ambiguous_message(content, old_string, "src/lib.rs", 3);

        // Header informa del conteggio e dell'obbligo di univocita'.
        assert!(msg.contains("trovato 3 volte in 'src/lib.rs'"),
            "conteggio mancante: {}", msg);
        assert!(msg.contains("deve essere UNICO"), "vincolo univocita' mancante: {}", msg);
        // Anti-loop: niente read_file, il contesto e' gia' qui.
        assert!(msg.contains("NON chiamare read_file"), "anti-loop mancante: {}", msg);
        // Estratto numerato presente con la riga reale dell'occorrenza.
        assert!(msg.contains("| val = compute();"),
            "riga numerata dell'occorrenza mancante: {}", msg);
        // Etichette di occorrenza multipla.
        assert!(msg.contains("Occorrenza 1"), "etichetta occorrenza 1 mancante: {}", msg);
        assert!(msg.contains("Occorrenza 2"), "etichetta occorrenza 2 mancante: {}", msg);
    }
}
