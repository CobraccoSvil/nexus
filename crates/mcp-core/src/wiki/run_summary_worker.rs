// ═══════════════════════════════════════════════════════════════════════════
// wiki/run_summary_worker.rs — ADR 0017 v2 TODO 7.
//
// Worker periodico che ingesta i resoconti degli `agent_runs` terminali come
// wiki_doc (scope='project', kind='run_summary'). Scopo: "memoria episodica" —
// nei run successivi l'agente puo' richiamare cosa e' stato fatto su quel
// progetto, con quali tool, con quale esito.
//
// Idempotenza: ogni run processato e' marcato con `agent_runs.kb_ingested`
// (mig 0304). Il worker scansiona solo righe con `kb_ingested IS NULL` e
// `status IN ('completed','failed','aborted')`.
//
// Settings DB-driven (mig 0305):
//   - agent.wiki.run_summary_worker_enabled       (default true)
//   - agent.wiki.run_summary_worker_interval_secs (default 60)
//   - agent.wiki.run_summary_max_per_minute       (default 30)
//
// Privacy: il body include lo status, il provider/modello, l'iteration_count,
// e — quando presente — `final_answer` troncato. Niente prompt o tool_input
// in chiaro (regola F): solo i tool name + status del singolo step.
// ═══════════════════════════════════════════════════════════════════════════

use crate::AppState;
use anyhow::{Context, Result};
use serde_json::json;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct RunSummarySettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub max_per_minute: usize,
}

impl RunSummarySettings {
    const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            interval_secs: 60,
            max_per_minute: 30,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(RunSummarySettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> RunSummarySettings {
    {
        let guard = SETTINGS_CACHE.read().await;
        if let Some((v, exp)) = *guard {
            if Instant::now() < exp {
                return v;
            }
        }
    }
    let value = match load_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "wiki.run_summary: lettura settings fallita, uso safe_defaults"
            );
            RunSummarySettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value, Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<RunSummarySettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.run_summary_worker_enabled', \
            'agent.wiki.run_summary_worker_interval_secs', \
            'agent.wiki.run_summary_max_per_minute' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.run_summary_*")?;

    let mut out = RunSummarySettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        match key.as_str() {
            "agent.wiki.run_summary_worker_enabled" => {
                out.enabled = matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
            "agent.wiki.run_summary_worker_interval_secs" => {
                if let Ok(v) = raw.trim().parse::<u64>() {
                    out.interval_secs = v.max(10);
                }
            }
            "agent.wiki.run_summary_max_per_minute" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.max_per_minute = v.max(1);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Entry-point
// ───────────────────────────────────────────────────────────────────────────

pub fn start_run_summary_worker(state: Arc<AppState>) {
    tokio::spawn(async move {
        // Delay iniziale maggiore (90s) per non sovrapporsi con il chat-note worker.
        tokio::time::sleep(Duration::from_secs(90)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            "wiki.run_summary: worker avviato"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }
            match scan_and_ingest(&state, &settings).await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(
                            ingested = n,
                            "wiki.run_summary: batch completato"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.run_summary: batch fallito");
                }
            }
            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

async fn scan_and_ingest(state: &AppState, settings: &RunSummarySettings) -> Result<usize> {
    let rows = sqlx::query(
        r#"
        SELECT ar.id, ar.project_id, ar.session_id, ar.status, ar.provider, ar.model,
               ar.iteration_count, ar.final_answer, ar.created_at, ar.completed_at
        FROM agent_runs ar
        JOIN projects p ON p.id = ar.project_id
        WHERE ar.kb_ingested IS NULL
          AND ar.status IN ('completed', 'failed', 'aborted')
          AND ar.completed_at IS NOT NULL
        ORDER BY ar.completed_at ASC
        LIMIT $1
        "#,
    )
    .bind(settings.max_per_minute as i64)
    .fetch_all(&state.db)
    .await
    .context("SELECT agent_runs pending kb_ingest")?;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut ingested = 0usize;
    for row in rows {
        let run_id: Uuid = match row.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let project_id: Uuid = match row.try_get("project_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let session_id: Uuid = match row.try_get("session_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let status: String = row.try_get("status").unwrap_or_default();
        let provider: Option<String> = row.try_get("provider").ok();
        let model: Option<String> = row.try_get("model").ok();
        let iteration_count: i32 = row.try_get("iteration_count").unwrap_or(0);
        let final_answer: Option<String> = row.try_get("final_answer").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now());
        let completed_at: chrono::DateTime<chrono::Utc> = row
            .try_get("completed_at")
            .unwrap_or_else(|_| chrono::Utc::now());

        match ingest_run(
            state,
            run_id,
            project_id,
            session_id,
            &status,
            provider.as_deref(),
            model.as_deref(),
            iteration_count,
            final_answer.as_deref(),
            created_at,
            completed_at,
        )
        .await
        {
            Ok(_) => {
                ingested += 1;
                mark_processed(&state.db, run_id).await;
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "wiki.run_summary: ingest_run fallito (retry al prossimo giro)"
                );
            }
        }
    }
    Ok(ingested)
}

#[allow(clippy::too_many_arguments)]
async fn ingest_run(
    state: &AppState,
    run_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    status: &str,
    provider: Option<&str>,
    model: Option<&str>,
    iteration_count: i32,
    final_answer: Option<&str>,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    // Recupera la lista dei tool usati (solo name + status, niente input/output
    // in chiaro per la regola F sui log sensibili).
    let tool_rows = sqlx::query(
        "SELECT tool_name, status FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(&state.db)
    .await
    .context("SELECT agent_steps per run_summary")?;

    let mut tool_lines: Vec<String> = Vec::with_capacity(tool_rows.len());
    for tr in tool_rows.iter().take(50) {
        let name: String = tr.try_get("tool_name").unwrap_or_default();
        let st: String = tr.try_get("status").unwrap_or_default();
        if !name.is_empty() {
            tool_lines.push(format!("- `{name}` ({st})"));
        }
    }
    if tool_rows.len() > 50 {
        tool_lines.push(format!("- ... e altri {} step omessi", tool_rows.len() - 50));
    }
    let tools_section = if tool_lines.is_empty() {
        "_Nessun tool registrato._".to_string()
    } else {
        tool_lines.join("\n")
    };

    let final_section = match final_answer {
        Some(s) if !s.trim().is_empty() => {
            // Cap a 4000 char per evitare doc enormi (i payload reali raramente
            // superano questa soglia ma capita per output strutturati).
            let truncated: String = s.chars().take(4000).collect();
            if s.chars().count() > 4000 {
                format!("{truncated}\n\n_[troncato: output originale ({} char)]_", s.chars().count())
            } else {
                truncated
            }
        }
        _ => "_Nessun output finale registrato._".to_string(),
    };

    let title = format!(
        "Run agent del {} ({status})",
        completed_at.format("%Y-%m-%d %H:%M")
    );
    let slug = format!("run-{}", run_id.simple());
    let duration_secs = (completed_at - created_at).num_seconds().max(0);

    let body_md = format!(
        "# Riepilogo run\n\n\
         **Run ID**: `{run_id}`\n\
         **Sessione**: `{session_id}`\n\
         **Stato**: {status}\n\
         **Provider**: {provider}\n\
         **Modello**: {model}\n\
         **Iterazioni**: {iters}\n\
         **Iniziato**: {start}\n\
         **Completato**: {end}\n\
         **Durata**: {duration}s\n\n\
         ## Tool usati\n\n{tools}\n\n\
         ## Output finale\n\n{final}\n",
        run_id = run_id,
        session_id = session_id,
        status = status,
        provider = provider.unwrap_or("(n/d)"),
        model = model.unwrap_or("(n/d)"),
        iters = iteration_count,
        start = created_at.to_rfc3339(),
        end = completed_at.to_rfc3339(),
        duration = duration_secs,
        tools = tools_section,
        final = final_section,
    );
    let body_hash = crate::wiki::vault::sha256_hex(&body_md);

    // Embed (best-effort).
    let snippet = if body_md.len() > 2000 {
        &body_md[..2000]
    } else {
        body_md.as_str()
    };
    let combined = format!("{title}\n\n{snippet}");
    let qdrant_point_id: Option<String> =
        match state.orchestrator.neural.embed_text("", &combined).await {
            Ok(vector) => {
                let doc_uuid = Uuid::new_v4();
                let point_id = doc_uuid.to_string();
                let payload = json!({
                    "scope": "project",
                    "project_id": project_id.to_string(),
                    "doc_id": point_id,
                    "title": title.clone(),
                    "kind": "run_summary",
                    "session_id": session_id.to_string(),
                    "status": status,
                });
                match crate::vector_memory::upsert_wiki_content_point(
                    &state.db, &point_id, vector, payload,
                )
                .await
                {
                    Ok(_) => Some(point_id),
                    Err(e) => {
                        tracing::debug!(
                            run_id = %run_id,
                            error = %e,
                            "wiki.run_summary: upsert Qdrant fallito (proseguo)"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id,
                    error = %e,
                    "wiki.run_summary: embed_text fallito (proseguo senza vector)"
                );
                None
            }
        };

    let tags: Vec<String> = vec![
        "run_summary".to_string(),
        "auto".to_string(),
        format!("status:{status}"),
    ];

    sqlx::query(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, tags, qdrant_point_id, edited_by,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            'run_summary', $6, $7, 'agent_run',
            'none', '{}', FALSE,
            $5, NULL,
            1, TRUE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            body_md         = EXCLUDED.body_md,
            body_hash       = EXCLUDED.body_hash,
            tags            = EXCLUDED.tags,
            qdrant_point_id = COALESCE(EXCLUDED.qdrant_point_id, wiki_docs.qdrant_point_id),
            updated_at      = NOW()
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(&tags)
    .bind(qdrant_point_id.as_deref())
    .execute(&state.db)
    .await
    .context("INSERT wiki_docs run_summary")?;

    Ok(())
}

async fn mark_processed(db: &PgPool, run_id: Uuid) {
    if let Err(e) =
        sqlx::query("UPDATE agent_runs SET kb_ingested = TRUE WHERE id = $1 AND kb_ingested IS NULL")
            .bind(run_id)
            .execute(db)
            .await
    {
        tracing::debug!(
            run_id = %run_id,
            error = %e,
            "wiki.run_summary: mark_processed fallito"
        );
    }
}
