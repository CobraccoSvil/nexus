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
    let row = sqlx::query(
        r#"SELECT COALESCE(r.root_path, w.absolute_path) AS root_path
           FROM projects p
           LEFT JOIN repositories r ON r.project_id = p.id
           LEFT JOIN workspaces w ON w.project_id = p.id
           WHERE p.id = $1
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "project not found".to_string()))?;

    let root_path: String = row
        .try_get("root_path")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Insert riga 'running' e return 202 + scan_id immediatamente.
    let scan_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO nexus_quality_scans (project_id, status) \
         VALUES ($1, 'running') RETURNING id",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("insert scan: {e}"),
        )
    })?;

    let db = state.db.clone();
    let orchestrator = state.orchestrator.clone();
    let dep_status = state.dependency_status.clone();
    let channels = state.project_channels.clone();
    tokio::spawn(async move {
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
                let by_sev_json = serde_json::to_value(&by_severity).unwrap_or(json!({}));
                let by_cat_json = serde_json::to_value(&by_category).unwrap_or(json!({}));
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
                .execute(&db)
                .await;
                nexus_events::dispatcher::emit(
                    &channels,
                    project_id,
                    nexus_events::ProjectEvent::QualityScanProgress {
                        scan_id: scan_id.to_string(),
                        phase: "completed".to_string(),
                        percent: Some(100),
                    },
                );
                nexus_events::dispatcher::emit(
                    &channels,
                    project_id,
                    nexus_events::ProjectEvent::FindingsUpdated {
                        scan_id: None,
                        total: total_findings as i64,
                        critical: *by_severity.get("critical").unwrap_or(&0) as i64,
                        warnings: *by_severity.get("warning").unwrap_or(&0) as i64,
                        resolved_ids: vec![],
                    },
                );
                tracing::info!(
                    "quality_scan background: scan_id={} project_id={} files={} findings={} duration_ms={}",
                    scan_id, project_id, files_scanned, total_findings, duration_ms
                );
            }
            Err(e) => {
                let duration_ms = started.elapsed().as_millis() as i32;
                let _ = sqlx::query(
                    "UPDATE nexus_quality_scans \
                     SET status = 'failed', error_message = $1, duration_ms = $2, completed_at = NOW() \
                     WHERE id = $3"
                )
                .bind(e.to_string())
                .bind(duration_ms)
                .bind(scan_id)
                .execute(&db)
                .await;
                tracing::warn!("quality_scan background: scan_id={} FAILED: {}", scan_id, e);
            }
        }
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
    let mut total_findings = 0u32;
    let mut findings_by_severity: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut findings_by_category: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

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
        let rel_path = std::path::Path::new(file_path.as_str())
            .strip_prefix(root_path)
            .unwrap_or(std::path::Path::new(file_path.as_str()));
        let rel_str = rel_path
            .to_string_lossy()
            .trim_start_matches('/')
            .trim_start_matches('\\')
            .replace('\\', "/");

        file_contents.insert(rel_str.clone(), content.clone());

        let file_findings: Vec<(String, String, String, String, Option<i32>)> =
            if file_path.ends_with(".sql") {
                let report = mcp_db::analyze_query(&content);
                report
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
                let report = mcp_quality::analyze_source(&rel_str, &content);
                report
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
            };

        for (category, severity, title, detail, line_number) in file_findings {
            // Genera context_snippet se abbiamo un numero di riga
            let snippet = line_number.map(|ln| mcp_quality::extract_context_snippet(
                    &content,
                    ln as usize,
                    5,
                ));

            batch.push(FindingRow {
                file: rel_str.clone(),
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

    // ── Fase vettoriale: arricchimento finding con analisi semantica ──────────
    // Guard: se il watchdog ha rilevato Qdrant o embedder down, salta
    // direttamente senza perdere tempo in tentativi e timeout.
    let qdrant_ok = dep_status.qdrant.load(std::sync::atomic::Ordering::Relaxed);
    let embedder_ok = dep_status
        .embedder
        .load(std::sync::atomic::Ordering::Relaxed);
    let skip_vector = !qdrant_ok || !embedder_ok;
    if skip_vector {
        tracing::info!(
            "quality_scan: skip fase vettoriale (watchdog: qdrant={}, embedder={})",
            qdrant_ok,
            embedder_ok
        );
    }

    // Se il watchdog segnala dipendenze sane, procede con arricchimento.
    // Timeout 60s come difesa di ultima istanza (le dipendenze potrebbero
    // cadere tra un probe e l'altro).
    let vector_enrichment_ok = if skip_vector {
        false
    } else {
        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            enrich_findings_with_vectors(db, orchestrator, project_id, &mut batch, &file_contents),
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
        }
    };
    if !vector_enrichment_ok && !skip_vector {
        tracing::info!(
            "quality_scan: arricchimento vettoriale non disponibile per progetto {}, \
             i finding mantengono solo analisi regex",
            project_id
        );
    }

    // ── Fase duplicati semantici ──────────────────────────────────────────────
    let semantic_dups = if skip_vector {
        Vec::new()
    } else {
        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            detect_semantic_duplicates(db, orchestrator, project_id, &file_contents),
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
    };
    for dup in &semantic_dups {
        batch.push(dup.clone());
    }

    // Conta totali
    for row in &batch {
        if !row.is_auto_suppressed {
            total_findings += 1;
            *findings_by_severity
                .entry(row.severity.clone())
                .or_insert(0) += 1;
            *findings_by_category
                .entry(row.category.clone())
                .or_insert(0) += 1;
        }
    }

    // Insert batchata: chunk da 200 row (piu' colonne = query piu' lunga).
    for chunk in batch.chunks(200) {
        if chunk.is_empty() {
            continue;
        }
        let cols = 11; // project_id, file_path, category, severity, title, detail, line_number, confidence, context_snippet, related_files, is_auto_suppressed
        let mut q = String::from(
            "INSERT INTO project_quality_findings \
             (project_id, file_path, category, severity, title, detail, line_number, \
              confidence, context_snippet, related_files, is_auto_suppressed) VALUES ",
        );
        let mut first = true;
        for i in 0..chunk.len() {
            if !first {
                q.push(',');
            }
            first = false;
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

    // Riapplica false-positive
    for fp_row in &fp_rows {
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

    // Hook detector DB
    {
        let project_root_path = std::path::PathBuf::from(root_path);
        let db_profile = crate::project_db::detector::detect_db_profile(&project_root_path);
        if db_profile.migration_tool.is_some() || !db_profile.marker_files.is_empty() {
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
                         project_database_config.detection_metadata = '{}'::jsonb"#
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
    }

    Ok((
        files.len(),
        total_findings,
        findings_by_severity,
        findings_by_category,
    ))
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

/// GET /api/projects/:id/quality-findings
pub async fn get_quality_findings(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let category = params
        .get("category")
        .cloned()
        .unwrap_or_else(|| "all".to_string());
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    // Se il chiamante non specifica severity, usa quality_severity_threshold dal DB come default.
    // "all" esplicito bypassa il threshold (es. pagina admin che vuole tutto).
    let severity: String = match params.get("severity") {
        Some(s) => s.clone(),
        None => sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'quality_severity_threshold'",
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "low".to_string()),
    };

    let rows = if severity == "all" && category == "all" {
        sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END, file_path \
             LIMIT $2"
        )
        .bind(project_id).bind(limit)
        .fetch_all(&state.db).await
    } else if severity == "high" && category == "all" {
        // threshold high: mostra solo high
        sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity = 'high' AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY file_path LIMIT $2"
        )
        .bind(project_id).bind(limit)
        .fetch_all(&state.db).await
    } else if severity == "medium" && category == "all" {
        // threshold medium: mostra high + medium
        sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity IN ('high','medium') AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY CASE severity WHEN 'high' THEN 1 ELSE 2 END, file_path LIMIT $2"
        )
        .bind(project_id).bind(limit)
        .fetch_all(&state.db).await
    } else if severity != "all" && category == "all" {
        sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND severity = $2 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY file_path LIMIT $3"
        )
        .bind(project_id).bind(severity).bind(limit)
        .fetch_all(&state.db).await
    } else {
        sqlx::query(
            "SELECT id, file_path, category, severity, title, detail, line_number, fixed_at, scanned_at, confidence, context_snippet, related_files \
             FROM project_quality_findings \
             WHERE project_id = $1 AND category = $2 AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
             ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END LIMIT $3"
        )
        .bind(project_id).bind(category).bind(limit)
        .fetch_all(&state.db).await
    }.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let findings: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: Uuid = r.try_get("id").unwrap_or_default();
            let fixed_at: Option<chrono::DateTime<chrono::Utc>> =
                r.try_get("fixed_at").ok().flatten();
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
        })
        .collect();

    Ok(Json(
        json!({ "findings": findings, "total": findings.len() }),
    ))
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

    let row = sqlx::query(
        r#"SELECT COALESCE(r.root_path, w.absolute_path) AS root_path
           FROM projects p
           LEFT JOIN repositories r ON r.project_id = p.id
           LEFT JOIN workspaces w ON w.project_id = p.id
           WHERE p.id = $1
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "project not found".to_string()))?;

    let root_path: String = row
        .try_get("root_path")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let abs_path = format!(
        "{}/{}",
        root_path.trim_end_matches('/'),
        file_path.trim_start_matches('/')
    );
    let content = std::fs::read_to_string(&abs_path)
        .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("file not found: {e}")))?;

    let findings: Vec<serde_json::Value> = if file_path.ends_with(".sql") {
        let report = mcp_db::analyze_query(&content);
        report
            .findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "category": f.category,
                    "severity": f.severity,
                    "title": f.title,
                    "detail": f.detail,
                    "lineNumber": serde_json::Value::Null,
                    "filePath": file_path,
                })
            })
            .collect()
    } else {
        let report = mcp_quality::analyze_source(&file_path, &content);
        report
            .findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "category": f.category,
                    "severity": f.severity,
                    "title": f.title,
                    "detail": f.detail,
                    "lineNumber": f.line.map(|l| l as i32),
                    "filePath": file_path,
                })
            })
            .collect()
    };

    Ok(Json(serde_json::json!({ "findings": findings })))
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

    let row = sqlx::query_as::<_, (String,)>(
        "SELECT COALESCE(r.root_path, w.absolute_path, p.analysis_json->>'rootPath', '') \
         FROM projects p \
         LEFT JOIN repositories r ON r.project_id = p.id \
         LEFT JOIN workspaces w ON w.project_id = p.id \
         WHERE p.id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (root_path,) = row;

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

    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();
    let start_idx = start_line.saturating_sub(1).min(total_lines);
    let end_idx = end_line.min(total_lines);

    let lines = all_lines[start_idx..end_idx].join("\n");

    Ok(Json(json!({
        "lines": lines,
        "startLine": start_idx + 1,
        "endLine": end_idx,
        "totalLines": total_lines,
    })))
}

// ── Arricchimento vettoriale dei finding ─────────────────────────────────────

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
    let test_embed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        orchestrator.embed_text("test"),
    )
    .await;
    match test_embed {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                "quality_scan vector: embedder non raggiungibile: {}, skip arricchimento",
                e
            );
            return false;
        }
        Err(_) => {
            tracing::warn!("quality_scan vector: timeout 10s test embedder, skip arricchimento");
            return false;
        }
    }

    // Pattern HTTP (non-DB) per sopprimere falsi positivi N+1
    let http_patterns = [
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
    // Pattern DB reali
    let db_patterns = [
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

    let mut enriched_count = 0u32;

    // Ordina gli indici per priorita': HIGH prima, poi MEDIUM.
    // Cosi' i finding piu' importanti vengono arricchiti per primi (cap = 50).
    let mut indices: Vec<usize> = (0..batch.len())
        .filter(|i| batch[*i].severity == "high" || batch[*i].severity == "medium")
        .collect();
    indices.sort_by_key(|i| if batch[*i].severity == "high" { 0 } else { 1 });

    for idx in indices {
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

        // Per finding "reliability" (N+1), verifica se il contesto contiene pattern HTTP
        if row.category == "reliability" && row.title.contains("N+1") {
            if let Some(ref snippet) = row.context_snippet {
                let snippet_lower = snippet.to_lowercase();
                let has_http = http_patterns
                    .iter()
                    .any(|p| snippet_lower.contains(&p.to_lowercase()));
                let has_db = db_patterns
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
                    continue;
                } else if has_db {
                    row.confidence = Some("high".into());
                } else {
                    row.confidence = Some("medium".into());
                }
            }
        }

        // Cerca file semanticamente correlati via code index
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
        let embed_result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orchestrator.embed_text(&search_text),
        )
        .await;
        match embed_result {
            Ok(Ok(vector)) => {
                match vector_memory::search_code_index(db, &vector, project_id, 5).await {
                    Ok(hits) => {
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
                        enriched_count += 1;

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
                    Err(e) => {
                        tracing::debug!("quality_scan vector: ricerca code index fallita: {}", e);
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::debug!("quality_scan vector: embedding fallito: {}", e);
            }
            Err(_) => {
                tracing::warn!("quality_scan vector: timeout 10s embedding, skip finding");
            }
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
                Ok(Err(_)) | Err(_) => continue,
            };
            total_embedded += 1;

            let hits = match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                vector_memory::search_code_index(db, &vector, project_id, 5),
            )
            .await
            {
                Ok(Ok(h)) => h,
                Ok(Err(_)) | Err(_) => continue,
            };

            for hit in &hits {
                if hit.score < 0.85 {
                    continue;
                }

                let hit_file = match hit.payload.get("file_path").and_then(|v| v.as_str()) {
                    Some(f) => f.to_string(),
                    None => continue,
                };

                // Ignora match nello stesso file
                if hit_file == *rel_path {
                    continue;
                }

                // Dedup coppie
                let pair_key = if rel_path < &hit_file {
                    (rel_path.clone(), hit_file.clone())
                } else {
                    (hit_file.clone(), rel_path.clone())
                };
                if seen_pairs.contains(&pair_key) {
                    continue;
                }
                seen_pairs.insert(pair_key);

                let hit_chunk = hit
                    .payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(contenuto non disponibile)");
                // Prendi solo le prime 3 righe del chunk come preview
                let preview: String = hit_chunk.lines().take(3).collect::<Vec<_>>().join(" | ");

                findings.push(FindingRow {
                    file: rel_path.clone(),
                    category: "semantic_duplication".into(),
                    severity: "medium".into(),
                    title: format!(
                        "Duplicato semantico: `{}` simile a codice in `{}`",
                        body.name, hit_file
                    ),
                    detail: format!(
                        "La funzione `{}` (righe {}-{}) ha similarita' {:.0}% con un blocco in `{}`. \
                         Preview: {}. Valutare estrazione in modulo condiviso.",
                        body.name, body.start_line, body.end_line,
                        hit.score * 100.0, hit_file, preview
                    ),
                    line_number: Some(body.start_line as i32),
                    confidence: Some("medium".into()),
                    context_snippet: Some(mcp_quality::extract_context_snippet(
                        content, body.start_line, 3
                    )),
                    related_files: Some(vec![hit_file]),
                    is_auto_suppressed: false,
                });
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

// ── Auto-scan singolo file su write/edit ─────────────────────────────────────

/// Chiamata in fire-and-forget dopo ogni write_file / edit_file.
/// Legge `quality_auto_scan` dal DB: se `"true"` esegue lo scan
/// solo sul file appena modificato (non sull'intero progetto).
/// Applica le stesse regole regex di `perform_quality_scan`, ma
/// limita la scansione al singolo path per non appesantire ogni salvataggio.
pub async fn maybe_auto_scan_file(
    db: &sqlx::PgPool,
    project_id: Uuid,
    file_path: &std::path::Path,
) {
    // Controlla il setting quality_auto_scan
    let enabled = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'quality_auto_scan'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(false);

    if !enabled {
        return;
    }

    // Legge threshold per filtrare i finding da inserire
    let threshold = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'quality_severity_threshold'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
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

    // Esegue i checker sul file singolo via mcp_quality::analyze_source (crate esterna)
    let report = mcp_quality::analyze_source(path_str, &content);
    if report.findings.is_empty() {
        return;
    }

    // Severità minima: low=0, medium=1, high=2
    let min_level: u8 = match threshold.as_str() {
        "high" => 2,
        "medium" => 1,
        _ => 0, // "low" o qualsiasi altro valore
    };

    // Elimina i finding precedenti per questo file e progetto
    let _ = sqlx::query(
        "DELETE FROM project_quality_findings WHERE project_id = $1 AND file_path = $2",
    )
    .bind(project_id)
    .bind(path_str)
    .execute(db)
    .await;

    // Inserisce i nuovi finding filtrati per soglia
    let mut inserted = 0u32;
    for f in &report.findings {
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

    tracing::debug!(
        "auto_scan_file: project={} file={} findings_raw={} inserted={} threshold={}",
        project_id,
        path_str,
        report.findings.len(),
        inserted,
        threshold
    );
}
