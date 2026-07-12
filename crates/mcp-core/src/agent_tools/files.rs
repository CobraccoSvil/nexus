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
            let info_msg = build_graph_out_of_graph_info(ctx.project_id).await;
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

/// Suffisso diagnostico per il warning "fuori dal build graph": sorgenti e
/// include-pattern da cui il grafo e' derivato. Stringa vuota se la cache non
/// e' disponibile o il calcolo fallisce (best-effort). Estratto da
/// `run_build_graph_preflight` per brevita'.
async fn build_graph_out_of_graph_info(project_id: uuid::Uuid) -> String {
    match crate::build_graph::BuildGraphCache::global() {
        Some(cache) => match cache.get_or_compute(project_id).await {
            Ok(info) => format!(
                " Build graph derivato da: {}. Include patterns: {}.",
                info.sources.join(", "),
                info.include_globs.join(", ")
            ),
            Err(_) => String::new(),
        },
        None => String::new(),
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
    if let Some(err) = read_max_bytes_guard(ctx, &target, path_str).await {
        return err;
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

    let read_full_max_lines: usize =
        crate::settings::get_setting(&ctx.db, "agent.fs.read_full_max_lines")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1200);
    render_large_file_response(&content, total_lines, path_str, read_full_max_lines)
}

/// Cap-byte difensivo sulla lettura integrale: se il file supera la soglia
/// `agent.fs.read_max_bytes` (DB-driven, regola G; 0/assente = nessun cap)
/// ritorna `Some(messaggio_errore)` che invita a `read_file_lines`. Estratto da
/// `tool_read_file`.
async fn read_max_bytes_guard(
    ctx: &AgentToolContext,
    target: &Path,
    path_str: &str,
) -> Option<String> {
    let read_max_bytes: u64 = crate::settings::get_setting(&ctx.db, "agent.fs.read_max_bytes")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(2_097_152);
    if read_max_bytes == 0 {
        return None;
    }
    let meta = tokio::fs::metadata(target).await.ok()?;
    if meta.len() > read_max_bytes {
        return Some(format!(
            "[Errore lettura '{}': file troppo grande ({} byte > limite {} byte). \
             Usa read_file_lines(path, start_line, end_line) per leggere una porzione, \
             o search_file_semantic per trovare le sezioni rilevanti.]",
            path_str,
            meta.len(),
            read_max_bytes
        ));
    }
    None
}

/// Compone la risposta per un file "grande" (oltre `READ_FILE_STRUCTURE_HINT_LINES`
/// righe): antepone una mappa strutturale per orientare l'agente. Per i file
/// medio-grandi restituisce il contenuto INTEGRALE subito dopo (politica "mai
/// troncare-e-buttare": nessuna riga persa). MA oltre `read_full_max_lines` il
/// contenuto integrale satura il contesto e, se l'agente rilegge identicamente
/// read_file (non avendo trovato subito la sezione), innesca un loop REALE:
/// incidente bookingService.ts 1711 righe -> 3 read_file identiche ->
/// loop_detected -> abort. Oltre la soglia si rimanda a read_file_lines guidati
/// dalla mappa (0/assente = sempre integrale). Estratto da `tool_read_file`.
/// Formatta la mappa strutturale (righe "  riga NNNN — descrizione") a partire
/// dalle definizioni estratte da `extract_file_structure`; placeholder se vuota.
/// Estratto da `render_large_file_response`.
fn format_structure_map(structure: &[(usize, String)]) -> String {
    if structure.is_empty() {
        return "  (nessuna struttura rilevata automaticamente)".to_string();
    }
    structure
        .iter()
        .map(|(ln, desc)| format!("  riga {:>4} — {}", ln, desc))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_large_file_response(
    content: &str,
    total_lines: usize,
    path_str: &str,
    read_full_max_lines: usize,
) -> String {
    let structure = extract_file_structure(content);
    let structure_map = format_structure_map(&structure);

    if read_full_max_lines > 0 && total_lines > read_full_max_lines {
        return format!(
            "[FILE GRANDE — {total_lines} righe — troppo lungo per la lettura integrale]\n\
            NON rileggere read_file su questo path: otterresti lo stesso output. Per procedere:\n\
            → leggi una sezione specifica con read_file_lines(\"{path_str}\", start_line, end_line) \
            usando le righe indicate dalla mappa qui sotto;\n\
            → oppure search_file_semantic(\"{path_str}\", \"cosa stai cercando\") per individuarla.\n\
            \n\
            === STRUTTURA DEL FILE ({struct_count} definizioni trovate) ===\n\
            {structure_map}",
            total_lines = total_lines,
            path_str = path_str,
            struct_count = structure.len(),
            structure_map = structure_map,
        );
    }

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

/// Estrae `(start_line, end_line)` (1-based, inclusi) dai parametri di
/// `read_file_lines`. Accetta sia `start_line`/`end_line` (parametri corretti)
/// sia `offset`/`limit` (alias usati erroneamente da alcune istruzioni del
/// supervisor — mappati automaticamente per evitare loop di re-lettura). Applica
/// il cap `READ_FILE_LINES_MAX` sul range. Estratto da `tool_read_file_lines`.
fn parse_line_range(input: &Value) -> Result<(usize, usize), String> {
    let start_line: usize = if let Some(n) = input.get("start_line").and_then(Value::as_u64) {
        if n < 1 {
            return Err("[Errore: 'start_line' deve essere un intero >= 1]".to_string());
        }
        n as usize
    } else if let Some(n) = input.get("offset").and_then(Value::as_u64) {
        if n < 1 {
            return Err("[Errore: 'offset' deve essere un intero >= 1]".to_string());
        }
        n as usize
    } else {
        return Err(
            "[Errore: parametro 'start_line' mancante (oppure 'offset' come alias)]".to_string(),
        );
    };

    let end_line: usize = if let Some(n) = input.get("end_line").and_then(Value::as_u64) {
        if n < start_line as u64 {
            return Err("[Errore: 'end_line' deve essere >= start_line]".to_string());
        }
        n as usize
    } else if let Some(limit) = input.get("limit").and_then(Value::as_u64) {
        // offset + limit - 1 → end_line inclusa
        (start_line as u64).saturating_add(limit).saturating_sub(1) as usize
    } else {
        return Err(
            "[Errore: parametro 'end_line' mancante (oppure 'limit' come alias)]".to_string(),
        );
    };

    // Limita il range massimo per evitare di caricare troppe righe
    let end_line = end_line.min(start_line + READ_FILE_LINES_MAX - 1);
    Ok((start_line, end_line))
}

pub(super) async fn tool_read_file_lines(ctx: &AgentToolContext, input: &Value) -> String {
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'path' mancante]".to_string(),
    };

    let (start_line, end_line) = match parse_line_range(input) {
        Ok(range) => range,
        Err(msg) => return msg,
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
    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore lettura '{}': {}]", path_str, e),
    };

    render_line_range(&content, path_str, start_line, end_line)
}

/// Rende la porzione `[start_line, end_line]` (1-based, inclusi) del contenuto
/// con prefisso numerato "NNNN | testo" e un hint di continuazione se restano
/// righe. Errore esplicito se `start_line` supera il totale. Estratto da
/// `tool_read_file_lines`.
fn render_line_range(content: &str, path_str: &str, start_line: usize, end_line: usize) -> String {
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

/// Prepara la scrittura di `target`: crea le directory intermedie e registra il
/// tracking ripristinabile (mig 0349) leggendo lo stato PRIMA della scrittura,
/// cosi' un revert riporta il file allo stato attuale. Il record e' best-effort
/// (warn ma non blocca: l'agente non puo' restare bloccato per un bug della
/// tabella di audit). Ritorna se il file esisteva gia'. Estratto da
/// `tool_write_file`.
async fn prepare_write_and_track(
    ctx: &AgentToolContext,
    target: &Path,
    path_str: &str,
    content: &str,
) -> Result<bool, String> {
    // Crea directory intermedie se necessario
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Err(format!("[Errore creazione directory: {}]", e));
        }
    }
    let existed_before = target.exists();
    let before_for_track: Option<String> = if existed_before {
        tokio::fs::read_to_string(target).await.ok()
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
    Ok(existed_before)
}

/// Preambolo di `write_file`: permesso di scrittura, path presente e non
/// protetto, parametro `content` presente. Ritorna `(path_str, content)` o il
/// messaggio d'errore. Estratto da `tool_write_file`.
fn read_write_params<'a>(
    ctx: &AgentToolContext,
    input: &'a Value,
) -> Result<(&'a str, &'a str), String> {
    if !ctx.can_write {
        return Err("[Errore: permesso di scrittura non concesso su questo progetto]".to_string());
    }
    let path_str = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "[Errore: parametro 'path' mancante]".to_string())?;
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(path_str) {
            return Err(format!("[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente. Modifica manualmente se necessario.]", path_str, pattern));
        }
    }
    let content = input
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "[Errore: parametro 'content' mancante]".to_string())?;
    Ok((path_str, content))
}

pub(super) async fn tool_write_file(ctx: &AgentToolContext, input: &Value) -> String {
    let (path_str, content) = match read_write_params(ctx, input) {
        Ok(pair) => pair,
        Err(msg) => return msg,
    };

    // Governance risorse in scrittura (porte ADR 0010 + URL interni), punto
    // unico con audit: su violazione registra in nexus_resource_audit e
    // ritorna il rifiuto. Catalogo policy: nexus_resource_policies (mig 0397).
    if let Some(msg) =
        crate::security::resource_governance::enforce_on_write(ctx, "write_file", path_str, content)
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

    let existed_before = match prepare_write_and_track(ctx, &target, path_str, content).await {
        Ok(existed) => existed,
        Err(msg) => return msg,
    };
    match tokio::fs::write(&target, content).await {
        Ok(()) => on_write_success(ctx, &target, path_str, content, existed_before, bg_warning),
        Err(e) => format!("[Errore scrittura '{}': {}]", path_str, e),
    }
}

/// Post-scrittura riuscita di `write_file`: emette l'evento FileChanged
/// (created/modified), avvia i task di background (auto-commit + reindex/scan/
/// lint/doc) e compone il messaggio di successo. Estratto da `tool_write_file`.
fn on_write_success(
    ctx: &AgentToolContext,
    target: &Path,
    path_str: &str,
    content: &str,
    existed_before: bool,
    bg_warning: Option<String>,
) -> String {
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

    let autocommit_op = if existed_before { "modify" } else { "create" };
    spawn_autocommit_snapshot(ctx, autocommit_op, path_str);
    spawn_write_reindex(ctx, target, path_str, content);
    build_write_success_message(path_str, content.len(), bg_warning)
}

/// Avvia in background lo snapshot di auto-commit per sessione su branch
/// dedicato (rete di sicurezza sopra `file_mutations`). Se non e' un git repo /
/// setting disabilitato / session_id assente, il modulo fa no-op silenzioso.
/// Punto unico (regola L) condiviso da `tool_write_file` e `tool_edit_file`.
fn spawn_autocommit_snapshot(ctx: &AgentToolContext, op: &str, path_str: &str) {
    let ac_db = ctx.db.clone();
    let ac_root = ctx.root_path.clone();
    let ac_is_git = ctx.is_git_repo;
    let ac_sid = ctx.session_id;
    // Soppressione FASE 2 (buco B2): per un sub-run isolato l'autocommit e' no-op
    // (il flag e' passato alla funzione, che early-return). L'unica fonte del
    // commit e' l'apply atomico post-run (PR4).
    let ac_isolated = ctx.isolated_subrun;
    let ac_path = path_str.to_string();
    let ac_op = op.to_string();
    tokio::spawn(async move {
        crate::session_autocommit::snapshot_after_mutation(
            &ac_db,
            &ac_root,
            ac_is_git,
            ac_sid,
            ac_isolated,
            &ac_op,
            &ac_path,
        )
        .await;
    });
}

/// Avvia in background la re-indicizzazione del file nel code index, l'eventuale
/// auto-scan qualita', la ri-validazione delle violazioni di governance risorse
/// (regola H: niente residui nel pannello Problemi) e l'hook M2 di registrazione
/// documentazione (`upsert_project_document_if_doc`). Estratto da
/// `tool_write_file`.
fn spawn_write_reindex(ctx: &AgentToolContext, target: &Path, path_str: &str, content: &str) {
    // Soppressione FASE 2 (buco B2): per un sub-run ISOLATO il reindex
    // fire-and-forget e' un no-op. Indicizzerebbe path del worktree effimero
    // nell'indice neurale del PROGETTO (contenuti mai promossi alla root) e,
    // non atteso, correrebbe col cleanup del worktree (lettura di file gia'
    // rimossi, lock su Windows). Il reindex avviene UNA volta post-apply sui
    // soli file realmente promossi alla project_root (PR4).
    if ctx.isolated_subrun {
        return;
    }
    let db_bg = ctx.db.clone();
    let neural_bg = ctx.neural.clone();
    let project_id_bg = ctx.project_id;
    let root_bg = ctx.root_path.clone();
    let target_bg = target.to_path_buf();
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
        crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &root_bg, &target_bg).await;
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
        let _ =
            upsert_project_document_if_doc(&db_bg, project_id_bg, &path_str_bg, &content_bg).await;
    });
}

/// Compone il messaggio di successo di `write_file`: riga base + eventuale
/// warning build-graph + nota B2 sulle config critiche (che richiedono il
/// riavvio dei servizi per avere effetto, mig 0438). Estratto da
/// `tool_write_file`.
fn build_write_success_message(
    path_str: &str,
    byte_len: usize,
    bg_warning: Option<String>,
) -> String {
    let mut msg = format!(
        "File '{}' scritto con successo ({} byte)",
        path_str, byte_len
    );
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

/// Classifica un path `.md` (gia' lowercased) in uno dei `doc_type` ammessi dal
/// check constraint `project_documents_doc_type_check` (functional_analysis,
/// technical_analysis, er_diagram, project_management, release_notes). `None` se
/// il file non e' documentazione riconosciuta. Estratto da
/// `upsert_project_document_if_doc`.
fn classify_project_doc_type(lower: &str) -> Option<&'static str> {
    if lower.contains("prd")
        || lower.starts_with("specs/")
        || lower.contains("/specs/")
        || lower.contains("functional")
    {
        Some("functional_analysis")
    } else if lower.ends_with("readme.md")
        || lower.contains("architecture")
        || lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.contains("technical")
    {
        Some("technical_analysis")
    } else if lower.contains("erd")
        || lower.contains("schema_diagram")
        || lower.contains("er_diagram")
    {
        Some("er_diagram")
    } else if lower.contains("changelog") || lower.contains("release_notes") {
        Some("release_notes")
    } else if lower.contains("contributing")
        || lower.contains("project_management")
        || lower.contains("roadmap")
    {
        Some("project_management")
    } else {
        None
    }
}

/// Titolo del documento: prima riga "# ..." del contenuto, oppure il nome file
/// senza estensione. Troncato a 255 caratteri. Estratto da
/// `upsert_project_document_if_doc`.
fn extract_doc_title(content: &str, rel_path: &str) -> String {
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
    title.chars().take(255).collect::<String>()
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
    let doc_type = match classify_project_doc_type(&lower) {
        Some(t) => t,
        None => return Ok(()),
    };
    let title = extract_doc_title(content, rel_path);

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

    let stdout = match run_grep_or_fallback(pattern, &search_path).await {
        Ok(s) => s,
        Err(msg) => return msg,
    };

    format_search_output(ctx, pattern, &stdout)
}

/// Esegue `grep -rn --include=* --max-count=50 -I` su `search_path` e ne
/// ritorna lo stdout (formato "path:lineno:contenuto"). Se lo spawn fallisce
/// (grep assente, tipico Windows nativo) ripiega su [`search_in_files_rust`],
/// che produce lo STESSO formato cosi' il post-processing a valle resta unico
/// (regola L). `Err(msg)` solo quando grep gira ma emette un errore reale
/// (stdout vuoto + stderr non vuoto), da propagare al chiamante.
async fn run_grep_or_fallback(pattern: &str, search_path: &Path) -> Result<String, String> {
    let output = Command::new("grep")
        .arg("-rn")
        .arg("--include=*")
        .arg("--max-count=50")
        .arg("-I") // ignora file binari
        .arg(pattern)
        .arg(search_path)
        .output()
        .await;

    match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            // grep esce con 1 e stdout vuoto quando non trova nulla: non e' un errore.
            if stdout.is_empty() && !stderr.is_empty() {
                return Err(format!("[grep error: {}]", stderr.trim()));
            }
            Ok(stdout)
        }
        // grep assente (tipico Windows nativo): ricerca in-process, best-effort.
        Err(_) => Ok(search_in_files_rust(search_path, pattern)),
    }
}

/// Formatta l'output della ricerca (comune a grep e al fallback Rust): rende i
/// path relativi alla root e applica il troncamento. Punto unico (regola L): la
/// stessa logica serviva sia al ramo grep sia al fallback Windows.
fn format_search_output(ctx: &AgentToolContext, pattern: &str, stdout: &str) -> String {
    // Limite massimo di output: 500KB. Risultati piu' grandi causano
    // RESOURCE_EXHAUSTED gRPC (limite 16MB client Python) e consumano
    // troppi token di contesto per l'LLM. 500KB ~ 10k righe di codice.
    const MAX_OUTPUT_BYTES: usize = 500 * 1024;
    const MAX_OUTPUT_LINES: usize = 2000;

    if stdout.is_empty() {
        return format!("Nessun risultato per '{pattern}'.");
    }
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

/// Compila il predicato di match di riga per il fallback Rust: grep di default
/// interpreta il pattern come espressione regolare (BRE); `regex` usa ERE/PCRE-
/// like, che per i pattern comuni (letterali, classi, alternanze) coincide. Se
/// la compilazione fallisce si degrada a ricerca letterale (substring), sempre
/// case-sensitive. Estratto da `search_in_files_rust`.
fn compile_line_matcher(pattern: &str) -> impl Fn(&str) -> bool {
    let re = regex::Regex::new(pattern).ok();
    let literal = pattern.to_string();
    move |line: &str| match &re {
        Some(r) => r.is_match(line),
        None => line.contains(&literal),
    }
}

/// Ricerca ricorsiva in Rust puro, fallback cross-platform quando `grep` non e'
/// disponibile (Windows nativo). Riproduce il comportamento essenziale di
/// `grep -rn --max-count=50 -I`:
/// - cammina la directory con `std::fs` (nessuna dipendenza esterna);
/// - salta le entry nascoste (nome che inizia con '.') come il resto del modulo;
/// - salta i file binari (euristica: byte NUL nei primi 8 KB), come `-I`;
/// - al piu' 50 righe corrispondenti per file (`--max-count=50`);
/// - match case-sensitive come `grep` di default: `regex::Regex` (gia' dipendenza)
///   e, se il pattern non e' una regex valida, ricerca letterale con `contains`.
///
/// Formato riga identico a `grep -rn`: "<path_assoluto>:<lineno>:<contenuto>".
fn search_in_files_rust(root: &std::path::Path, pattern: &str) -> String {
    let matches = compile_line_matcher(pattern);

    // Budget difensivo per non camminare all'infinito su alberi enormi: ben oltre
    // il troncamento a valle (2000 righe / 500 KB), quindi non altera il risultato.
    const MAX_FILES_VISITED: usize = 50_000;
    const MAX_TOTAL_MATCHES: usize = 5_000;

    let mut out = String::new();
    let mut total_matches = 0usize;
    let mut files_visited = 0usize;
    // DFS iterativa (niente ricorsione: alberi profondi non fanno overflow).
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue, // permessi/inesistente: best-effort, salta
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue; // salta dotfile/dotdir (allineato a tool_list_files)
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue; // symlink/altro: non seguiamo (evita cicli)
            }
            files_visited += 1;
            if files_visited > MAX_FILES_VISITED || total_matches >= MAX_TOTAL_MATCHES {
                return out;
            }
            let remaining = MAX_TOTAL_MATCHES - total_matches;
            total_matches += append_file_matches(&path, &matches, remaining, &mut out);
        }
    }
    out
}

/// Cerca `matches` nel file `path` e appende in `out` le righe corrispondenti in
/// formato `grep -rn` ("path:lineno:contenuto"). Salta i binari (euristica `-I`:
/// byte NUL nei primi 8 KB) e i file illeggibili. Limita a min(50, `remaining`)
/// match. Ritorna il numero di match appesi. Estratto da `search_in_files_rust`.
fn append_file_matches(
    path: &Path,
    matches: &impl Fn(&str) -> bool,
    remaining: usize,
    out: &mut String,
) -> usize {
    const MAX_MATCHES_PER_FILE: usize = 50;
    const BINARY_SNIFF_BYTES: usize = 8 * 1024;

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    // Euristica `-I`: file binario se contiene un NUL nell'intestazione.
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0) {
        return 0;
    }
    let cap = MAX_MATCHES_PER_FILE.min(remaining);
    let text = String::from_utf8_lossy(&bytes);
    let path_str = path.to_string_lossy();
    let mut per_file = 0usize;
    for (idx, line) in text.lines().enumerate() {
        if matches(line) {
            out.push_str(&format!("{}:{}:{}\n", path_str, idx + 1, line));
            per_file += 1;
            if per_file >= cap {
                break;
            }
        }
    }
    per_file
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
        return delete_directory(&target, path_str, recursive).await;
    }
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

/// Elimina una directory, ricorsivamente se `recursive`. Estratto da
/// `tool_delete_file`: il ramo non-ricorsivo suggerisce `recursive:true` se la
/// directory non e' vuota.
async fn delete_directory(target: &Path, path_str: &str, recursive: bool) -> String {
    if recursive {
        match tokio::fs::remove_dir_all(target).await {
            Ok(()) => format!(
                "Directory '{}' eliminata ricorsivamente con successo",
                path_str
            ),
            Err(e) => format!("[Errore eliminazione directory '{}': {}]", path_str, e),
        }
    } else {
        match tokio::fs::remove_dir(target).await {
            Ok(()) => format!("Directory '{}' eliminata con successo", path_str),
            Err(e) => format!(
                "[Errore eliminazione directory '{}': {} (se non e' vuota usa recursive:true)]",
                path_str, e
            ),
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

/// Rende i blocchi "Occorrenza N (~riga M):" con l'estratto numerato attorno a
/// ciascuna hit (finestra +3/-6 righe, cap 900 byte per blocco). Estratto da
/// `build_old_string_ambiguous_message`; il valore ritornato e' gia' trimmato
/// dei newline finali. Riusa il punto unico [`render_numbered_window`].
fn render_occurrence_blocks(lines: &[&str], hit_lines: &[usize]) -> String {
    const WINDOW_BEFORE: usize = 3;
    const WINDOW_AFTER: usize = 6;
    const MAX_BYTES_PER_HIT: usize = 900;

    let total_lines = lines.len();
    let mut blocks = String::new();
    for (n, &hit) in hit_lines.iter().enumerate() {
        let start = hit.saturating_sub(WINDOW_BEFORE);
        let end = (hit + WINDOW_AFTER + 1).min(total_lines);
        let (excerpt, _) = render_numbered_window(lines, start, end, MAX_BYTES_PER_HIT);
        blocks.push_str(&format!(
            "Occorrenza {} (~riga {}):\n{}\n\n",
            n + 1,
            hit + 1,
            excerpt
        ));
    }
    // Rimuove i due newline finali per pulizia.
    blocks.trim_end().to_string()
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
    const MAX_HITS_SHOWN: usize = 3;

    let lines: Vec<&str> = content.lines().collect();
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

    let blocks = render_occurrence_blocks(&lines, &hit_lines);
    let more = if count > hit_lines.len() {
        format!(
            " (mostrate le prime {} di {} occorrenze)",
            hit_lines.len(),
            count
        )
    } else {
        String::new()
    };

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
fn build_old_string_not_found_message(
    content: &str,
    old_string_lf: &str,
    path_str: &str,
) -> String {
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
        Some(i) => format!(
            "Contenuto attuale attorno alla riga {} (righe {}..{})",
            i + 1,
            start + 1,
            lines_shown_end
        ),
        None => format!(
            "Contenuto attuale (righe {}..{})",
            start + 1,
            lines_shown_end
        ),
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

/// Preambolo di `edit_file`: permesso di scrittura, path presente e non
/// protetto, parametri `old_string`/`new_string` presenti. Ritorna la tripla
/// `(path_str, old_string, new_string)` o il messaggio d'errore. Estratto da
/// `tool_edit_file`.
fn read_edit_params<'a>(
    ctx: &AgentToolContext,
    input: &'a Value,
) -> Result<(&'a str, &'a str, &'a str), String> {
    if !ctx.can_write {
        return Err("[Errore: permesso di scrittura non concesso su questo progetto]".to_string());
    }
    let path_str = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "[Errore: parametro 'path' mancante]".to_string())?;
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(path_str) {
            return Err(format!("[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente.]", path_str, pattern));
        }
    }
    let old_string = input
        .get("old_string")
        .and_then(Value::as_str)
        .ok_or_else(|| "[Errore: parametro 'old_string' mancante]".to_string())?;
    let new_string = input
        .get("new_string")
        .and_then(Value::as_str)
        .ok_or_else(|| "[Errore: parametro 'new_string' mancante]".to_string())?;
    Ok((path_str, old_string, new_string))
}

pub(super) async fn tool_edit_file(ctx: &AgentToolContext, input: &Value) -> String {
    let (path_str, old_string, new_string) = match read_edit_params(ctx, input) {
        Ok(triple) => triple,
        Err(msg) => return msg,
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

    edit_matched_content(ctx, &target, path_str, old_string, new_string, bg_warning).await
}

/// Legge il file, normalizza CRLF -> LF per un matching consistente e, in base al
/// numero di occorrenze di `old_string`, ritorna il messaggio di errore
/// (0 = non trovato, N>1 = ambiguo) o applica la sostituzione univoca. Estratto
/// da `tool_edit_file`.
async fn edit_matched_content(
    ctx: &AgentToolContext,
    target: &Path,
    path_str: &str,
    old_string: &str,
    new_string: &str,
    bg_warning: Option<String>,
) -> String {
    let raw_content = match tokio::fs::read_to_string(target).await {
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
        n if n > 1 => build_old_string_ambiguous_message(&content, &old_string_lf, path_str, n),
        _ => {
            apply_edit_and_persist(
                ctx,
                target,
                path_str,
                EditApply {
                    content_lf: &content,
                    old_string_lf: &old_string_lf,
                    new_string_lf: &new_string_lf,
                    raw_content: &raw_content,
                    was_crlf,
                    bg_warning,
                },
            )
            .await
        }
    }
}

/// Parametri della sostituzione da persistere in [`apply_edit_and_persist`].
/// Raggruppati per evitare una firma con troppi argomenti (clippy).
struct EditApply<'a> {
    /// Contenuto attuale LF-normalizzato del file.
    content_lf: &'a str,
    /// `old_string` LF-normalizzato (gia' verificato univoco dal chiamante).
    old_string_lf: &'a str,
    /// `new_string` LF-normalizzato che sostituisce l'occorrenza.
    new_string_lf: &'a str,
    /// Contenuto preesistente grezzo (EOL originali), per il tracking mutazioni.
    raw_content: &'a str,
    /// Vero se il file originale usava CRLF: gli EOL vengono ripristinati.
    was_crlf: bool,
    /// Warning build-graph da accodare al messaggio di successo.
    bg_warning: Option<String>,
}

/// Tracking ripristinabile (mig 0349) per un `edit_file`: registra before/after
/// PRIMA della scrittura (`before` e' il contenuto preesistente gia' letto dal
/// chiamante). Best-effort: warn ma non blocca. Estratto da
/// `apply_edit_and_persist`.
async fn record_edit_mutation(ctx: &AgentToolContext, path_str: &str, before: &str, after: &str) {
    if let Err(e) = crate::file_mutations::record_mutation(
        &ctx.db,
        ctx.project_id,
        ctx.session_id,
        Some(ctx.user_id),
        path_str,
        "edit_file",
        Some(before),
        Some(after),
    )
    .await
    {
        tracing::warn!(
            project_id = %ctx.project_id, path = %path_str,
            "file_mutations::record_mutation fallita (edit_file): {e}"
        );
    }
}

/// Applica la sostituzione univoca (gia' validata dal chiamante), ripristina gli
/// EOL originali, registra il tracking mutazioni, scrive il file e avvia i task
/// di background (auto-commit + reindex/scan/lint). Ritorna il messaggio finale.
/// Estratto dal ramo di successo di `tool_edit_file`.
async fn apply_edit_and_persist(
    ctx: &AgentToolContext,
    target: &Path,
    path_str: &str,
    apply: EditApply<'_>,
) -> String {
    let new_content_lf = apply
        .content_lf
        .replacen(apply.old_string_lf, apply.new_string_lf, 1);
    // Ripristina gli EOL originali del file (CRLF se l'originale era CRLF).
    // Senza questo, ogni edit di un file Windows convertirebbe l'intero file
    // in LF generando un diff git rumoroso (bug 14).
    let new_content = if apply.was_crlf {
        new_content_lf.replace('\n', "\r\n")
    } else {
        new_content_lf
    };
    record_edit_mutation(ctx, path_str, apply.raw_content, &new_content).await;
    match tokio::fs::write(target, &new_content).await {
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
            spawn_autocommit_snapshot(ctx, "modify", path_str);
            spawn_edit_reindex(ctx, target);
            let base = format!(
                "File '{}' modificato con successo ({} byte → {} byte)",
                path_str,
                apply.content_lf.len(),
                new_content.len()
            );
            match apply.bg_warning {
                Some(w) => format!("{}\n\n{}", base, w),
                None => base,
            }
        }
        Err(e) => format!("[Errore scrittura '{}': {}]", path_str, e),
    }
}

/// Avvia in background la re-indicizzazione del file dopo un `edit_file`:
/// reindex nel code index, eventuale auto-scan qualita' e ri-validazione delle
/// violazioni di governance risorse risolte dall'edit (regola H: niente residui
/// nel pannello Problemi). Variante di `spawn_write_reindex` senza l'hook M2 sui
/// documenti (edit non ricrea il .md da zero). Estratto da `tool_edit_file`.
fn spawn_edit_reindex(ctx: &AgentToolContext, target: &Path) {
    // Soppressione FASE 2 (buco B2): sub-run isolato -> reindex no-op (stesso
    // razionale di `spawn_write_reindex`: worktree effimero, race col cleanup).
    // Reindex-once post-apply sui file promossi alla root (PR4).
    if ctx.isolated_subrun {
        return;
    }
    let db_bg = ctx.db.clone();
    let neural_bg = ctx.neural.clone();
    let project_id_bg = ctx.project_id;
    let root_bg = ctx.root_path.clone();
    let target_bg = target.to_path_buf();
    tokio::spawn(async move {
        let _ = crate::projects::reindex_single_file(
            &db_bg,
            &neural_bg,
            project_id_bg,
            &root_bg,
            &target_bg,
        )
        .await;
        crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &root_bg, &target_bg).await;
        crate::security::resource_linter::revalidate_file_violations(
            &db_bg,
            project_id_bg,
            &root_bg.to_string_lossy(),
            &target_bg,
        )
        .await;
    });
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

    let (from, to) = match resolve_from_to(&ctx.root_path, from_str, to_str) {
        Ok(pair) => pair,
        Err(msg) => return msg,
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

    copy_from_to(&from, &to, from_str, to_str).await
}

/// Esegue la copia risolta: file singolo (creando le directory genitore) o
/// directory ricorsiva. Estratto da `tool_fs_copy` per coesione e brevita'.
async fn copy_from_to(from: &Path, to: &Path, from_str: &str, to_str: &str) -> String {
    if from.is_file() {
        // Crea directory genitore se non esiste
        if let Some(parent) = to.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return format!("[Errore creazione directory destinazione: {}]", e);
            }
        }
        match tokio::fs::copy(from, to).await {
            Ok(bytes) => format!(
                "File copiato '{}' -> '{}' ({} byte)",
                from_str, to_str, bytes
            ),
            Err(e) => format!("[Errore copia file: {}]", e),
        }
    } else if from.is_dir() {
        match copy_dir_recursive(from, to).await {
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

/// Risolve una coppia di path relativi (`from`/`to`) confinandoli alla root,
/// con messaggi d'errore distinti "sorgente"/"destinazione". Punto unico
/// (regola L) del pattern condiviso da `tool_fs_move`/`tool_fs_copy`.
fn resolve_from_to(
    root: &Path,
    from_str: &str,
    to_str: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let from = resolve_relative_path(root, from_str).map_err(|e| {
        format!(
            "[Errore percorso sorgente: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        )
    })?;
    let to = resolve_relative_path(root, to_str).map_err(|e| {
        format!(
            "[Errore percorso destinazione: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        )
    })?;
    Ok((from, to))
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

    let (from, to) = match resolve_from_to(&ctx.root_path, from_str, to_str) {
        Ok(pair) => pair,
        Err(msg) => return msg,
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
        assert!(
            msg.starts_with("[Errore: old_string non trovato nel file 'src/lib.rs'."),
            "header originale non preservato: {}",
            msg
        );
        assert!(
            msg.contains("NON chiamare read_file"),
            "warning anti-loop perso: {}",
            msg
        );

        // 2. Riga simile correttamente individuata (riga 42).
        assert!(
            msg.contains("Prima riga simile trovata ~riga 42."),
            "ancora di navigazione non presente: {}",
            msg
        );

        // 3. Header dell'estratto presente con riferimento alla riga 42.
        assert!(
            msg.contains("Contenuto attuale attorno alla riga 42"),
            "header dell'estratto attorno alla riga simile mancante: {}",
            msg
        );

        // 4. Estratto NUMERATO contiene la riga 42 ed alcune righe attorno
        //    (finestra +/- 15: dovrebbe coprire almeno 27 e 57).
        assert!(
            msg.contains("  42 | pub fn target_function"),
            "riga 42 numerata non presente nell'estratto: {}",
            msg
        );
        assert!(
            msg.contains("  30 | "),
            "riga 30 (window before) attesa nell'estratto: {}",
            msg
        );
        assert!(
            msg.contains("  55 | "),
            "riga 55 (window after) attesa nell'estratto: {}",
            msg
        );

        // 5. Limite di sicurezza: il messaggio totale deve restare contenuto
        //    (tetto ~2 KB sull'estratto + margine).
        assert!(
            msg.len() < 4096,
            "messaggio sopra il tetto ragionevole ({}B): {}",
            msg.len(),
            msg
        );
    }

    #[test]
    fn fallback_alle_prime_righe_se_nessun_token_simile() {
        let content = "alpha\nbeta\ngamma\ndelta\n".to_string();
        let old_string = "stringa_che_non_compare_da_nessuna_parte_xyz123";

        let msg = build_old_string_not_found_message(&content, old_string, "f.txt");

        assert!(
            msg.contains("Nessuna riga contiene il primo token di old_string."),
            "hint di assenza atteso: {}",
            msg
        );
        // L'estratto di fallback parte dalla riga 1.
        assert!(
            msg.contains("   1 | alpha"),
            "fallback alle prime righe non emesso: {}",
            msg
        );
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
        assert!(
            msg.contains("trovato 3 volte in 'src/lib.rs'"),
            "conteggio mancante: {}",
            msg
        );
        assert!(
            msg.contains("deve essere UNICO"),
            "vincolo univocita' mancante: {}",
            msg
        );
        // Anti-loop: niente read_file, il contesto e' gia' qui.
        assert!(
            msg.contains("NON chiamare read_file"),
            "anti-loop mancante: {}",
            msg
        );
        // Estratto numerato presente con la riga reale dell'occorrenza.
        assert!(
            msg.contains("| val = compute();"),
            "riga numerata dell'occorrenza mancante: {}",
            msg
        );
        // Etichette di occorrenza multipla.
        assert!(
            msg.contains("Occorrenza 1"),
            "etichetta occorrenza 1 mancante: {}",
            msg
        );
        assert!(
            msg.contains("Occorrenza 2"),
            "etichetta occorrenza 2 mancante: {}",
            msg
        );
    }
}
