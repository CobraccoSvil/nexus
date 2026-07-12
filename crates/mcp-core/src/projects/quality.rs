// Scansione qualita' del codice: run, get findings, mark fixed, scan singolo file, file lines.
// Refactor 0104: scan asincrona con polling, risolve bug 14b (proxy timeout 30s).

use super::*;

// ── Struttura dati per i finding arricchiti ───────────────────────────────────

/// Riga di finding con tutti i campi, inclusi quelli vettoriali (migrazione 0105).
#[derive(Clone)]
struct FindingRow {
    file: String,
    category: String,
    severity: String,
    title: String,
    detail: String,
    line_number: Option<i32>,
    confidence: Option<String>,
    context_snippet: Option<String>,
    related_files: Option<Vec<String>>,
    is_auto_suppressed: bool,
}

// ── Helper: raccolta file sorgente ────────────────────────────────────────────

/// Raccoglie file sorgente ricorsivamente, escludendo build dirs
pub(super) fn collect_source_files(root: &str, extensions: &[&str]) -> Vec<String> {
    let skip_dirs = [
        "node_modules",
        ".git",
        "target",
        "dist",
        "build",
        ".next",
        "obj",
        "bin",
        "__pycache__",
        ".venv",
        "venv",
    ];
    let mut result = Vec::new();
    collect_recursive(root, extensions, &skip_dirs, &mut result, 0);
    result
}

pub(super) fn collect_recursive(
    dir: &str,
    extensions: &[&str],
    skip_dirs: &[&str],
    result: &mut Vec<String>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !skip_dirs.contains(&name) {
                collect_recursive(
                    &path.to_string_lossy(),
                    extensions,
                    skip_dirs,
                    result,
                    depth + 1,
                );
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    result.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
}

// ── Handler HTTP ──────────────────────────────────────────────────────────────

/// Risolve la root del progetto (repo o workspace) come `COALESCE(r.root_path,
/// w.absolute_path)`. Punto unico condiviso dagli handler `run_quality_scan` e
/// `scan_single_file` (regola L). 404 se il progetto non esiste, 500 su errore DB.
async fn resolve_project_root_path(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<String, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        r#"SELECT COALESCE(r.root_path, w.absolute_path) AS root_path
           FROM projects p
           LEFT JOIN repositories r ON r.project_id = p.id
           LEFT JOIN workspaces w ON w.project_id = p.id
           WHERE p.id = $1
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "project not found".to_string()))?;

    row.try_get("root_path")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Inserisce la riga di scan in stato 'running' e ritorna lo `scan_id`.
async fn insert_running_scan(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<i64, (StatusCode, Json<Value>)> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO nexus_quality_scans (project_id, status) \
         VALUES ($1, 'running') RETURNING id",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| {
        // NB: falso positivo del detector SQL-injection (ADR 0021): la parola
        // "insert" in questo messaggio d'errore matcha \bINSERT\b + il segnaposto
        // {e} matcha format!. Non e' costruzione di query: e' solo testo d'errore.
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("insert scan: {e}"),
        )
    })
}

/// POST /api/projects/:id/quality-scan
///
/// Refactor 0104: handler ASINCRONO. Spezzato in:
///  - Fase sync (rapida): risolve root_path, insert riga 'running' in
///    nexus_quality_scans, return 202 + scan_id
///  - Fase async (background tokio task): scan filesystem, insert findings,
///    UPDATE riga finale a 'completed' o 'failed'
///
/// Risolve bug 14b (test E2E redemptor): scan sincrona >30s causava timeout
/// del proxy Next.js, che droppava connessione, Axum abortiva la scan
/// (cancel handler) lasciando 0 findings nel DB.
///
/// Il client ora: POST -> riceve 202 + scan_id, poi polla
/// GET /api/projects/:id/quality-scan/:scan_id finche' status != 'running'.
pub async fn run_quality_scan(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let root_path = resolve_project_root_path(&state.db, project_id).await?;

    // Insert riga 'running' e return 202 + scan_id immediatamente.
    let scan_id = insert_running_scan(&state.db, project_id).await?;

    let db = state.db.clone();
    let orchestrator = state.orchestrator.clone();
    let dep_status = state.dependency_status.clone();
    let channels = state.project_channels.clone();
    tokio::spawn(async move {
        run_quality_scan_background(
            db,
            orchestrator,
            dep_status,
            channels,
            project_id,
            scan_id,
            root_path,
        )
        .await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "scan_id": scan_id,
            "status": "running",
            "message": "Scansione avviata in background. Polla GET /api/projects/:id/quality-scan/:scan_id ogni 2s finche' status != 'running'.",
        })),
    ))
}

/// Corpo del task background della scan: emette l'evento di start, esegue la
/// scan completa e finalizza la riga `nexus_quality_scans` a 'completed' o
/// 'failed'. Estratto da `run_quality_scan` per contenerne la lunghezza.
#[allow(clippy::too_many_arguments)]
async fn run_quality_scan_background(
    db: sqlx::PgPool,
    orchestrator: crate::orchestrator::Orchestrator,
    dep_status: crate::task_watchdog::DependencyStatusRef,
    channels: nexus_events::ProjectChannels,
    project_id: Uuid,
    scan_id: i64,
    root_path: String,
) {
    let started = std::time::Instant::now();
    nexus_events::dispatcher::emit(
        &channels,
        project_id,
        nexus_events::ProjectEvent::QualityScanProgress {
            scan_id: scan_id.to_string(),
            phase: "started".to_string(),
            percent: Some(0),
        },
    );
    match perform_quality_scan(&db, &orchestrator, project_id, &root_path, &dep_status).await {
        Ok((files_scanned, total_findings, by_severity, by_category)) => {
            let duration_ms = started.elapsed().as_millis() as i32;
            finalize_scan_completed(
                &db,
                &channels,
                project_id,
                scan_id,
                files_scanned,
                total_findings,
                &by_severity,
                &by_category,
                duration_ms,
            )
            .await;
        }
        Err(e) => {
            let duration_ms = started.elapsed().as_millis() as i32;
            finalize_scan_failed(&db, scan_id, &e, duration_ms).await;
        }
    }
}

/// Persiste l'esito 'completed' della scan ed emette gli eventi realtime
/// (progress 100% + FindingsUpdated). Best-effort.
#[allow(clippy::too_many_arguments)]
async fn finalize_scan_completed(
    db: &sqlx::PgPool,
    channels: &nexus_events::ProjectChannels,
    project_id: Uuid,
    scan_id: i64,
    files_scanned: usize,
    total_findings: u32,
    by_severity: &std::collections::HashMap<String, u32>,
    by_category: &std::collections::HashMap<String, u32>,
    duration_ms: i32,
) {
    let by_sev_json = serde_json::to_value(by_severity).unwrap_or(json!({}));
    let by_cat_json = serde_json::to_value(by_category).unwrap_or(json!({}));
    let _ = sqlx::query(
        "UPDATE nexus_quality_scans \
         SET status = 'completed', files_scanned = $1, total_findings = $2, \
             by_severity = $3, by_category = $4, duration_ms = $5, \
             completed_at = NOW() \
         WHERE id = $6",
    )
    .bind(files_scanned as i32)
    .bind(total_findings as i32)
    .bind(&by_sev_json)
    .bind(&by_cat_json)
    .bind(duration_ms)
    .bind(scan_id)
    .execute(db)
    .await;
    emit_scan_completed_events(channels, project_id, scan_id, total_findings, by_severity);
    tracing::info!(
        "quality_scan background: scan_id={} project_id={} files={} findings={} duration_ms={}",
        scan_id,
        project_id,
        files_scanned,
        total_findings,
        duration_ms
    );
}

/// Emette gli eventi realtime di fine scan: progress 100% + FindingsUpdated.
fn emit_scan_completed_events(
    channels: &nexus_events::ProjectChannels,
    project_id: Uuid,
    scan_id: i64,
    total_findings: u32,
    by_severity: &std::collections::HashMap<String, u32>,
) {
    nexus_events::dispatcher::emit(
        channels,
        project_id,
        nexus_events::ProjectEvent::QualityScanProgress {
            scan_id: scan_id.to_string(),
            phase: "completed".to_string(),
            percent: Some(100),
        },
    );
    nexus_events::dispatcher::emit(
        channels,
        project_id,
        nexus_events::ProjectEvent::FindingsUpdated {
            scan_id: None,
            total: total_findings as i64,
            critical: *by_severity.get("critical").unwrap_or(&0) as i64,
            warnings: *by_severity.get("warning").unwrap_or(&0) as i64,
            resolved_ids: vec![],
        },
    );
}

/// Persiste l'esito 'failed' della scan con il messaggio d'errore. Best-effort.
async fn finalize_scan_failed(db: &sqlx::PgPool, scan_id: i64, error: &str, duration_ms: i32) {
    let _ = sqlx::query(
        "UPDATE nexus_quality_scans \
         SET status = 'failed', error_message = $1, duration_ms = $2, completed_at = NOW() \
         WHERE id = $3",
    )
    .bind(error)
    .bind(duration_ms)
    .bind(scan_id)
    .execute(db)
    .await;
    tracing::warn!(
        "quality_scan background: scan_id={} FAILED: {}",
        scan_id,
        error
    );
}

/// Esegue la scansione qualita' completa. Chiamata dal task background.
/// Ritorna (files_scanned, total_findings, by_severity, by_category).
///
/// Refactor 0105: accetta l'Orchestrator per accedere all'embedder
/// e al code index vettoriale (Qdrant). Dopo la scan regex, arricchisce
/// i finding con:
/// - context_snippet (righe attorno al finding)
/// - related_files (file semanticamente simili via code index)
/// - confidence (validazione falsi positivi via analisi vettoriale)
/// - duplicati semantici (funzioni simili in file diversi)
async fn perform_quality_scan(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    root_path: &str,
    dep_status: &crate::task_watchdog::DependencyStatusRef,
) -> Result<
    (
        usize,
        u32,
        std::collections::HashMap<String, u32>,
        std::collections::HashMap<String, u32>,
    ),
    String,
> {
    // Salva i falsi positivi, fa fresh-start e raccoglie i finding regex per file.
    let (fp_rows, mut batch, file_contents, files_len) =
        collect_regex_batch(db, project_id, root_path).await;

    // Fase vettoriale (arricchimento) + duplicati semantici, con guard watchdog.
    run_semantic_phase(
        db,
        orchestrator,
        project_id,
        &mut batch,
        &file_contents,
        dep_status,
    )
    .await;

    // Conta totali (esclusi gli auto-soppressi).
    let mut total_findings = 0u32;
    let mut findings_by_severity: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut findings_by_category: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    count_findings_into(
        &batch,
        &mut total_findings,
        &mut findings_by_severity,
        &mut findings_by_category,
    );

    insert_findings_batch(db, project_id, &batch).await;
    reapply_false_positives(db, project_id, &fp_rows).await;
    upsert_db_profile(db, project_id, root_path).await;

    Ok((
        files_len,
        total_findings,
        findings_by_severity,
        findings_by_category,
    ))
}

/// Salva i falsi positivi correnti, esegue il fresh-start (DELETE) e raccoglie i
/// finding regex file per file. Ritorna `(fp_rows, batch, file_contents,
/// numero_file)`. Estratto da `perform_quality_scan`.
async fn collect_regex_batch(
    db: &sqlx::PgPool,
    project_id: Uuid,
    root_path: &str,
) -> (
    Vec<sqlx::postgres::PgRow>,
    Vec<FindingRow>,
    std::collections::HashMap<String, String>,
    usize,
) {
    // Salva i falsi positivi precedenti prima di cancellare
    let fp_rows = sqlx::query(
        "SELECT file_path, line_number, category, title, false_positive_reason \
         FROM project_quality_findings \
         WHERE project_id = $1 AND is_false_positive = TRUE",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Elimina tutti i finding (fresh start)
    let _ = sqlx::query("DELETE FROM project_quality_findings WHERE project_id = $1")
        .bind(project_id)
        .execute(db)
        .await;

    let extensions = ["rs", "ts", "tsx", "js", "jsx", "py", "sql", "cs", "go"];
    let mut batch: Vec<FindingRow> = Vec::new();
    // Cache contenuti file: (rel_path -> contenuto) per arricchimento vettoriale
    let mut file_contents: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let files = collect_source_files(root_path, &extensions);
    for file_path in &files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rel_str = relative_path_str(file_path, root_path);
        file_contents.insert(rel_str.clone(), content.clone());
        scan_one_file_into_batch(file_path, &rel_str, &content, &mut batch);
    }

    (fp_rows, batch, file_contents, files.len())
}

/// Fase vettoriale + duplicati semantici sul batch di finding. Guard: se il
/// watchdog segnala Qdrant/embedder down, salta tutto senza tentativi. Timeout
/// 60s per fase come difesa di ultima istanza. Muta il batch in-place.
async fn run_semantic_phase(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    batch: &mut Vec<FindingRow>,
    file_contents: &std::collections::HashMap<String, String>,
    dep_status: &crate::task_watchdog::DependencyStatusRef,
) {
    if !semantic_deps_ready(dep_status) {
        return;
    }
    run_vector_enrichment_phase(db, orchestrator, project_id, batch, file_contents).await;
    let semantic_dups = run_semantic_dup_phase(db, orchestrator, project_id, file_contents).await;
    for dup in semantic_dups {
        batch.push(dup);
    }
}

/// Guard watchdog: true se sia Qdrant sia l'embedder risultano sani (nessuno
/// skip). Logga in caso di skip.
fn semantic_deps_ready(dep_status: &crate::task_watchdog::DependencyStatusRef) -> bool {
    let qdrant_ok = dep_status.qdrant.load(std::sync::atomic::Ordering::Relaxed);
    let embedder_ok = dep_status
        .embedder
        .load(std::sync::atomic::Ordering::Relaxed);
    if !qdrant_ok || !embedder_ok {
        tracing::info!(
            "quality_scan: skip fase vettoriale (watchdog: qdrant={}, embedder={})",
            qdrant_ok,
            embedder_ok
        );
        return false;
    }
    true
}

/// Arricchimento vettoriale con timeout 60s; logga se non disponibile.
async fn run_vector_enrichment_phase(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    batch: &mut [FindingRow],
    file_contents: &std::collections::HashMap<String, String>,
) {
    let vector_enrichment_ok = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        enrich_findings_with_vectors(db, orchestrator, project_id, batch, file_contents),
    )
    .await
    {
        Ok(ok) => ok,
        Err(_) => {
            tracing::warn!(
                "quality_scan: timeout 60s nell'arricchimento vettoriale per progetto {}, \
                 proseguo con soli risultati regex",
                project_id
            );
            false
        }
    };
    if !vector_enrichment_ok {
        tracing::info!(
            "quality_scan: arricchimento vettoriale non disponibile per progetto {}, \
             i finding mantengono solo analisi regex",
            project_id
        );
    }
}

/// Ricerca duplicati semantici con timeout 60s; Vec vuoto su timeout.
async fn run_semantic_dup_phase(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    file_contents: &std::collections::HashMap<String, String>,
) -> Vec<FindingRow> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        detect_semantic_duplicates(db, orchestrator, project_id, file_contents),
    )
    .await
    {
        Ok(dups) => dups,
        Err(_) => {
            tracing::warn!(
                "quality_scan: timeout 60s nella ricerca duplicati semantici per progetto {}, skip",
                project_id
            );
            Vec::new()
        }
    }
}

/// Normalizza il path assoluto di un file nel path relativo alla root progetto,
/// con separatori POSIX. Usato come chiave della cache contenuti e dei finding.
/// Robusto al prefisso verbatim Windows (`\\?\`) su ENTRAMBI gli input: root
/// canonicalizzate e target dei tool possono arrivare in quella forma e lo
/// strip_prefix per componenti non matcherebbe mai una coppia mista.
fn relative_path_str(file_path: &str, root_path: &str) -> String {
    use nexus_types::workspace_paths::strip_windows_verbatim;
    let file_clean = strip_windows_verbatim(file_path);
    let root_clean = strip_windows_verbatim(root_path);
    let rel_path = std::path::Path::new(file_clean.as_ref())
        .strip_prefix(root_clean.as_ref())
        .unwrap_or(std::path::Path::new(file_clean.as_ref()));
    rel_path
        .to_string_lossy()
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .replace('\\', "/")
}

/// Raccoglie i finding regex di un file come tuple
/// `(category, severity, title, detail, line_number)`: SQL via `mcp_db`
/// (line_number sempre None), resto via `mcp_quality`.
fn collect_file_findings_tuples(
    file_path: &str,
    rel_str: &str,
    content: &str,
) -> Vec<(String, String, String, String, Option<i32>)> {
    if file_path.ends_with(".sql") {
        mcp_db::analyze_query(content)
            .findings
            .iter()
            .map(|f| {
                (
                    f.category.clone(),
                    f.severity.clone(),
                    f.title.clone(),
                    f.detail.clone(),
                    None,
                )
            })
            .collect()
    } else {
        mcp_quality::analyze_source(rel_str, content)
            .findings
            .iter()
            .map(|f| {
                (
                    f.category.clone(),
                    f.severity.clone(),
                    f.title.clone(),
                    f.detail.clone(),
                    f.line.map(|l| l as i32),
                )
            })
            .collect()
    }
}

/// Analizza un singolo file (SQL via `mcp_db`, resto via `mcp_quality`) e
/// accoda i finding regex nel batch, generando il context_snippet quando c'e'
/// un numero di riga. Comportamento identico al ciclo inline originale.
fn scan_one_file_into_batch(
    file_path: &str,
    rel_str: &str,
    content: &str,
    batch: &mut Vec<FindingRow>,
) {
    let file_findings = collect_file_findings_tuples(file_path, rel_str, content);

    for (category, severity, title, detail, line_number) in file_findings {
        // Genera context_snippet se abbiamo un numero di riga
        let snippet =
            line_number.map(|ln| mcp_quality::extract_context_snippet(content, ln as usize, 5));

        batch.push(FindingRow {
            file: rel_str.to_string(),
            category,
            severity,
            title,
            detail,
            line_number,
            confidence: None, // compilato dopo dalla fase vettoriale
            context_snippet: snippet,
            related_files: None, // compilato dopo dalla fase vettoriale
            is_auto_suppressed: false,
        });
    }
}

/// Conta i finding non auto-soppressi aggregandoli per severity e categoria.
fn count_findings_into(
    batch: &[FindingRow],
    total: &mut u32,
    by_severity: &mut std::collections::HashMap<String, u32>,
    by_category: &mut std::collections::HashMap<String, u32>,
) {
    for row in batch {
        if !row.is_auto_suppressed {
            *total += 1;
            *by_severity.entry(row.severity.clone()).or_insert(0) += 1;
            *by_category.entry(row.category.clone()).or_insert(0) += 1;
        }
    }
}

/// Costruisce l'INSERT multi-row per `n_rows` finding (11 colonne per riga),
/// generando i segnaposto parametrizzati `($1, $2, ...)`.
fn build_findings_insert_sql(n_rows: usize) -> String {
    let cols = 11; // project_id, file_path, category, severity, title, detail, line_number, confidence, context_snippet, related_files, is_auto_suppressed
    let mut q = String::from(
        "INSERT INTO project_quality_findings \
         (project_id, file_path, category, severity, title, detail, line_number, \
          confidence, context_snippet, related_files, is_auto_suppressed) VALUES ",
    );
    for i in 0..n_rows {
        if i > 0 {
            q.push(',');
        }
        let base = i * cols + 1;
        q.push_str(&format!(
            "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
            base,
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
            base + 8,
            base + 9,
            base + 10
        ));
    }
    q
}

/// Insert batchata dei finding: chunk da 200 row (piu' colonne = query piu'
/// lunga). Best-effort: gli errori di insert sono ignorati come nell'originale.
async fn insert_findings_batch(db: &sqlx::PgPool, project_id: Uuid, batch: &[FindingRow]) {
    for chunk in batch.chunks(200) {
        if chunk.is_empty() {
            continue;
        }
        let q = build_findings_insert_sql(chunk.len());
        let mut query = sqlx::query(&q);
        for row in chunk {
            query = query
                .bind(project_id)
                .bind(&row.file)
                .bind(&row.category)
                .bind(&row.severity)
                .bind(&row.title)
                .bind(&row.detail)
                .bind(row.line_number)
                .bind(&row.confidence)
                .bind(&row.context_snippet)
                .bind(&row.related_files)
                .bind(row.is_auto_suppressed);
        }
        let _ = query.execute(db).await;
    }
}

/// Riapplica i flag false-positive salvati prima del fresh start, riconciliando
/// per (file_path, line_number, category, title). Best-effort come l'originale.
async fn reapply_false_positives(
    db: &sqlx::PgPool,
    project_id: Uuid,
    fp_rows: &[sqlx::postgres::PgRow],
) {
    for fp_row in fp_rows {
        let fp_file: String = fp_row.try_get("file_path").unwrap_or_default();
        let fp_line: Option<i32> = fp_row.try_get("line_number").ok().flatten();
        let fp_cat: String = fp_row.try_get("category").unwrap_or_default();
        let fp_title: String = fp_row.try_get("title").unwrap_or_default();
        let fp_reason: Option<String> = fp_row.try_get("false_positive_reason").ok().flatten();

        let _ = sqlx::query(
            "UPDATE project_quality_findings \
             SET is_false_positive = TRUE, false_positive_reason = $1, false_positive_at = NOW() \
             WHERE project_id = $2 AND file_path = $3 \
               AND (line_number = $4 OR ($4 IS NULL AND line_number IS NULL)) \
               AND category = $5 AND title = $6",
        )
        .bind(&fp_reason)
        .bind(project_id)
        .bind(&fp_file)
        .bind(fp_line)
        .bind(&fp_cat)
        .bind(&fp_title)
        .execute(db)
        .await;
    }
}

/// Hook detector DB: rileva il profilo database del progetto e lo persiste in
/// `project_database_config` (upsert conservativo). Best-effort.
async fn upsert_db_profile(db: &sqlx::PgPool, project_id: Uuid, root_path: &str) {
    let project_root_path = std::path::PathBuf::from(root_path);
    let db_profile = crate::project_db::detector::detect_db_profile(&project_root_path);
    if db_profile.migration_tool.is_none() && db_profile.marker_files.is_empty() {
        return;
    }
    let engine_str = db_profile.engine.as_str().to_string();
    let tool_str = db_profile
        .migration_tool
        .as_ref()
        .map(|t| t.as_str().to_string());
    let mig_path = db_profile.migration_path.clone();
    let metadata = serde_json::to_value(&db_profile).unwrap_or(serde_json::json!({}));
    let _ = sqlx::query(
        r#"INSERT INTO project_database_config
           (project_id, engine, hosting_mode, migration_tool, migration_path, detection_metadata)
           VALUES ($1, $2, 'external', $3, $4, $5)
           ON CONFLICT (project_id) DO UPDATE
           SET engine = EXCLUDED.engine,
               migration_tool = COALESCE(project_database_config.migration_tool, EXCLUDED.migration_tool),
               migration_path = COALESCE(project_database_config.migration_path, EXCLUDED.migration_path),
               detection_metadata = EXCLUDED.detection_metadata,
               updated_at = NOW()
           WHERE project_database_config.migration_tool IS NULL OR
                 project_database_config.detection_metadata = '{}'::jsonb"#,
    )
    .bind(project_id)
    .bind(&engine_str)
    .bind(&tool_str)
    .bind(&mig_path)
    .bind(&metadata)
    .execute(db)
    .await
    .ok();
    tracing::info!(
        project_id = %project_id,
        engine = %engine_str,
        tool = ?tool_str,
        confidence = db_profile.confidence,
        "DB detector: profilo rilevato per progetto"
    );
}

/// GET /api/projects/:id/quality-scan/:scan_id - polling stato scan
pub async fn get_quality_scan_status(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath((project_id, scan_id)): AxumPath<(Uuid, i64)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let row = sqlx::query(
        "SELECT status, files_scanned, total_findings, by_severity, by_category, \
                error_message, duration_ms, started_at, completed_at \
         FROM nexus_quality_scans \
         WHERE id = $1 AND project_id = $2",
    )
    .bind(scan_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "scan not found".to_string()))?;

    let status: String = row.try_get("status").unwrap_or_default();
    let files_scanned: Option<i32> = row.try_get("files_scanned").ok().flatten();
    let total_findings: Option<i32> = row.try_get("total_findings").ok().flatten();
    let by_severity: Option<serde_json::Value> = row.try_get("by_severity").ok().flatten();
    let by_category: Option<serde_json::Value> = row.try_get("by_category").ok().flatten();
    let error_message: Option<String> = row.try_get("error_message").ok().flatten();
    let duration_ms: Option<i32> = row.try_get("duration_ms").ok().flatten();
    let started_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("started_at").ok().flatten();
    let completed_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("completed_at").ok().flatten();

    Ok(Json(json!({
        "scan_id": scan_id,
        "projectId": project_id.to_string(),
        "status": status,
        "filesScanned": files_scanned,
        "totalFindings": total_findings,
        "bySeverity": by_severity,
        "byCategory": by_category,
        "errorMessage": error_message,
        "durationMs": duration_ms,
        "startedAt": started_at.map(|d| d.to_rfc3339()),
        "completedAt": completed_at.map(|d| d.to_rfc3339()),
    })))
}

/// Pruning lazy dei findings su file non piu' esistenti (regola H). L'auto-scan
/// per-file (maybe_auto_scan_file) aggiorna solo i file MODIFICATI e non rimuove
/// i findings di file SPOSTATI/CANCELLATI (es. ristrutturazione src/pages ->
/// src/app/pages): senza una scansione completa restano "stale" nel pannello come
/// falsi positivi. Li rimuoviamo alla lettura. Guardia: pruna SOLO se la
/// repository_root_path del progetto e' una dir accessibile, per non cancellare
/// tutto se il filesystem e' temporaneamente irraggiungibile (host diverso/smontato).
async fn prune_stale_findings(db: &sqlx::PgPool, project_id: Uuid) {
    let Some(root) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT repository_root_path FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten() else {
        return;
    };
    if !std::path::Path::new(&root).is_dir() {
        return;
    }
    let Ok(rows) = sqlx::query(
        "SELECT id, file_path FROM project_quality_findings \
         WHERE project_id = $1 AND fixed_at IS NULL",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    else {
        return;
    };
    let stale_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| {
            let fp: String = r.try_get("file_path").ok()?;
            if !fp.is_empty() && !std::path::Path::new(&fp).exists() {
                r.try_get::<Uuid, _>("id").ok()
            } else {
                None
            }
        })
        .collect();
    if stale_ids.is_empty() {
        return;
    }
    let _ = sqlx::query("DELETE FROM project_quality_findings WHERE id = ANY($1)")
        .bind(&stale_ids)
        .execute(db)
        .await;
    tracing::info!(
        project_id = %project_id,
        pruned = stale_ids.len(),
        "quality-findings: rimossi findings stale su file non piu' esistenti"
    );
}

/// Determina il filtro severity effettivo: se il chiamante lo specifica lo usa
/// tale e quale; altrimenti applica `quality_severity_threshold` dal DB come
/// default (fallback "low"). "all" esplicito bypassa il threshold.
async fn resolve_severity_filter(db: &sqlx::PgPool, requested: Option<&String>) -> String {
    match requested {
        Some(s) => s.clone(),
        None => sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'quality_severity_threshold'",
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "low".to_string()),
    }
}

/// Esegue la query dei finding applicando il filtro severity+category, con
/// l'ordinamento specifico per ciascun caso. Estratto da `get_quality_findings`.
/// Delega ai due rami (solo-severity / per-category) per contenerne la lunghezza.
/// Query letterali (non interpolate) per non introdurre falsi positivi
/// SQL-injection: la variabilita' e' solo nei bind parametrizzati.
async fn query_findings_filtered(
    db: &sqlx::PgPool,
    project_id: Uuid,
    severity: &str,
    category: &str,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    if category == "all" {
        query_findings_by_severity(db, project_id, severity, limit).await
    } else {
        query_findings_by_category(db, project_id, category, limit).await
    }
}

/// Caso `severity == "all" && category == "all"`: tutti i finding non
/// falsi-positivi, ordinati per severity poi file_path.
async fn query_findings_all_severities(
    db: &sqlx::PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
         FROM project_quality_findings \
         WHERE project_id = $1 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
         ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END, file_path \
         LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Rami con `category == "all"`: filtra per la sola severity (all/high/medium o
/// una severity puntuale). Ordinamento coerente con l'originale.
async fn query_findings_by_severity(
    db: &sqlx::PgPool,
    project_id: Uuid,
    severity: &str,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    match severity {
        "all" => query_findings_all_severities(db, project_id, limit).await,
        "high" => sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity = 'high' AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY file_path LIMIT $2",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(db)
        .await,
        "medium" => sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity IN ('high','medium') AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY CASE severity WHEN 'high' THEN 1 ELSE 2 END, file_path LIMIT $2",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(db)
        .await,
        _ => sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity = $2 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY file_path LIMIT $3",
        )
        .bind(project_id)
        .bind(severity)
        .bind(limit)
        .fetch_all(db)
        .await,
    }
}

/// Ramo con `category != "all"`: filtra per categoria, ordinando per severity.
async fn query_findings_by_category(
    db: &sqlx::PgPool,
    project_id: Uuid,
    category: &str,
    limit: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
         FROM project_quality_findings \
         WHERE project_id = $1 AND category = $2 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
         ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END LIMIT $3",
    )
    .bind(project_id)
    .bind(category)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// GET /api/projects/:id/quality-findings
pub async fn get_quality_findings(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    prune_stale_findings(&state.db, project_id).await;

    let category = params
        .get("category")
        .cloned()
        .unwrap_or_else(|| "all".to_string());
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let severity: String = resolve_severity_filter(&state.db, params.get("severity")).await;

    let rows = query_findings_filtered(&state.db, project_id, &severity, &category, limit)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let findings: Vec<Value> = rows.iter().map(finding_row_to_json).collect();

    Ok(Json(
        json!({ "findings": findings, "total": findings.len() }),
    ))
}

/// Serializza una riga di `project_quality_findings` nel JSON camelCase esposto
/// dall'API. Estratto da `get_quality_findings` per contenerne la lunghezza.
fn finding_row_to_json(r: &sqlx::postgres::PgRow) -> Value {
    let id: Uuid = r.try_get("id").unwrap_or_default();
    let fixed_at: Option<chrono::DateTime<chrono::Utc>> = r.try_get("fixed_at").ok().flatten();
    let confidence: Option<String> = r.try_get("confidence").ok().flatten();
    let context_snippet: Option<String> = r.try_get("context_snippet").ok().flatten();
    let related_files: Option<Vec<String>> = r.try_get("related_files").ok().flatten();
    json!({
        "id": id.to_string(),
        "filePath": r.try_get::<String, _>("file_path").unwrap_or_default(),
        "category": r.try_get::<String, _>("category").unwrap_or_default(),
        "severity": r.try_get::<String, _>("severity").unwrap_or_default(),
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "detail": r.try_get::<String, _>("detail").unwrap_or_default(),
        "lineNumber": r.try_get::<Option<i32>, _>("line_number").unwrap_or(None),
        "fixedAt": fixed_at.map(|d| d.to_rfc3339()),
        "confidence": confidence,
        "contextSnippet": context_snippet,
        "relatedFiles": related_files,
    })
}

/// POST /api/projects/:id/quality-findings/:finding_id/mark-fixed
pub async fn mark_finding_fixed(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath((project_id, finding_id)): AxumPath<(Uuid, Uuid)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    sqlx::query(
        "UPDATE project_quality_findings SET fixed_at = NOW() WHERE id = $1 AND project_id = $2",
    )
    .bind(finding_id)
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

/// Serializza un finding di `scan_single_file` nel JSON camelCase esposto,
/// col `lineNumber` gia' calcolato (Null per i finding SQL, i32 per gli altri).
/// Accetta i campi come `&str` per servire sia `mcp_db::DbFinding` sia
/// `mcp_quality::QualityFinding`.
fn single_file_finding_json(
    category: &str,
    severity: &str,
    title: &str,
    detail: &str,
    file_path: &str,
    line_number: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "category": category,
        "severity": severity,
        "title": title,
        "detail": detail,
        "lineNumber": line_number,
        "filePath": file_path,
    })
}

/// POST /api/projects/:id/quality-scan-file
/// Analizza un singolo file e restituisce i finding senza toccare il DB.
pub async fn scan_single_file(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let file_path: String = body["file_path"]
        .as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "file_path required".to_string()))?
        .to_string();

    if file_path.contains("..") || file_path.starts_with('/') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid file_path".to_string(),
        ));
    }

    let root_path = resolve_project_root_path(&state.db, project_id).await?;

    let abs_path = format!(
        "{}/{}",
        root_path.trim_end_matches('/'),
        file_path.trim_start_matches('/')
    );
    let content = std::fs::read_to_string(&abs_path)
        .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("file not found: {e}")))?;

    let findings = analyze_single_file_findings(&file_path, &content);

    Ok(Json(serde_json::json!({ "findings": findings })))
}

/// Analizza un singolo file e ne serializza i finding in JSON camelCase.
/// SQL via `mcp_db` (lineNumber Null), resto via `mcp_quality`.
fn analyze_single_file_findings(file_path: &str, content: &str) -> Vec<serde_json::Value> {
    if file_path.ends_with(".sql") {
        mcp_db::analyze_query(content)
            .findings
            .iter()
            .map(|f| {
                single_file_finding_json(
                    &f.category,
                    &f.severity,
                    &f.title,
                    &f.detail,
                    file_path,
                    serde_json::Value::Null,
                )
            })
            .collect()
    } else {
        mcp_quality::analyze_source(file_path, content)
            .findings
            .iter()
            .map(|f| {
                single_file_finding_json(
                    &f.category,
                    &f.severity,
                    &f.title,
                    &f.detail,
                    file_path,
                    json!(f.line.map(|l| l as i32)),
                )
            })
            .collect()
    }
}

/// Come `resolve_project_root_path` ma include il fallback `analysis_json->>'rootPath'`
/// e stringa vuota finale, usato da `get_file_lines`. 404 se il progetto non esiste.
async fn resolve_project_root_with_analysis(
    db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<String, (StatusCode, Json<Value>)> {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT COALESCE(r.root_path, w.absolute_path, p.analysis_json->>'rootPath', '') \
         FROM projects p \
         LEFT JOIN repositories r ON r.project_id = p.id \
         LEFT JOIN workspaces w ON w.project_id = p.id \
         WHERE p.id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;
    Ok(row.0)
}

/// GET /api/projects/:id/file-lines?path=...&start=N&end=M
/// Legge un intervallo di righe di un file del progetto.
pub async fn get_file_lines(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let file_path = params.get("path").cloned().unwrap_or_default();
    let start_line: usize = params
        .get("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let end_line: usize = params
        .get("end")
        .and_then(|s| s.parse().ok())
        .unwrap_or(start_line + 80);

    if file_path.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "path required".to_string(),
        ));
    }

    let root_path = resolve_project_root_with_analysis(&state.db, project_id).await?;

    let abs_path = if std::path::Path::new(&file_path).is_absolute() {
        file_path.clone()
    } else {
        format!(
            "{}/{}",
            root_path.trim_end_matches('/'),
            file_path.trim_start_matches('/')
        )
    };

    let content = tokio::fs::read_to_string(&abs_path)
        .await
        .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("Cannot read file: {e}")))?;

    Ok(Json(slice_lines_json(&content, start_line, end_line)))
}

/// Estrae l'intervallo di righe `[start_line, end_line]` (1-based, clampato ai
/// limiti del file) e lo serializza nel JSON di risposta di `get_file_lines`.
fn slice_lines_json(content: &str, start_line: usize, end_line: usize) -> Value {
    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();
    let start_idx = start_line.saturating_sub(1).min(total_lines);
    let end_idx = end_line.min(total_lines);

    let lines = all_lines[start_idx..end_idx].join("\n");

    json!({
        "lines": lines,
        "startLine": start_idx + 1,
        "endLine": end_idx,
        "totalLines": total_lines,
    })
}

// ── Arricchimento vettoriale dei finding ─────────────────────────────────────

/// Restituisce gli indici dei finding da arricchire (solo high/medium),
/// ordinati per priorita': HIGH prima, poi MEDIUM.
fn prioritized_finding_indices(batch: &[FindingRow]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..batch.len())
        .filter(|i| batch[*i].severity == "high" || batch[*i].severity == "medium")
        .collect();
    indices.sort_by_key(|i| if batch[*i].severity == "high" { 0 } else { 1 });
    indices
}

/// Arricchisce i finding con dati dal code index vettoriale:
/// - confidence: "high"/"medium"/"low" basata sull'analisi del contesto vettoriale
/// - related_files: file semanticamente simili trovati via Qdrant
/// - is_auto_suppressed: true se il finding e' probabilmente un falso positivo
///
/// Ritorna true se l'arricchimento e' riuscito (code index disponibile).
async fn enrich_findings_with_vectors(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    batch: &mut [FindingRow],
    file_contents: &std::collections::HashMap<String, String>,
) -> bool {
    // Verifica che l'embedder sia raggiungibile (timeout 10s per evitare blocchi)
    if !probe_embedder(orchestrator).await {
        return false;
    }

    let mut enriched_count = 0u32;

    for idx in prioritized_finding_indices(batch) {
        let row = &mut batch[idx];

        // Genera context_snippet se non gia' presente
        if row.context_snippet.is_none() {
            if let (Some(ln), Some(content)) = (row.line_number, file_contents.get(&row.file)) {
                row.context_snippet = Some(mcp_quality::extract_context_snippet(
                    content,
                    ln as usize,
                    5,
                ));
            }
        }

        // Per finding "reliability" (N+1), distingue HTTP (falso positivo) da DB.
        // Se auto-soppresso, salta la ricerca correlati.
        if classify_n_plus_one(row) {
            continue;
        }

        // Incrementa il contatore solo quando la ricerca code-index e' riuscita
        // (semantica identica all'originale: contava solo il ramo Ok(hits)).
        if enrich_related_files(db, orchestrator, project_id, row).await {
            enriched_count += 1;
        }

        // Cap: max 50 embedding per scan per non rallentare troppo
        if enriched_count >= 50 {
            break;
        }
    }

    tracing::info!(
        "quality_scan vector: arricchiti {} finding con dati vettoriali",
        enriched_count
    );
    true
}

/// Pattern HTTP (non-DB) per sopprimere falsi positivi N+1.
const HTTP_PATTERNS: &[&str] = &[
    "fetch(",
    "axios.",
    "http.get",
    "http.post",
    "http.put",
    "http.delete",
    "HttpClient",
    "urllib",
    "requests.",
    "got(",
    "ky(",
    "ofetch(",
    "useFetch",
    "$fetch",
    "superagent",
];

/// Pattern DB reali (query verso database).
const DB_PATTERNS: &[&str] = &[
    "prisma.",
    ".query(",
    "knex(",
    "db.",
    "sequelize.",
    "typeorm",
    "mongoose.",
    "pool.query",
    "connection.query",
    "SqlCommand",
    "ExecuteReader",
    "execute(",
    ".findOne(",
    ".findAll(",
    "repository.",
    "getRepository",
];

/// Verifica che l'embedder risponda (timeout 10s). Ritorna false loggando la
/// causa quando non raggiungibile o in timeout.
async fn probe_embedder(orchestrator: &crate::orchestrator::Orchestrator) -> bool {
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        orchestrator.embed_text("test"),
    )
    .await
    {
        Ok(Ok(_)) => true,
        Ok(Err(e)) => {
            tracing::warn!(
                "quality_scan vector: embedder non raggiungibile: {}, skip arricchimento",
                e
            );
            false
        }
        Err(_) => {
            tracing::warn!("quality_scan vector: timeout 10s test embedder, skip arricchimento");
            false
        }
    }
}

/// Classifica un finding N+1 in base al contesto: HTTP puro => falso positivo
/// auto-soppresso (ritorna true, da saltare); DB => confidence high; altrimenti
/// medium. Ritorna true solo quando il finding e' stato auto-soppresso.
fn classify_n_plus_one(row: &mut FindingRow) -> bool {
    if !(row.category == "reliability" && row.title.contains("N+1")) {
        return false;
    }
    let Some(snippet) = row.context_snippet.as_ref() else {
        return false;
    };
    let snippet_lower = snippet.to_lowercase();
    let has_http = HTTP_PATTERNS
        .iter()
        .any(|p| snippet_lower.contains(&p.to_lowercase()));
    let has_db = DB_PATTERNS
        .iter()
        .any(|p| snippet_lower.contains(&p.to_lowercase()));

    if has_http && !has_db {
        // Chiamata HTTP confusa per query DB: falso positivo
        row.confidence = Some("low".into());
        row.is_auto_suppressed = true;
        tracing::info!(
            "quality_scan vector: auto-soppresso falso positivo N+1 in {}:{}",
            row.file,
            row.line_number.unwrap_or(0)
        );
        return true;
    } else if has_db {
        row.confidence = Some("high".into());
    } else {
        row.confidence = Some("medium".into());
    }
    false
}

/// Cerca file semanticamente correlati al finding via code index e ne popola
/// `related_files` + `confidence` (se non gia' impostata). Best-effort: log su
/// timeout/errore, nessuna propagazione. Ritorna true solo se la ricerca sul
/// code index e' andata a buon fine (usato per il cap di 50 embedding/scan).
async fn enrich_related_files(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    row: &mut FindingRow,
) -> bool {
    let Some(hits) = embed_and_search_finding(db, orchestrator, project_id, row).await else {
        return false;
    };
    apply_related_and_confidence(row, &hits);
    true
}

/// Costruisce il testo di ricerca dal finding (max 500 char), ne calcola
/// l'embedding (timeout 10s) e interroga il code index. Ritorna gli hit o None
/// su errore/timeout di embedding o ricerca (loggati).
async fn embed_and_search_finding(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    row: &FindingRow,
) -> Option<Vec<vector_memory::VectorPointHit>> {
    // Embedding del contesto del finding per ricerca semantica
    let search_text = format!(
        "{}: {} — {}",
        row.file,
        row.title,
        row.context_snippet.as_deref().unwrap_or(&row.detail)
    );
    // Limita la lunghezza del testo per l'embedding (max 500 char)
    let search_text = if search_text.len() > 500 {
        search_text[..500].to_string()
    } else {
        search_text
    };

    // Timeout 10s per singola embed: evita blocchi se embedder/Qdrant rallentano
    let vector = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        orchestrator.embed_text(&search_text),
    )
    .await
    {
        Ok(Ok(vector)) => vector,
        Ok(Err(e)) => {
            tracing::debug!("quality_scan vector: embedding fallito: {}", e);
            return None;
        }
        Err(_) => {
            tracing::warn!("quality_scan vector: timeout 10s embedding, skip finding");
            return None;
        }
    };

    match vector_memory::search_code_index(db, &vector, project_id, 5).await {
        Ok(hits) => Some(hits),
        Err(e) => {
            tracing::debug!("quality_scan vector: ricerca code index fallita: {}", e);
            None
        }
    }
}

/// Applica al finding i file correlati (dedup, esclude se stesso, max 3) e la
/// confidence derivata dallo score migliore (se non gia' impostata).
fn apply_related_and_confidence(row: &mut FindingRow, hits: &[vector_memory::VectorPointHit]) {
    let related: Vec<String> = hits
        .iter()
        .filter_map(|h| {
            h.payload
                .get("file_path")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .filter(|f| f != &row.file) // escludi il file stesso
        .collect::<std::collections::HashSet<_>>() // dedup
        .into_iter()
        .take(3)
        .collect();
    if !related.is_empty() {
        row.related_files = Some(related);
    }

    // Se il finding non ha ancora confidence, assegnala in base ai match
    if row.confidence.is_none() {
        let best_score = hits.first().map(|h| h.score).unwrap_or(0.0);
        row.confidence = Some(if best_score > 0.85 {
            "high".into()
        } else if best_score > 0.6 {
            "medium".into()
        } else {
            "low".into()
        });
    }
}

/// Cerca duplicati semantici: funzioni in file diversi con corpo molto simile.
/// Usa l'embedder per vettorializzare i corpi di funzione e il code index
/// per trovare match ad alta similarita' (score > 0.85).
async fn detect_semantic_duplicates(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    file_contents: &std::collections::HashMap<String, String>,
) -> Vec<FindingRow> {
    let mut findings = Vec::new();
    // Traccia coppie gia' segnalate per evitare duplicati (A,B) e (B,A)
    let mut seen_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut total_embedded = 0u32;

    for (rel_path, content) in file_contents {
        // Solo file di codice (non SQL)
        if rel_path.ends_with(".sql") {
            continue;
        }

        let bodies = mcp_quality::extract_function_bodies(content, 8);
        for body in &bodies {
            // Cap globale: max 80 embedding per la fase duplicati
            if total_embedded >= 80 {
                break;
            }

            let Some(hits) = embed_and_search_body(db, orchestrator, project_id, body).await else {
                continue;
            };
            total_embedded += 1;

            for hit in &hits {
                if let Some(finding) =
                    dup_finding_from_hit(hit, rel_path, content, body, &mut seen_pairs)
                {
                    findings.push(finding);
                }
            }
        }
        if total_embedded >= 80 {
            break;
        }
    }

    tracing::info!(
        "quality_scan semantic_dup: {} duplicati trovati (embedded {} funzioni)",
        findings.len(),
        total_embedded
    );
    findings
}

/// Embedding del corpo di funzione (prime 800 char) + ricerca sul code index.
/// Ritorna gli hit se entrambe le fasi riescono entro i timeout, altrimenti
/// None (l'originale faceva `continue` senza incrementare il contatore).
async fn embed_and_search_body(
    db: &sqlx::PgPool,
    orchestrator: &crate::orchestrator::Orchestrator,
    project_id: Uuid,
    body: &mcp_quality::FunctionBody,
) -> Option<Vec<vector_memory::VectorPointHit>> {
    // Prendi le prime 800 char del corpo come testo per l'embedding
    let text = if body.body.len() > 800 {
        body.body[..800].to_string()
    } else {
        body.body.clone()
    };

    // Timeout 10s per singola embed: evita blocchi se embedder/Qdrant rallentano
    let vector = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        orchestrator.embed_text(&text),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(_)) | Err(_) => return None,
    };

    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        vector_memory::search_code_index(db, &vector, project_id, 5),
    )
    .await
    {
        Ok(Ok(h)) => Some(h),
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Valuta un singolo hit del code index: se supera la soglia di similarita',
/// non e' nello stesso file e la coppia non e' gia' vista, costruisce il
/// `FindingRow` di duplicato semantico. Aggiorna `seen_pairs` per il dedup.
fn dup_finding_from_hit(
    hit: &vector_memory::VectorPointHit,
    rel_path: &str,
    content: &str,
    body: &mcp_quality::FunctionBody,
    seen_pairs: &mut std::collections::HashSet<(String, String)>,
) -> Option<FindingRow> {
    if hit.score < 0.85 {
        return None;
    }

    let hit_file = hit
        .payload
        .get("file_path")
        .and_then(|v| v.as_str())?
        .to_string();

    // Ignora match nello stesso file
    if hit_file == *rel_path {
        return None;
    }

    // Dedup coppie
    let pair_key = if rel_path < hit_file.as_str() {
        (rel_path.to_string(), hit_file.clone())
    } else {
        (hit_file.clone(), rel_path.to_string())
    };
    if seen_pairs.contains(&pair_key) {
        return None;
    }
    seen_pairs.insert(pair_key);

    Some(make_dup_finding(hit, rel_path, content, body, hit_file))
}

/// Costruisce il `FindingRow` di duplicato semantico a partire da hit + corpo
/// funzione. Estratto da `dup_finding_from_hit` per contenerne la lunghezza.
fn make_dup_finding(
    hit: &vector_memory::VectorPointHit,
    rel_path: &str,
    content: &str,
    body: &mcp_quality::FunctionBody,
    hit_file: String,
) -> FindingRow {
    let hit_chunk = hit
        .payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("(contenuto non disponibile)");
    // Prendi solo le prime 3 righe del chunk come preview
    let preview: String = hit_chunk.lines().take(3).collect::<Vec<_>>().join(" | ");

    FindingRow {
        file: rel_path.to_string(),
        category: "semantic_duplication".into(),
        severity: "medium".into(),
        title: format!(
            "Duplicato semantico: `{}` simile a codice in `{}`",
            body.name, hit_file
        ),
        detail: format!(
            "La funzione `{}` (righe {}-{}) ha similarita' {:.0}% con un blocco in `{}`. \
             Preview: {}. Valutare estrazione in modulo condiviso.",
            body.name,
            body.start_line,
            body.end_line,
            hit.score * 100.0,
            hit_file,
            preview
        ),
        line_number: Some(body.start_line as i32),
        confidence: Some("medium".into()),
        context_snippet: Some(mcp_quality::extract_context_snippet(
            content,
            body.start_line,
            3,
        )),
        related_files: Some(vec![hit_file]),
        is_auto_suppressed: false,
    }
}

// ── Auto-scan singolo file su write/edit ─────────────────────────────────────

/// Legge il valore di un setting dalla tabella `settings`. Ritorna None se il
/// DB non risponde o la chiave non esiste. Punto unico per le letture settings
/// dell'auto-scan (evita di duplicare la query in piu' punti).
async fn read_setting(db: &sqlx::PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// Chiamata in fire-and-forget dopo ogni write_file / edit_file.
/// Legge `quality_auto_scan` dal DB: se `"true"` esegue lo scan
/// solo sul file appena modificato (non sull'intero progetto).
/// Applica le stesse regole regex di `perform_quality_scan`, ma
/// limita la scansione al singolo path per non appesantire ogni salvataggio.
pub async fn maybe_auto_scan_file(
    db: &sqlx::PgPool,
    project_id: Uuid,
    project_root: &std::path::Path,
    file_path: &std::path::Path,
) {
    // Controlla il setting quality_auto_scan
    let enabled = read_setting(db, "quality_auto_scan")
        .await
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !enabled {
        return;
    }

    // Legge threshold per filtrare i finding da inserire
    let threshold = read_setting(db, "quality_severity_threshold")
        .await
        .unwrap_or_else(|| "low".to_string());
    let threshold = threshold.trim().to_lowercase();

    let path_str = match file_path.to_str() {
        Some(s) => s,
        None => return,
    };

    // Legge il contenuto del file
    let content = match tokio::fs::read_to_string(file_path).await {
        Ok(c) => c,
        Err(_) => return,
    };

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let allowed_ext = ["rs", "ts", "tsx", "js", "jsx", "py", "sql", "cs", "go"];
    if !allowed_ext.contains(&ext) {
        return;
    }

    // Persistenza col path RELATIVO alla root (stesso formato del full-scan,
    // punto unico relative_path_str): il path assoluto canonicalizzato dei tool
    // (verbatim `\\?\D:\...` su Windows) rendeva i finding illeggibili in UI e
    // il click dal pannello Problemi non risolveva mai il file (404).
    let rel_str = relative_path_str(path_str, &project_root.to_string_lossy());

    // Esegue i checker sul file singolo via mcp_quality::analyze_source (crate esterna)
    let report = mcp_quality::analyze_source(&rel_str, &content);

    // Severità minima: low=0, medium=1, high=2
    let min_level: u8 = match threshold.as_str() {
        "high" => 2,
        "medium" => 1,
        _ => 0, // "low" o qualsiasi altro valore
    };

    rescan_and_persist_file(db, project_id, &rel_str, &report, min_level, &threshold).await;
}

/// Sostituisce (DELETE + INSERT filtrato) i finding del singolo file e, se
/// qualcosa e' cambiato, emette `FindingsUpdated`. Estratto da
/// `maybe_auto_scan_file` per contenerne la lunghezza.
async fn rescan_and_persist_file(
    db: &sqlx::PgPool,
    project_id: Uuid,
    path_str: &str,
    report: &mcp_quality::QualityReport,
    min_level: u8,
    threshold: &str,
) {
    // Elimina SEMPRE i finding precedenti per questo file e progetto, PRIMA di
    // qualsiasi early-return: quando il file e' stato CORRETTO (0 finding nuovi)
    // i finding vecchi devono comunque sparire dal pannello. Il DELETE qui tocca
    // SOLO i finding di QUESTO file (project_id + file_path), quindi non puo'
    // toccare violazioni risorse/playwright (che vivono su altre tabelle).
    // RETURNING id: serve a popolare resolved_ids nell'evento FindingsUpdated
    // cosi' il frontend marca/rimuove i finding risolti senza ri-scansionare.
    let deleted_ids: Vec<Uuid> = sqlx::query_scalar(
        "DELETE FROM project_quality_findings \
         WHERE project_id = $1 AND file_path = $2 \
         RETURNING id",
    )
    .bind(project_id)
    .bind(path_str)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Inserisce i nuovi finding filtrati per soglia
    let inserted =
        insert_filtered_findings(db, project_id, path_str, &report.findings, min_level).await;

    tracing::debug!(
        "auto_scan_file: project={} file={} findings_raw={} deleted={} inserted={} threshold={}",
        project_id,
        path_str,
        report.findings.len(),
        deleted_ids.len(),
        inserted,
        threshold
    );

    // Notifica realtime: ribilancia badge + pannelli (Problemi via get_project_problems,
    // Ottimizzazione via get_quality_findings). Emette anche quando inserted==0
    // (file pulito): e' proprio il caso "problema risolto" che prima restava nel
    // pannello. `resolved_ids` = finding cancellati di QUESTO file; il frontend li
    // marca in-place. Solo se qualcosa e' cambiato (cancellato o inserito), per
    // non generare rumore di eventi a ogni salvataggio innocuo.
    if !deleted_ids.is_empty() || inserted > 0 {
        emit_findings_updated_for_project(db, project_id, deleted_ids).await;
    }
}

/// Inserisce i finding di un singolo file filtrati per severity minima
/// (`min_level`: low=0, medium=1, high=2). Ritorna il numero di finding inseriti.
/// Best-effort: gli errori di insert sono ignorati come nell'originale.
async fn insert_filtered_findings(
    db: &sqlx::PgPool,
    project_id: Uuid,
    path_str: &str,
    findings: &[mcp_quality::QualityFinding],
    min_level: u8,
) -> u32 {
    let mut inserted = 0u32;
    for f in findings {
        let level: u8 = match f.severity.as_str() {
            "high" => 2,
            "medium" => 1,
            _ => 0,
        };
        if level < min_level {
            continue;
        }
        let line_number: Option<i32> = f.line.map(|l| l as i32);
        let _ = sqlx::query(
            "INSERT INTO project_quality_findings \
             (project_id, file_path, category, severity, title, detail, line_number, scanned_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
        )
        .bind(project_id)
        .bind(path_str)
        .bind(&f.category)
        .bind(&f.severity)
        .bind(&f.title)
        .bind(&f.detail)
        .bind(line_number)
        .execute(db)
        .await;
        inserted += 1;
    }
    inserted
}

/// Emette `FindingsUpdated` ricalcolando i totali correnti del progetto dalla
/// tabella `project_quality_findings` (fonte unica QUALITY). `resolved_ids` =
/// finding rimossi in questo aggiornamento, usati dal frontend per marcare
/// in-place. Punto unico per emettere l'evento dall'auto-scan per-file: usa il
/// registry globale (`emit_global`) cosi' non serve propagare `&ProjectChannels`
/// nei call site fire-and-forget in `files.rs`.
async fn emit_findings_updated_for_project(
    db: &sqlx::PgPool,
    project_id: Uuid,
    resolved_ids: Vec<Uuid>,
) {
    // Totali correnti (esclusi i falsi positivi, coerente con get_quality_findings).
    let counts: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT \
            COUNT(*) AS total, \
            COUNT(*) FILTER (WHERE severity = 'high') AS critical, \
            COUNT(*) FILTER (WHERE severity = 'medium') AS warnings \
         FROM project_quality_findings \
         WHERE project_id = $1 AND (is_false_positive = FALSE OR is_false_positive IS NULL)",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let (total, critical, warnings) = counts.unwrap_or((0, 0, 0));
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::ProjectEvent::FindingsUpdated {
            scan_id: None,
            total,
            critical,
            warnings,
            resolved_ids,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    /// Crea le tabelle minimali toccate da `maybe_auto_scan_file` su un DB di test.
    async fn setup_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )
        .execute(pool)
        .await
        .expect("create settings");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project_quality_findings (\
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                project_id UUID NOT NULL, \
                scanned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                file_path TEXT NOT NULL, \
                category TEXT NOT NULL, \
                severity TEXT NOT NULL, \
                title TEXT NOT NULL, \
                detail TEXT NOT NULL, \
                line_number INTEGER, \
                fixed_at TIMESTAMPTZ, \
                is_false_positive BOOLEAN NOT NULL DEFAULT FALSE)",
        )
        .execute(pool)
        .await
        .expect("create project_quality_findings");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('quality_auto_scan', 'true')")
            .execute(pool)
            .await
            .expect("seed quality_auto_scan");
    }

    /// Regressione del bug "i problemi risolti non vengono eliminati": un file
    /// CORRETTO (senza piu' finding) deve far sparire i finding precedenti.
    /// Prima del fix, `maybe_auto_scan_file` faceva `return` su `findings.is_empty()`
    /// PRIMA del DELETE, lasciando il finding stantio nel pannello.
    #[sqlx::test]
    async fn auto_scan_file_pulito_cancella_finding_precedenti(pool: PgPool) {
        setup_schema(&pool).await;
        let project_id = Uuid::new_v4();

        // File su disco SENZA finding (codice Rust banale).
        // NB: falso positivo del detector SQL-injection (ADR 0021): il metodo
        // Rust PathBuf::join matcha \bJOIN\b + format! con {} matcha il pattern.
        // Non e' SQL: e' costruzione di un path per una dir temporanea di test.
        let dir = std::env::temp_dir().join(format!("nexus_qtest_{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.expect("mkdir tmp");
        let file_path = dir.join("clean.rs");
        // Contenuto senza finding di analyze_source: funzione PRIVATA documentata
        // (niente "public function without documentation", niente smell di base).
        tokio::fs::write(
            &file_path,
            "/// Somma due interi.\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .await
        .expect("write file");
        // I finding sono persistiti col path RELATIVO alla root (qui `dir`).
        let rel_str = "clean.rs";

        // Finding stantio gia' presente per quel file (simula problema poi corretto).
        sqlx::query(
            "INSERT INTO project_quality_findings \
             (project_id, file_path, category, severity, title, detail) \
             VALUES ($1, $2, 'maintainability', 'medium', 'vecchio', 'da rimuovere')",
        )
        .bind(project_id)
        .bind(rel_str)
        .execute(&pool)
        .await
        .expect("seed finding");

        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_quality_findings WHERE project_id = $1 AND file_path = $2",
        )
        .bind(project_id)
        .bind(rel_str)
        .fetch_one(&pool)
        .await
        .expect("count before");
        assert_eq!(
            before, 1,
            "il finding stantio deve esistere prima dello scan"
        );

        maybe_auto_scan_file(&pool, project_id, &dir, &file_path).await;

        let after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_quality_findings WHERE project_id = $1 AND file_path = $2",
        )
        .bind(project_id)
        .bind(rel_str)
        .fetch_one(&pool)
        .await
        .expect("count after");
        assert_eq!(
            after, 0,
            "su file pulito i finding precedenti devono essere cancellati"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
