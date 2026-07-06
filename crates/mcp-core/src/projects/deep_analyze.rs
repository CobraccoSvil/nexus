// Analisi AI profonda del progetto tramite agent.project.analyzer.

use super::*;

// ── Costanti deep-analyzer ────────────────────────────────────────────────────

/// Lista (allowlist) dei file di configurazione raccolti dal deep-analyzer.
const DEEP_ANALYZER_CONFIG_PATTERNS: &[&str] = &[
    ".env",
    ".env.example",
    ".env.dev",
    ".env.development",
    ".env.local",
    ".env.prod",
    ".env.prod.example",
    "docker-compose.yml",
    "docker-compose.dev.yml",
    "docker-compose.prod.yml",
    "Dockerfile",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "Gemfile",
    "composer.json",
    "appsettings.json",
    "appsettings.Development.json",
    "appsettings.Production.json",
    "Makefile",
    "README.md",
];

/// Massimo numero di file di config raccolti.
const DEEP_ANALYZER_MAX_FILES: usize = 20;
/// Massima dimensione singolo file (byte) — oltre viene troncato.
const DEEP_ANALYZER_MAX_FILE_BYTES: usize = 12_000;
/// Profondita' massima di walk per cercare file di config nelle subdirectory.
const DEEP_ANALYZER_MAX_DEPTH: usize = 3;

// ── Helper privati ────────────────────────────────────────────────────────────

/// Walk asincrona limitata: raccoglie file il cui basename matcha i pattern.
pub(super) async fn collect_config_files(root: &Path) -> Vec<serde_json::Value> {
    let patterns: std::collections::HashSet<&str> =
        DEEP_ANALYZER_CONFIG_PATTERNS.iter().copied().collect();
    let mut found: Vec<serde_json::Value> = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    let skip_dirs: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".next",
        "bin",
        "obj",
        ".venv",
        "__pycache__",
        ".turbo",
        ".cache",
        "vendor",
    ];

    while let Some((dir, depth)) = stack.pop() {
        if found.len() >= DEEP_ANALYZER_MAX_FILES {
            break;
        }
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if let Ok(ft) = entry.file_type().await {
                if ft.is_dir() {
                    if should_descend_dir(&name, depth, skip_dirs) {
                        stack.push((path, depth + 1));
                    }
                } else if ft.is_file() && patterns.contains(name.as_str()) {
                    if found.len() >= DEEP_ANALYZER_MAX_FILES {
                        break;
                    }
                    found.push(read_config_file_entry(&path, root, &name).await);
                }
            }
        }
    }
    found
}

/// Decide se scendere in una subdirectory durante la walk: rispetta la
/// profondita' massima, salta le dir nascoste (eccetto `.github`) e la lista
/// `skip_dirs`. Estratto da [`collect_config_files`] (stessa logica inline).
fn should_descend_dir(name: &str, depth: usize, skip_dirs: &[&str]) -> bool {
    if depth + 1 > DEEP_ANALYZER_MAX_DEPTH {
        return false;
    }
    if name.starts_with('.') && name != "." && name != ".github" {
        return false;
    }
    !skip_dirs.contains(&name)
}

/// Legge un file di config candidato e ne costruisce la entry JSON
/// (`path`/`content`/`truncated`/`size_bytes`), troncando il contenuto a
/// [`DEEP_ANALYZER_MAX_FILE_BYTES`]. Estratto da [`collect_config_files`].
async fn read_config_file_entry(path: &Path, root: &Path, name: &str) -> serde_json::Value {
    let rel_path = path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_string());
    let raw = tokio::fs::read(path).await.unwrap_or_default();
    let truncated = raw.len() > DEEP_ANALYZER_MAX_FILE_BYTES;
    let bytes_slice = if truncated {
        &raw[..DEEP_ANALYZER_MAX_FILE_BYTES]
    } else {
        &raw[..]
    };
    let content = String::from_utf8_lossy(bytes_slice).to_string();
    json!({
        "path": rel_path,
        "content": content,
        "truncated": truncated,
        "size_bytes": raw.len(),
    })
}

/// Recupera i servizi systemd registrati per il progetto.
///
/// Linux-only: interroga il bus systemd `--user` via `bash -lc`. Su Windows non
/// esiste systemd; il ramo dedicato ritorna una lista vuota (best-effort, coerente
/// con la natura non bloccante dell'analisi) senza spawnare `bash`/`systemctl`
/// (assenti) e senza spammare errori.
#[cfg(unix)]
pub(super) async fn collect_registered_services(slug: &str) -> Vec<serde_json::Value> {
    use tokio::process::Command;
    let out = Command::new("bash")
        .arg("-lc")
        .arg(format!(
            "systemctl --user list-unit-files '{slug}-*.service' --no-pager --no-legend 2>/dev/null | awk '{{print $1}}'"
        ))
        .output()
        .await;
    let mut services = Vec::new();
    if let Ok(o) = out {
        let txt = String::from_utf8_lossy(&o.stdout);
        for line in txt.lines() {
            let unit = line.trim();
            if unit.is_empty() {
                continue;
            }
            let info = Command::new("bash")
                .arg("-lc")
                .arg(format!(
                    "systemctl --user show '{unit}' --property=ActiveState,ExecStart,WorkingDirectory --no-pager 2>/dev/null"
                ))
                .output().await;
            let mut active_state = String::new();
            let mut exec_start = String::new();
            let mut workdir = String::new();
            if let Ok(i) = info {
                let body = String::from_utf8_lossy(&i.stdout);
                for ln in body.lines() {
                    if let Some(v) = ln.strip_prefix("ActiveState=") {
                        active_state = v.to_string();
                    } else if let Some(v) = ln.strip_prefix("ExecStart=") {
                        exec_start = v.to_string();
                    } else if let Some(v) = ln.strip_prefix("WorkingDirectory=") {
                        workdir = v.to_string();
                    }
                }
            }
            services.push(json!({
                "unit": unit,
                "active_state": active_state,
                "exec_start": exec_start,
                "working_dir": workdir,
            }));
        }
    }
    services
}

/// Ramo Windows: nessun systemd. Best-effort vuoto (l'analisi profonda non
/// dipende da questa sezione su Windows nativo).
#[cfg(windows)]
pub(super) async fn collect_registered_services(_slug: &str) -> Vec<serde_json::Value> {
    Vec::new()
}

// ── Pipeline analyzer (LLM via gateway) ────────────────────────────────────────

/// Placeholder del template `agent.project.analyzer` (mig 0094). Stessa lista
/// usata storicamente dal brain (`_render_analyzer_prompt`): la fonte e' il
/// template DB, qui si fa solo la sostituzione testuale.
const ANALYZER_CONFIG_CONTENT_MAX: usize = 8_000;

/// Costruisce il prompt dell'analyzer sostituendo i placeholder `{{...}}` del
/// template col payload del progetto. I file di config sono serializzati in JSON
/// compatto (content troncato a [`ANALYZER_CONFIG_CONTENT_MAX`] char come nel
/// rendering storico) e inseriti come stringa.
fn render_analyzer_prompt(
    template: &str,
    repo_summary: &str,
    lang_hint: &str,
    frameworks_list: &[String],
    config_files: &[serde_json::Value],
    registered_services: &[serde_json::Value],
) -> String {
    let config_payload: Vec<serde_json::Value> = config_files
        .iter()
        .map(|f| {
            let content = f
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .chars()
                .take(ANALYZER_CONFIG_CONTENT_MAX)
                .collect::<String>();
            json!({
                "path": f.get("path").and_then(|p| p.as_str()).unwrap_or(""),
                "content": content,
                "truncated": f.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    let config_str = serde_json::to_string(&config_payload).unwrap_or_else(|_| "[]".to_string());
    let services_str =
        serde_json::to_string(registered_services).unwrap_or_else(|_| "[]".to_string());
    let frameworks = if frameworks_list.is_empty() {
        "nessuno rilevato".to_string()
    } else {
        frameworks_list.join(", ")
    };
    let lang = if lang_hint.is_empty() {
        "non determinato"
    } else {
        lang_hint
    };

    template
        .replace("{{lang_hint}}", lang)
        .replace("{{frameworks_list}}", &frameworks)
        .replace("{{repo_summary}}", repo_summary)
        .replace("{{config_files_payload}}", &config_str)
        .replace("{{registered_services}}", &services_str)
}

/// Esegue l'agente `agent.project.analyzer` interamente in Rust: carica il
/// template dal DB, lo renderizza col payload, chiama il Nexus Gateway col
/// modello del purpose `project_analyzer` (tier-routed) e parsa il JSON degli
/// insights. Ritorna un `Value` con la stessa forma della vecchia risposta del
/// brain (`status`/`insights`/`model_used`/`duration_ms`/`error`) cosi' il
/// chiamante non cambia il codice a valle. `Err(messaggio)` per i fallimenti
/// non recuperabili (template assente, routing non disponibile): il chiamante
/// li marca come run `failed`.
/// Carica il template attivo `agent.project.analyzer` dal punto unico
/// nexus_prompt_templates (regola L). `Err` se DB down o template assente.
async fn load_analyzer_template(db: &sqlx::PgPool) -> Result<String, String> {
    let template: Option<String> = sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates \
         WHERE key = 'agent.project.analyzer' AND is_active = TRUE \
         ORDER BY version DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("DB template analyzer: {e}"))?;
    template.ok_or_else(|| "prompt agent.project.analyzer non trovato/attivo in DB".to_string())
}

/// Risolve `(provider, model)` per il purpose `project_analyzer` via routing per
/// tier (mig 0461): `best_model_for_tier` sceglie il miglior modello del catalog
/// escludendo i provider in cooldown, sostituendo il loop chain manuale del brain
/// (regola L). Niente nome modello hardcoded (regola G). `Err` se non risolvibile.
async fn resolve_analyzer_model(db: &sqlx::PgPool) -> Result<(String, String), String> {
    match crate::internal_routing::resolve_purpose_model_db(db, "project_analyzer").await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider,
            model,
            rationale,
        } => {
            tracing::info!("project_analyze: modello risolto {provider}/{model} ({rationale})");
            Ok((provider, model))
        }
        other => Err(format!(
            "routing purpose 'project_analyzer' non risolvibile: {}",
            other.into_model("project_analyzer").err().unwrap_or_default()
        )),
    }
}

/// Mappa la risposta del gateway nella forma storica del brain
/// (`status`/`insights`/`model_used`/`duration_ms`/`error`). Estratto da
/// [`run_analyzer_completion`] (comportamento identico).
fn map_analyzer_response(
    resp: Result<crate::nexus_gateway::GwResponse, impl std::fmt::Display>,
    model_used: &str,
    started: std::time::Instant,
) -> serde_json::Value {
    let duration_ms = || started.elapsed().as_millis() as i64;
    match resp {
        Ok(resp) => {
            let content = resp.content.trim().to_string();
            if content.is_empty() {
                return json!({
                    "status": "failed",
                    "error": format!("{model_used}: risposta vuota"),
                    "insights": null,
                    "model_used": model_used,
                    "duration_ms": duration_ms(),
                });
            }
            // Parsing via il punto unico llm_json (gestisce fence/preamboli).
            match crate::llm_json::parse_llm_json(&content) {
                Ok(parsed) => json!({
                    "status": "completed",
                    "insights": parsed,
                    "model_used": model_used,
                    "duration_ms": duration_ms(),
                }),
                Err(e) => json!({
                    "status": "failed",
                    "error": format!("{model_used}: output non parsabile come JSON: {e}"),
                    "insights": null,
                    "model_used": model_used,
                    "duration_ms": duration_ms(),
                }),
            }
        }
        Err(e) => json!({
            "status": "failed",
            "error": format!("{model_used}: {e}"),
            "insights": null,
            "model_used": model_used,
            "duration_ms": duration_ms(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_analyzer_completion(
    db: &sqlx::PgPool,
    project_name: &str,
    repo_summary: &str,
    lang_hint: &str,
    frameworks_list: &[String],
    config_files: &[serde_json::Value],
    registered_services: &[serde_json::Value],
    started: std::time::Instant,
) -> Result<serde_json::Value, String> {
    let template = load_analyzer_template(db).await?;

    let summary = if repo_summary.is_empty() {
        format!("progetto {project_name}")
    } else {
        repo_summary.to_string()
    };
    let rendered = render_analyzer_prompt(
        &template,
        &summary,
        lang_hint,
        frameworks_list,
        config_files,
        registered_services,
    );

    let (provider, model) = resolve_analyzer_model(db).await?;

    // Completion via Nexus Gateway, pinnando il provider deciso a monte (no
    // secondo routing divergente; il cooldown e' gia' stato applicato dalla
    // selezione per tier). Errore della singola chiamata -> status failed (non
    // panic): il run resta tracciato.
    let gw = crate::nexus_gateway::NexusGatewayClient::from_db(db).await;
    let gw_req = crate::nexus_gateway::GwRequest {
        model: format!("{provider}/{model}"),
        messages: vec![crate::nexus_gateway::GwMessage {
            role: "user".to_string(),
            content: json!(rendered),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            thinking_signature: None,
        }],
        max_tokens: Some(8000),
        pin_provider: Some(provider.clone()),
        metadata: crate::nexus_gateway::GwMetadata {
            feature: "project_analyzer".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };

    let model_used = format!("{provider}/{model}");
    let resp = gw.complete(gw_req).await;
    Ok(map_analyzer_response(resp, &model_used, started))
}

// ── Handler HTTP ──────────────────────────────────────────────────────────────

/// POST /api/projects/:id/deep-analyze — analisi AI profonda del progetto.
///
/// Refactor 0102 (Wave structural): handler ASINCRONO. Spezzato in:
///  - Fase sync (rapida): insert riga insights con status='running', return 202 + run_id
///  - Fase async (background tokio task): chiamata brain + UPDATE riga finale
///
/// Risolve il bug "deep-analyze 500/timeout proxy 30s": la chiamata sincrona
/// poteva durare oltre il timeout del proxy Next.js, dropping connection,
/// client riceveva 500 anche se il job era in corso server-side.
///
/// Il client ora chiama POST → riceve 202 + run_id, poi GET /insights polla
/// finche' status != 'running'.
pub async fn deep_analyze_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = context.repository_root_path.clone();
    if !root.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Directory del progetto non trovata",
        ));
    }

    // ── Fase sync: insert riga 'running' e return 202 ────────────────────────
    let run_id = insert_running_row(&state.db, project_id).await?;

    // Snapshot dei dati per la fase async (la closure vive con 'static).
    let db = state.db.clone();
    let neural = state.orchestrator.neural.clone();
    let project_channels = state.project_channels.clone();
    let repo_root_str = root.to_string_lossy().to_string();
    let project_name = context.details.name.clone();
    let project_slug = context.details.slug.clone();

    tokio::spawn(async move {
        run_deep_analyze_background(
            db,
            neural,
            project_id,
            run_id,
            root,
            project_name,
            project_slug,
        )
        .await;
        // Clone tenuti volutamente (riservati a uso futuro): li scartiamo qui per
        // preservare l'ownership senza effetti osservabili.
        let _ = (repo_root_str, project_channels);
    });

    // Risposta immediata 202 Accepted con run_id per polling client-side
    Ok(Json(json!({
        "run_id": run_id,
        "status": "running",
        "message": "Analisi avviata in background. Polla GET /api/projects/:id/insights ogni 3s finche' status != 'running'.",
    })))
}

/// Inserisce la riga insights iniziale con `status='running'` e ritorna il
/// `run_id` generato. Estratto dalla fase sync di [`deep_analyze_project`]
/// (query INSERT parametrizzata, comportamento identico).
async fn insert_running_row(db: &sqlx::PgPool, project_id: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO nexus_project_insights
            (project_id, insight_version, insights, prompt_key, prompt_version,
             status, config_files_count)
         VALUES ($1, 1, '{}'::jsonb, 'agent.project.analyzer', 1, 'running', 0)
         RETURNING id",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("insert running: {e}"),
        )
    })
}

/// Ricava `(lang_hint, frameworks_list, repo_summary)` dall'ultima analisi
/// statica del progetto. Estratto da [`deep_analyze_project`] per contenere la
/// lunghezza della fase async (comportamento identico).
fn build_repo_summary(
    static_analysis: &serde_json::Value,
    project_name: &str,
) -> (String, Vec<String>, String) {
    let lang_hint = static_analysis
        .get("languages")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("language"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let frameworks_list: Vec<String> = static_analysis
        .get("frameworks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let repo_summary = format!(
        "{} file totali in {} (linguaggio dominante: {}). Framework: {}.",
        static_analysis
            .get("totalFiles")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        project_name,
        if lang_hint.is_empty() {
            "non determinato"
        } else {
            lang_hint.as_str()
        },
        if frameworks_list.is_empty() {
            "nessuno".to_string()
        } else {
            frameworks_list.join(", ")
        },
    );
    (lang_hint, frameworks_list, repo_summary)
}

/// Persiste il risultato dell'analyzer sulla riga 'running' e ritorna
/// `(status_str, insights_payload)` per l'eventuale seeding. Estratto da
/// [`deep_analyze_project`] (comportamento identico all'UPDATE inline).
async fn persist_analyzer_result(
    db: &sqlx::PgPool,
    run_id: i64,
    cfg_count: i32,
    started: std::time::Instant,
    brain_resp: &serde_json::Value,
) -> (String, serde_json::Value) {
    let status_str = brain_resp
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("failed")
        .to_string();
    let model_used = brain_resp
        .get("model_used")
        .and_then(|v| v.as_str())
        .map(String::from);
    let duration_ms = brain_resp
        .get("duration_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| started.elapsed().as_millis() as i64) as i32;
    let insights_payload = brain_resp.get("insights").cloned().unwrap_or(json!({}));
    let error_msg = brain_resp
        .get("error")
        .and_then(|v| v.as_str())
        .map(String::from);

    let _ = sqlx::query(
        "UPDATE nexus_project_insights
            SET insights = $1, model_used = $2, duration_ms = $3,
                config_files_count = $4, status = $5, error_message = $6
            WHERE id = $7",
    )
    .bind(&insights_payload)
    .bind(&model_used)
    .bind(duration_ms)
    .bind(cfg_count)
    .bind(&status_str)
    .bind(&error_msg)
    .bind(run_id)
    .execute(db)
    .await;

    tracing::info!(
        "deep_analyze background: run_id={} status={} duration_ms={}",
        run_id,
        status_str,
        duration_ms
    );

    (status_str, insights_payload)
}

/// Corpo della fase async del deep-analyze (eseguita in un task tokio staccato).
///
/// Pipeline: recupero analisi statica -> raccolta config/servizi -> completion
/// analyzer -> UPDATE riga finale -> seed insights su wiki. Estratto dalla
/// closure di [`deep_analyze_project`] per contenerne la lunghezza; il
/// comportamento osservabile (query, log, seed) e' identico.
async fn run_deep_analyze_background(
    db: sqlx::PgPool,
    neural: crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    run_id: i64,
    root: std::path::PathBuf,
    project_name: String,
    project_slug: String,
) {
    let started = std::time::Instant::now();

    // 1. Recupera l'ultima analisi statica e ne deriva il riassunto.
    let static_analysis: serde_json::Value = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT analysis_json FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(&db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or(json!({}));
    let (lang_hint, frameworks_list, repo_summary) =
        build_repo_summary(&static_analysis, &project_name);

    // 2. Raccoglie config files dal filesystem.
    let config_files = collect_config_files(&root).await;
    let cfg_count = config_files.len() as i32;

    // 3. Servizi systemd registrati.
    let services = collect_registered_services(&project_slug).await;

    // 4. Esegue l'agente analyzer interamente in Rust (cutover brain->Rust):
    //    template DB (punto unico nexus_prompt_templates) -> render placeholder
    //    -> completion via Nexus Gateway -> parse JSON. Nessuna chiamata al
    //    brain Python. La pipeline replica quella storica di /agent/project-analyze
    //    (prompt agent.project.analyzer, mig 0094) ma il fallback per cooldown
    //    e' delegato al routing per tier (best_model_for_tier via purpose
    //    'project_analyzer', mig 0461) invece del loop chain manuale: il punto
    //    unico vive nel selettore modello + gateway (regola L), non duplicato qui.
    let brain_resp: serde_json::Value = match run_analyzer_completion(
        &db,
        &project_name,
        &repo_summary,
        &lang_hint,
        &frameworks_list,
        &config_files,
        &services,
        started,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            let _ = mark_failed(&db, run_id, &e, started.elapsed().as_millis() as i32).await;
            return;
        }
    };

    // 5. UPDATE finale della riga 'running'.
    let (status_str, insights_payload) =
        persist_analyzer_result(&db, run_id, cfg_count, started, &brain_resp).await;

    // ADR 0017 v2 TODO 4 — seed knowledge da insights deep-analyze.
    // Reimplementazione su `wiki_docs` (scope=project, kind='insight')
    // con embedding upsert in collection unificata `wiki_content`. Solo
    // sui run completati con successo (lo status_str == 'completed'):
    // sui run falliti il payload e' tipicamente vuoto/parziale.
    if status_str == "completed" {
        let neural_for_seed = neural.clone();
        let db_for_seed = db.clone();
        if let Err(e) = seed_insights_to_wiki(
            &db_for_seed,
            &neural_for_seed,
            project_id,
            run_id,
            &insights_payload,
        )
        .await
        {
            tracing::warn!(
                project_id = %project_id,
                run_id = run_id,
                error = %e,
                "deep_analyze: seed insights su wiki_docs fallito (best-effort)"
            );
        }
    }
}

/// Helper: marca una riga insights come 'failed' con error_message.
async fn mark_failed(
    db: &sqlx::PgPool,
    run_id: i64,
    msg: &str,
    duration_ms: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE nexus_project_insights
            SET status = 'failed', error_message = $1, duration_ms = $2
            WHERE id = $3",
    )
    .bind(msg)
    .bind(duration_ms)
    .bind(run_id)
    .execute(db)
    .await?;
    tracing::warn!("deep_analyze background: run_id={} FAILED: {}", run_id, msg);
    Ok(())
}

/// GET /api/projects/:id/insights — ritorna l'ultimo insight salvato.
pub async fn get_project_insights(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    // Verifica accesso
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    let row = sqlx::query(
        "SELECT insights, model_used, duration_ms, config_files_count, status,
                error_message, created_at
         FROM nexus_project_insights
         WHERE project_id = $1
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}")))?;

    match row {
        Some(r) => {
            let insights: serde_json::Value = r.try_get("insights").unwrap_or(json!({}));
            let model_used: Option<String> = r.try_get("model_used").ok();
            let duration_ms: Option<i32> = r.try_get("duration_ms").ok();
            let config_files_count: Option<i32> = r.try_get("config_files_count").ok();
            let status: Option<String> = r.try_get("status").ok();
            let error_message: Option<String> = r.try_get("error_message").ok();
            let created_at: Option<chrono::DateTime<chrono::Utc>> = r.try_get("created_at").ok();
            Ok(Json(json!({
                "exists": true,
                "insights": insights,
                "model_used": model_used,
                "duration_ms": duration_ms,
                "config_files_count": config_files_count,
                "status": status,
                "error_message": error_message,
                "created_at": created_at,
            })))
        }
        None => Ok(Json(json!({ "exists": false }))),
    }
}

// ── ADR 0017 v2 TODO 4 — seed insights -> wiki_docs ──────────────────────────
//
// Materializza il payload `insights` del deep-analyzer in uno o piu' documenti
// `wiki_docs` (scope=project, kind='insight'). Strategia:
//
//   1. Se il payload e' un oggetto con chiave `findings` / `insights` / `items`
//      contenente un array di oggetti -> uno wiki_doc per item.
//   2. Altrimenti un singolo wiki_doc "riepilogo" per run, body=JSON pretty.
//
// Idempotenza: lo slug e' deterministico (`insight-{run_id}` o
// `insight-{run_id}-{i}`) e usiamo ON CONFLICT DO UPDATE sull'indice unico
// `uq_wiki_docs_slug (scope, project_id, slug)`. Rieseguire la stessa
// analisi sullo stesso run aggiorna i doc esistenti senza duplicare.
//
// Embedding + upsert Qdrant: best-effort (best practice ADR 0017 v2). Se il
// brain e' down il doc resta in DB senza `qdrant_point_id`.
async fn seed_insights_to_wiki(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    run_id: i64,
    insights: &Value,
) -> anyhow::Result<()> {
    let items = extract_insight_items(insights);
    if items.is_empty() {
        tracing::debug!(
            project_id = %project_id,
            run_id = run_id,
            "deep_analyze.seed: nessun item da seedare (payload vuoto o non strutturato)"
        );
        return Ok(());
    }

    let total = items.len();
    let mut seeded = 0usize;
    for (idx, item) in items.iter().enumerate() {
        let slug = if total == 1 {
            format!("insight-run-{run_id}")
        } else {
            format!("insight-run-{run_id}-{idx:02}")
        };
        if seed_single_insight(db, neural, project_id, run_id, &slug, item).await {
            seeded += 1;
        }
    }

    tracing::info!(
        project_id = %project_id,
        run_id = run_id,
        seeded,
        total,
        "deep_analyze.seed: completato"
    );
    Ok(())
}

/// Costruisce i tag di un insight: base (`insight`, `deep-analyze`) + categoria +
/// `file:<path>` per ogni file rilevato, con dedup stabile (ordine preservato).
fn build_insight_tags(item: &InsightItem) -> Vec<String> {
    let mut tags: Vec<String> = vec!["insight".to_string(), "deep-analyze".to_string()];
    if let Some(category) = &item.category {
        tags.push(category.clone());
    }
    for fp in &item.file_paths {
        tags.push(format!("file:{fp}"));
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    tags
}

/// Embed del testo dell'insight + upsert nella collection `wiki_content`
/// (best-effort). Ritorna il `qdrant_point_id` se l'upsert e' andato a buon fine,
/// `None` se embed o upsert falliscono (il doc resta comunque in DB senza vector).
async fn embed_insight_point(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    item: &InsightItem,
    title: &str,
    body_md: &str,
    slug: &str,
) -> Option<String> {
    let snippet = if body_md.len() > 2000 {
        &body_md[..2000]
    } else {
        body_md
    };
    let combined = format!("{title}\n\n{snippet}");
    match neural.embed_text("", &combined).await {
        Ok(vector) => {
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "scope": "project",
                "project_id": project_id.to_string(),
                "doc_id": point_id,
                "title": title,
                "kind": "insight",
                "intent": item.category.clone().unwrap_or_default(),
            });
            match vector_memory::upsert_wiki_content_point(db, &point_id, vector, payload).await {
                Ok(_) => Some(point_id),
                Err(e) => {
                    tracing::debug!(
                        slug = %slug,
                        error = %e,
                        "deep_analyze.seed: upsert Qdrant fallito (proseguo)"
                    );
                    None
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                slug = %slug,
                error = %e,
                "deep_analyze.seed: embed_text fallito (proseguo senza vector)"
            );
            None
        }
    }
}

/// Materializza un singolo insight come `wiki_doc` (embed best-effort + upsert
/// idempotente su `uq_wiki_docs_slug`). Ritorna `true` se l'INSERT/UPDATE e'
/// riuscito. Estratto dal loop di [`seed_insights_to_wiki`] (comportamento
/// identico per singolo item).
async fn seed_single_insight(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    run_id: i64,
    slug: &str,
    item: &InsightItem,
) -> bool {
    let title = item
        .title
        .clone()
        .unwrap_or_else(|| format!("Insight run #{run_id}"));
    let body_md = item.body_md.clone();
    let tags = build_insight_tags(item);
    let qdrant_point_id =
        embed_insight_point(db, neural, project_id, item, &title, &body_md, slug).await;

    let body_hash = crate::wiki::vault::sha256_hex(&body_md);
    let res = sqlx::query(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags, qdrant_point_id,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            'insight', $6, $7, $8,
            'none', '{}', FALSE,
            $5, NULL,
            1, TRUE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title           = EXCLUDED.title,
            body_md         = EXCLUDED.body_md,
            body_hash       = EXCLUDED.body_hash,
            tags            = EXCLUDED.tags,
            qdrant_point_id = COALESCE(EXCLUDED.qdrant_point_id, wiki_docs.qdrant_point_id),
            generated_hash  = CASE
                                WHEN wiki_docs.manually_edited THEN wiki_docs.generated_hash
                                ELSE EXCLUDED.body_hash
                              END,
            updated_at      = NOW()
        "#,
    )
    .bind(project_id)
    .bind(slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(item.category.as_deref())
    .bind(&tags)
    .bind(qdrant_point_id.as_deref())
    .execute(db)
    .await;

    match res {
        Ok(_) => {
            tracing::info!(
                project_id = %project_id,
                run_id = run_id,
                slug = %slug,
                "wiki.deep_analyze: insight seedato come wiki_doc"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                run_id = run_id,
                slug = %slug,
                error = %e,
                "deep_analyze.seed: INSERT wiki_docs fallito"
            );
            false
        }
    }
}

/// Item normalizzato estratto dal payload insights (categorie LLM-agnostiche).
#[derive(Debug, Default, Clone)]
struct InsightItem {
    title: Option<String>,
    body_md: String,
    category: Option<String>,
    file_paths: Vec<String>,
}

/// Estrazione robusta degli "insight items" dal payload del brain. Il formato
/// concreto puo' variare (il prompt del project-analyzer e' DB-driven); qui
/// supportiamo le forme piu' comuni:
///
///   - { findings: [ {title, summary|description, category|kind, files: [..]} , ... ] }
///   - { insights: [ ... ] }
///   - { items: [ ... ] }
///   - oggetto top-level con campi 'summary', 'overview', ecc. -> singolo item
///   - tutto il resto -> singolo item con body = JSON pretty
fn extract_insight_items(insights: &Value) -> Vec<InsightItem> {
    // 1) Cerca array in keys note.
    for key in ["findings", "insights", "items", "results"] {
        if let Some(arr) = insights.get(key).and_then(|v| v.as_array()) {
            let items: Vec<InsightItem> = arr
                .iter()
                .filter_map(|el| el.as_object().map(parse_insight_object))
                .filter(|it| !it.body_md.trim().is_empty())
                .collect();
            if !items.is_empty() {
                return items;
            }
        }
    }

    // 2) Se il payload e' un oggetto top-level con `summary` o `overview` lo
    //    trasformiamo in un singolo item descrittivo.
    if let Some(obj) = insights.as_object() {
        return single_item_from_object(insights, obj);
    }

    Vec::new()
}

/// Costruisce (al piu') un singolo [`InsightItem`] descrittivo da un oggetto
/// top-level privo di array note. Fallback: dump JSON pretty come body markdown.
/// Estratto da [`extract_insight_items`] (comportamento identico).
fn single_item_from_object(
    insights: &Value,
    obj: &serde_json::Map<String, Value>,
) -> Vec<InsightItem> {
    // Vuoto -> niente da seedare.
    if obj.is_empty() {
        return Vec::new();
    }
    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let body = obj
        .get("summary")
        .or_else(|| obj.get("overview"))
        .or_else(|| obj.get("description"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Fallback: dump JSON intero come body markdown.
            format!(
                "```json\n{}\n```",
                serde_json::to_string_pretty(insights).unwrap_or_default()
            )
        });
    if body.trim().is_empty() {
        return Vec::new();
    }
    vec![InsightItem {
        title,
        body_md: body,
        category: obj
            .get("category")
            .or_else(|| obj.get("kind"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        file_paths: Vec::new(),
    }]
}

fn parse_insight_object(obj: &serde_json::Map<String, Value>) -> InsightItem {
    let title = obj
        .get("title")
        .or_else(|| obj.get("name"))
        .or_else(|| obj.get("headline"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let body_md = obj
        .get("body_md")
        .or_else(|| obj.get("body"))
        .or_else(|| obj.get("description"))
        .or_else(|| obj.get("summary"))
        .or_else(|| obj.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Se non c'e' un campo testuale, dump JSON dell'item.
            format!(
                "```json\n{}\n```",
                serde_json::to_string_pretty(&Value::Object(obj.clone())).unwrap_or_default()
            )
        });

    let category = obj
        .get("category")
        .or_else(|| obj.get("kind"))
        .or_else(|| obj.get("type"))
        .or_else(|| obj.get("severity"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let file_paths = collect_item_file_paths(obj);

    InsightItem {
        title,
        body_md,
        category,
        file_paths,
    }
}

/// Raccoglie i path dei file citati da un insight (chiavi array note + singolo
/// `file`), con trim, dedup e ordinamento stabile. Estratto da
/// [`parse_insight_object`] (comportamento identico).
fn collect_item_file_paths(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut file_paths: Vec<String> = Vec::new();
    for key in ["files", "file_paths", "evidence_files", "paths"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            for el in arr {
                if let Some(s) = el.as_str() {
                    let s = s.trim();
                    if !s.is_empty() {
                        file_paths.push(s.to_string());
                    }
                }
            }
        }
    }
    // Singolo path in chiave `file`.
    if let Some(s) = obj.get("file").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            file_paths.push(s.to_string());
        }
    }
    file_paths.sort();
    file_paths.dedup();
    file_paths
}

#[cfg(test)]
mod analyzer_tests {
    use super::*;

    #[test]
    fn render_analyzer_prompt_sostituisce_tutti_i_placeholder() {
        let template = "Lang: {{lang_hint}}\nFw: {{frameworks_list}}\n\
                        Repo: {{repo_summary}}\nCfg: {{config_files_payload}}\n\
                        Svc: {{registered_services}}";
        let config = vec![json!({
            "path": "package.json",
            "content": "{\"name\":\"demo\"}",
            "truncated": false
        })];
        let services = vec![json!({"unit": "demo.service", "active_state": "active"})];
        let out = render_analyzer_prompt(
            template,
            "10 file in demo",
            "typescript",
            &["next".to_string(), "react".to_string()],
            &config,
            &services,
        );
        // Nessun placeholder residuo.
        assert!(!out.contains("{{"), "placeholder non sostituiti: {out}");
        assert!(out.contains("Lang: typescript"));
        assert!(out.contains("Fw: next, react"));
        assert!(out.contains("Repo: 10 file in demo"));
        // Il payload config e' JSON con il path del file.
        assert!(out.contains("package.json"));
        assert!(out.contains("demo.service"));
    }

    #[test]
    fn render_analyzer_prompt_valori_vuoti_hanno_default() {
        let template = "{{lang_hint}}|{{frameworks_list}}";
        let out = render_analyzer_prompt(template, "", "", &[], &[], &[]);
        assert_eq!(out, "non determinato|nessuno rilevato");
    }

    #[test]
    fn render_analyzer_prompt_tronca_content_config_lungo() {
        let big = "x".repeat(ANALYZER_CONFIG_CONTENT_MAX + 500);
        let config = vec![json!({"path": "Big", "content": big, "truncated": true})];
        let out = render_analyzer_prompt("{{config_files_payload}}", "s", "go", &[], &config, &[]);
        // Il content e' troncato al massimo consentito (conta le 'x' nel JSON).
        let x_count = out.matches('x').count();
        assert_eq!(x_count, ANALYZER_CONFIG_CONTENT_MAX);
    }

    #[test]
    fn gateway_request_analyzer_pinna_provider_e_formatta_model() {
        // Verifica la forma della richiesta inviata al gateway: model
        // "provider/model", pin_provider valorizzato, feature di tracciamento.
        let provider = "openai";
        let model = "gpt-4.1-mini";
        let req = crate::nexus_gateway::GwRequest {
            model: format!("{provider}/{model}"),
            messages: vec![crate::nexus_gateway::GwMessage {
                role: "user".to_string(),
                content: json!("prompt analyzer"),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            }],
            max_tokens: Some(8000),
            pin_provider: Some(provider.to_string()),
            metadata: crate::nexus_gateway::GwMetadata {
                feature: "project_analyzer".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let wire = serde_json::to_value(&req).expect("serializza GwRequest");
        assert_eq!(wire["model"], "openai/gpt-4.1-mini");
        assert_eq!(wire["pin_provider"], "openai");
        assert_eq!(wire["max_tokens"], 8000);
        assert_eq!(wire["metadata"]["feature"], "project_analyzer");
        assert_eq!(wire["messages"][0]["role"], "user");
        assert_eq!(wire["messages"][0]["content"], "prompt analyzer");
    }
}
