// Deep review tramite Gemini Batch API: submit, get status, parse issues.

use super::*;

/// Avvia un batch job Gemini per analisi approfondita dei file del progetto
pub async fn submit_deep_review(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> Result<Json<Value>, ApiError> {
    // Verifica ownership
    let row = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', ''), p.owner_user_id FROM projects p LEFT JOIN repositories r ON r.project_id = p.id WHERE p.id = $1"
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (root_path, owner_id) = row;
    let caller_id = parse_user_id(&claims).map_err(|e| {
        api_error(
            StatusCode::UNAUTHORIZED,
            e.1 .0["error"]
                .as_str()
                .unwrap_or("Unauthorized")
                .to_string(),
        )
    })?;
    if owner_id != Some(caller_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Access denied".to_string(),
        ));
    }

    // Verifica che batch API sia abilitata
    let batch_enabled: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'google_batch_api_enabled'")
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .unwrap_or_else(|| "false".to_string());

    if batch_enabled != "true" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Google Batch API non abilitata. Attivala nelle impostazioni.".to_string(),
        ));
    }

    // Carica chiave API e modello
    let api_key: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'google_api_key'")
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .unwrap_or_default();

    if api_key.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "google_api_key non configurata".to_string(),
        ));
    }

    let model_setting: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'google_batch_model'")
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let batch_model: String = match model_setting {
        Some(m) => m,
        None => {
            // Risolto dal PUNTO UNICO tier-only (regola L/G): niente fallback su
            // default_model. Errore esplicito se il tier non risolve.
            let (_prov, model) =
                crate::internal_routing::resolve_purpose_model(&state, "google_batch")
                    .await
                    .into_model("google_batch")
                    .map_err(|m| api_error(StatusCode::SERVICE_UNAVAILABLE, m))?;
            model
        }
    };

    let threshold: usize = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'google_batch_threshold'",
    )
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| "5".to_string())
    .parse::<usize>()
    .unwrap_or(5);

    const MAX_FILE_BYTES: u64 = 50 * 1024;

    let source_files = collect_source_files(&root_path, CODE_EXTENSIONS);

    // Costruisce la lista file con contenuto
    let mut files_payload: Vec<serde_json::Value> = Vec::new();
    for abs_path in &source_files {
        if let Ok(meta) = std::fs::metadata(abs_path) {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        } else {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(abs_path) else {
            continue;
        };
        let rel = std::path::Path::new(abs_path)
            .strip_prefix(&root_path)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| abs_path.clone());
        files_payload.push(json!({"path": rel, "content": content}));
    }

    if files_payload.len() < threshold {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Solo {} file trovati, soglia minima e' {}. Usa la scansione normale.",
                files_payload.len(),
                threshold
            ),
        ));
    }

    let file_count = files_payload.len();

    // Genera job_id e salva stato iniziale in Redis
    let job_id = Uuid::new_v4().to_string();
    let redis_key = format!("deep_review:{}", job_id);
    let _ = redis::cmd("SET")
        .arg(&redis_key)
        .arg(
            serde_json::to_string(&json!({
                "state": "JOB_STATE_PENDING",
                "completed": 0,
                "total": file_count,
            }))
            .unwrap_or_default(),
        )
        .arg("EX")
        .arg(86400u64)
        .query_async::<()>(&mut state.redis.clone())
        .await;

    // Task background — processa file via generateContent
    let mut redis_bg = state.redis.clone();
    let api_key_bg = api_key.clone();
    let model_bg = batch_model.clone();
    let _job_id_bg = job_id.clone();
    let redis_key_bg = redis_key.clone();
    let system_prompt_owned = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "quality.deep_review_code_analysis",
    )
    .await;

    tokio::spawn(async move {
        use futures::stream::{self, StreamExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        const BATCH_SIZE: usize = 8;
        const CONCURRENCY: usize = 2;
        const MAX_RETRIES: usize = 3;

        let http_client = nexus_http::build_client_with_config(&nexus_http::NexusHttpConfig {
            timeout_secs: 180,
            pool_max: 4,
            pool_idle_timeout_secs: 120,
            proxy: std::env::var("NEXUS_PROXY").ok().filter(|v| !v.is_empty()),
        });
        let url = Arc::new(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model_bg, api_key_bg
        ));
        let system_prompt = Arc::new(system_prompt_owned);

        // Aggiorna stato a RUNNING
        let _ = redis::cmd("SET")
            .arg(&redis_key_bg)
            .arg(
                serde_json::to_string(
                    &json!({"state":"JOB_STATE_RUNNING","completed":0,"total":file_count}),
                )
                .unwrap_or_default(),
            )
            .arg("EX")
            .arg(86400u64)
            .query_async::<()>(&mut redis_bg)
            .await;

        let completed = Arc::new(AtomicUsize::new(0));
        let all_results = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
        let failed = Arc::new(tokio::sync::Mutex::new(Option::<String>::None));

        let chunks: Vec<Vec<Value>> = files_payload
            .chunks(BATCH_SIZE)
            .map(|c| c.to_vec())
            .collect();

        stream::iter(chunks)
            .for_each_concurrent(CONCURRENCY, |chunk| {
                let http = http_client.clone();
                let url = Arc::clone(&url);
                let sp = Arc::clone(&system_prompt);
                let completed = Arc::clone(&completed);
                let all_results = Arc::clone(&all_results);
                let failed = Arc::clone(&failed);
                let redis_key = redis_key_bg.clone();
                let mut redis_conn = redis_bg.clone();
                let chunk_len = chunk.len();

                async move {
                    if failed.lock().await.is_some() { return; }

                    let mut chunk_prompt = format!("{}\n\n", sp.as_str());
                    for f in &chunk {
                        let path = f["path"].as_str().unwrap_or("");
                        let content = f["content"].as_str().unwrap_or("");
                        chunk_prompt.push_str(&format!(
                            "=== {} ===\n{}\n\n",
                            path, &content[..content.len().min(4000)]
                        ));
                    }

                    let body = json!({
                        "contents": [{"role":"user","parts":[{"text": chunk_prompt}]}],
                        "generationConfig": {"temperature": 0.1}
                    });

                    // Retry loop con exponential backoff per rate limiting
                    let resp_json: Value = {
                        let mut attempt = 0;
                        let mut last_err = String::new();
                        loop {
                            if attempt >= MAX_RETRIES {
                                *failed.lock().await = Some(last_err.clone());
                                let _ = redis::cmd("SET").arg(&redis_key)
                                    .arg(serde_json::to_string(&json!({"state":"JOB_STATE_FAILED","error":last_err})).unwrap_or_default())
                                    .arg("EX").arg(3600u64).query_async::<()>(&mut redis_conn).await;
                                return;
                            }
                            if attempt > 0 {
                                let delay_secs = [2u64, 8, 30][attempt.min(2)];
                                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                            }
                            attempt += 1;

                            let resp = match http.post(url.as_str()).json(&body).send().await {
                                Ok(r) => r,
                                Err(e) => {
                                    last_err = e.to_string();
                                    continue;
                                }
                            };

                            let status = resp.status();
                            if status.as_u16() == 429 || status.as_u16() >= 500 {
                                last_err = format!("HTTP {} - rate limit or server error", status.as_u16());
                                continue;
                            }
                            if !status.is_success() {
                                let err = resp.text().await.unwrap_or_default();
                                *failed.lock().await = Some(err.clone());
                                let _ = redis::cmd("SET").arg(&redis_key)
                                    .arg(serde_json::to_string(&json!({"state":"JOB_STATE_FAILED","error":format!("API error {}: {}", status.as_u16(), err)})).unwrap_or_default())
                                    .arg("EX").arg(3600u64).query_async::<()>(&mut redis_conn).await;
                                return;
                            }
                            match resp.json::<Value>().await {
                                Ok(v) => break v,
                                Err(e) => {
                                    last_err = e.to_string();
                                    continue;
                                }
                            }
                        }
                    };

                    let text = resp_json["candidates"][0]["content"]["parts"][0]["text"]
                        .as_str().unwrap_or("[]");
                    let clean = text.trim()
                        .trim_start_matches("```json")
                        .trim_start_matches("```")
                        .trim_end_matches("```")
                        .trim();

                    if let Ok(chunk_results) = serde_json::from_str::<Vec<Value>>(clean) {
                        all_results.lock().await.extend(chunk_results);
                    }

                    let done = completed.fetch_add(chunk_len, Ordering::Relaxed) + chunk_len;

                    let _ = redis::cmd("SET").arg(&redis_key)
                        .arg(serde_json::to_string(&json!({
                            "state": "JOB_STATE_RUNNING",
                            "completed": done,
                            "total": file_count,
                        })).unwrap_or_default())
                        .arg("EX").arg(86400u64).query_async::<()>(&mut redis_conn).await;
                }
            })
            .await;

        if let Some(err) = failed.lock().await.as_ref() {
            tracing::warn!("Deep review failed: {}", err);
            return;
        }

        let final_results = all_results.lock().await.clone();
        let _ = redis::cmd("SET")
            .arg(&redis_key_bg)
            .arg(
                serde_json::to_string(&json!({
                    "state": "JOB_STATE_SUCCEEDED",
                    "completed": file_count,
                    "total": file_count,
                    "results": final_results,
                }))
                .unwrap_or_default(),
            )
            .arg("EX")
            .arg(86400u64)
            .query_async::<()>(&mut redis_bg)
            .await;
    });

    Ok(Json(json!({
        "jobName": job_id,
        "jobId": job_id,
        "fileCount": file_count,
        "status": "submitted"
    })))
}

pub async fn get_deep_review_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, job_id)): AxumPath<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', ''), p.owner_user_id FROM projects p LEFT JOIN repositories r ON r.project_id = p.id WHERE p.id = $1"
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (_root_path, owner_id) = row;
    let caller_id = parse_user_id(&claims).map_err(|e| {
        api_error(
            StatusCode::UNAUTHORIZED,
            e.1 .0["error"]
                .as_str()
                .unwrap_or("Unauthorized")
                .to_string(),
        )
    })?;
    if owner_id != Some(caller_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Access denied".to_string(),
        ));
    }

    // Legge stato job da Redis
    let redis_key = format!("deep_review:{}", job_id);
    let cached: Option<String> = redis::cmd("GET")
        .arg(&redis_key)
        .query_async(&mut state.redis.clone())
        .await
        .unwrap_or(None);

    let job_data: Value = cached
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(
            || json!({"state": "JOB_STATE_NOT_FOUND", "error": "Job not found or expired"}),
        );

    let state_str = job_data["state"].as_str().unwrap_or("UNKNOWN");
    let completed = job_data["completed"].as_i64().unwrap_or(0);
    let total = job_data["total"].as_i64().unwrap_or(0);

    if state_str == "JOB_STATE_FAILED" {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            job_data["error"]
                .as_str()
                .unwrap_or("Job failed")
                .to_string(),
        ));
    }

    if state_str == "JOB_STATE_SUCCEEDED" {
        return Ok(Json(json!({
            "state": state_str,
            "completed": completed,
            "total": total,
            "results": job_data["results"],
        })));
    }

    Ok(Json(json!({
        "state": state_str,
        "completed": completed,
        "total": total,
    })))
}

#[allow(dead_code)]
pub(super) fn parse_issues_from_response(text: &str) -> Vec<Value> {
    let start = text.find('{');
    let end = text.rfind('}').map(|i| i + 1);
    if let (Some(s), Some(e)) = (start, end) {
        if e > s {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text[s..e]) {
                if let Some(arr) = parsed["issues"].as_array() {
                    return arr.clone();
                }
            }
        }
    }
    vec![]
}
