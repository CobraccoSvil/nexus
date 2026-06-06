//! PR-3 sub-agents: handler MCP `dispatch_subagent`.
//!
//! Sostituisce il vecchio dispatch_subtask (rimosso in M55). Riabilita
//! il dispatch di sotto-agenti via brain LangGraph: chiama l'endpoint
//! REST `POST /agent/subagent-run` esposto dal brain (PR-3 step 2).
//!
//! Il tool e' guard-ato lato server:
//! - orchestrator.subagents_enabled deve essere true (lettura settings)
//! - kind in orchestrator.subagent_kinds_whitelist
//! - depth <= orchestrator.subagent_max_depth
//! - costo cumulativo < orchestrator.subagent_cost_cap_per_run_usd

use serde_json::{json, Value};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

use super::AgentToolContext;

pub async fn tool_dispatch_subagent(ctx: &AgentToolContext, input: &Value) -> String {
    // 1. Parse input
    let kind = match input.get("kind").and_then(Value::as_str) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return err("parametro 'kind' obbligatorio"),
    };
    let task = match input.get("task").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return err("parametro 'task' obbligatorio e non vuoto"),
    };
    let context_blob = input
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expected_format = input
        .get("expected_output_format")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    run_single_subagent(ctx, &kind, &task, &context_blob, &expected_format)
        .await
        .to_string()
}

/// Esegue UNA singola sub-run. Logica condivisa fra `dispatch_subagent`
/// (singolo) e `dispatch_subagents` (batch parallelo, Comp.0/3b).
///
/// Ritorna sempre un `Value`: l'oggetto risultato in caso di successo, oppure
/// `{"error": "..."}`. I guard (enabled/whitelist/depth/cost) sono valutati
/// per ogni sub-run; nel batch il cost cap e' best-effort (race tollerata
/// dato il cap conservativo sul parallelismo).
async fn run_single_subagent(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
) -> Value {
    let project_id = ctx.project_id;

    // 2. Setting guards (lettura diretta da DB via ctx.db)
    let (enabled, whitelist_csv, max_depth, cost_cap_usd, default_timeout): (
        bool,
        String,
        i64,
        f64,
        i64,
    ) = match read_subagent_settings(ctx).await {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("lettura settings fallita: {e}")}),
    };

    if !enabled {
        return json!({"error": "sub-agents disabilitati (orchestrator.subagents_enabled=false)"});
    }
    let whitelist: Vec<&str> = whitelist_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !whitelist.contains(&kind) {
        return json!({"error": format!("kind '{kind}' non in whitelist: {whitelist:?}")});
    }

    // 3. Carica definition kind
    let row = sqlx::query(
        "SELECT prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background, is_enabled
         FROM nexus_subagent_definitions WHERE kind = $1 LIMIT 1",
    )
    .bind(kind)
    .fetch_optional(&*ctx.db)
    .await
    .map_err(|e| format!("query definition: {e}"));
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return json!({"error": format!("kind '{kind}' non trovato in nexus_subagent_definitions")})
        }
        Err(e) => return json!({"error": e}),
    };
    let is_enabled: bool = row.get::<bool, _>("is_enabled");
    if !is_enabled {
        return json!({"error": format!("kind '{kind}' disabilitato")});
    }
    let timeout_s: i64 = row.get::<i32, _>("timeout_s") as i64;
    let timeout_s = if timeout_s > 0 {
        timeout_s
    } else {
        default_timeout
    };
    let is_background: bool = row.get::<bool, _>("is_background");

    // 4. Depth guard: leggi dal parent_run_id quanti sub-agent gia' sopra di noi.
    // Se ctx.parent_run_id is None siamo il main → depth=1 per il nuovo sub.
    let parent_run_id = ctx
        .parent_run_id
        .unwrap_or_else(|| ctx.session_id.unwrap_or(Uuid::nil()));
    let current_depth = if ctx.parent_run_id.is_some() {
        2_i64
    } else {
        1_i64
    };
    if current_depth > max_depth {
        return json!({"error": format!("depth {current_depth} > max {max_depth}: sub-agent annidamento eccessivo")});
    }

    // 5. Cost guard cumulativo per parent (cast NUMERIC -> double precision via SQL)
    let cumulative: f64 = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(cost_usd), 0)::double precision FROM nexus_subagent_runs WHERE parent_run_id = $1",
    )
    .bind(parent_run_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0.0);
    if cumulative >= cost_cap_usd {
        return json!({"error": format!("cost cap parent_run_id={parent_run_id} raggiunto ({cumulative:.4} >= {cost_cap_usd:.4})")});
    }

    // 6. Crea row in nexus_subagent_runs con status='pending'
    let subagent_run_id: Uuid = match sqlx::query_scalar(
        r#"INSERT INTO nexus_subagent_runs
           (parent_run_id, project_id, kind, task_description, context_blob, expected_format,
            status, is_background, depth, source)
           VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $8, 'db')
           RETURNING id"#,
    )
    .bind(parent_run_id)
    .bind(project_id)
    .bind(kind)
    .bind(task)
    .bind(context_blob)
    .bind(expected_format)
    .bind(is_background)
    .bind(current_depth as i32)
    .fetch_one(&*ctx.db)
    .await
    {
        Ok(id) => id,
        Err(e) => return json!({"error": format!("INSERT nexus_subagent_runs: {e}")}),
    };

    // 7. Chiama il brain endpoint /agent/subagent-run per attivare la sub-run.
    // L'endpoint e' bloccante per is_background=false, fire-and-forget per true.
    let brain_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://localhost:8001".to_string());
    let payload = json!({
        "subagent_run_id": subagent_run_id.to_string(),
        "parent_run_id":   parent_run_id.to_string(),
        "project_id":      project_id.to_string(),
        "user_id":         ctx.user_id.to_string(),
        "session_id":      ctx.session_id.map(|u| u.to_string()).unwrap_or_default(),
        "kind":            kind,
        "task":            task,
        "context":         context_blob,
        "expected_format": expected_format,
        "depth":           current_depth,
        "is_background":   is_background,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_s as u64 + 30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = client
        .post(format!("{brain_url}/agent/subagent-run"))
        .json(&payload)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r
                .json()
                .await
                .unwrap_or_else(|_| json!({"summary": "(no body)"}));
            // Ritorna compact summary al main
            let summary = body
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("(no summary)");
            let status = body
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            json!({
                "subagent_run_id": subagent_run_id.to_string(),
                "kind": body.get("kind").cloned().unwrap_or(json!(kind)),
                "status": status,
                "summary": summary,
                "artifacts": body.get("artifacts").cloned().unwrap_or(json!([])),
                "iterations": body.get("iterations").cloned().unwrap_or(json!(0)),
                "cost_usd": body.get("cost_usd").cloned().unwrap_or(json!(0)),
                "tokens": body.get("tokens").cloned().unwrap_or(json!({})),
            })
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            // Marca la sub-run come failed best-effort
            let _ = sqlx::query("UPDATE nexus_subagent_runs SET status = 'failed', completed_at = NOW(), final_summary = $1 WHERE id = $2")
                .bind(format!("[brain HTTP {status}]"))
                .bind(subagent_run_id)
                .execute(&*ctx.db)
                .await;
            json!({"error": format!("brain endpoint HTTP {status}: {}", body.chars().take(200).collect::<String>()), "subagent_run_id": subagent_run_id.to_string()})
        }
        Err(e) => {
            let _ = sqlx::query("UPDATE nexus_subagent_runs SET status = 'failed', completed_at = NOW(), final_summary = $1 WHERE id = $2")
                .bind(format!("[brain unreachable: {e}]"))
                .bind(subagent_run_id)
                .execute(&*ctx.db)
                .await;
            json!({"error": format!("brain endpoint unreachable: {e}"), "subagent_run_id": subagent_run_id.to_string()})
        }
    }
}

/// `dispatch_subagents` — esegue PIU' sub-run in parallelo (Comp.0/3b).
///
/// Input:
///   - `tasks`: array di {kind, task, context?, expected_output_format?} (1-8)
///   - `max_parallel`: ampiezza dell'ondata concorrente (default e tetto dal
///     setting admin `orchestrator.max_parallel_subagents`, hard cap 8)
///
/// Esegue a ondate di `max_parallel` via join_all (I/O-bound verso il brain).
/// E' la base del DAG scheduler parallelo (Comp.3b); i guard per-sub e il
/// cost cap restano quelli di `run_single_subagent`.
pub async fn tool_dispatch_subagents(ctx: &AgentToolContext, input: &Value) -> String {
    let tasks = match input.get("tasks").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return err("parametro 'tasks' (array non vuoto) obbligatorio"),
    };
    if tasks.len() > 8 {
        return err("troppi task in un batch (max 8)");
    }
    // Tetto effettivo dal setting admin (PUNTO UNICO, regola L): l'LLM puo'
    // chiedere un'ampiezza d'ondata via `max_parallel`, ma viene clampata al
    // massimo configurato in `orchestrator.max_parallel_subagents`. Prima qui
    // c'era un clamp hardcoded (1,4) che ignorava il setting admin: il valore
    // del pannello "Agenti Paralleli" non aveva quindi alcun effetto sul tool
    // diretto. `MAX_PARALLEL_HARD_CAP` resta come rete di sicurezza anti-runaway.
    let configured_max = read_max_parallel_subagents(ctx).await;
    let max_parallel = input
        .get("max_parallel")
        .and_then(|v| v.as_u64())
        .unwrap_or(configured_max)
        .clamp(1, configured_max) as usize;

    // Valida e normalizza ogni task prima di eseguire.
    let mut parsed: Vec<(String, String, String, String)> = Vec::with_capacity(tasks.len());
    for (i, t) in tasks.iter().enumerate() {
        let kind = t
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let task = t
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind.is_empty() || task.trim().is_empty() {
            return err(&format!("task[{i}]: 'kind' e 'task' sono obbligatori"));
        }
        let context_blob = t
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let expected = t
            .get("expected_output_format")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        parsed.push((kind, task, context_blob, expected));
    }

    // Esegui a ondate concorrenti (cap conservativo).
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed.chunks(max_parallel) {
        let futs = wave
            .iter()
            .map(|(k, ta, c, e)| run_single_subagent(ctx, k, ta, c, e));
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }

    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "results": results,
    })
    .to_string()
}

async fn read_subagent_settings(
    ctx: &AgentToolContext,
) -> Result<(bool, String, i64, f64, i64), String> {
    // Lettura blocco per minimizzare round-trip.
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.subagents_enabled',
            'orchestrator.subagent_kinds_whitelist',
            'orchestrator.subagent_max_depth',
            'orchestrator.subagent_cost_cap_per_run_usd',
            'orchestrator.subagent_default_timeout_s'
        )",
    )
    .fetch_all(&*ctx.db)
    .await
    .map_err(|e| format!("query settings: {e}"))?;

    let mut enabled = false;
    let mut whitelist = String::new();
    let mut max_depth: i64 = 2;
    let mut cost_cap: f64 = 5.0;
    let mut default_timeout: i64 = 300;
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        match k.as_str() {
            "orchestrator.subagents_enabled" => {
                enabled = matches!(
                    v.trim().to_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            }
            "orchestrator.subagent_kinds_whitelist" => whitelist = v,
            "orchestrator.subagent_max_depth" => max_depth = v.trim().parse().unwrap_or(2),
            "orchestrator.subagent_cost_cap_per_run_usd" => {
                cost_cap = v.trim().parse().unwrap_or(5.0)
            }
            "orchestrator.subagent_default_timeout_s" => {
                default_timeout = v.trim().parse().unwrap_or(300)
            }
            _ => {}
        }
    }
    Ok((enabled, whitelist, max_depth, cost_cap, default_timeout))
}

/// Tetto massimo di sicurezza per l'ampiezza dell'ondata concorrente di
/// sub-agenti. Il valore admin `orchestrator.max_parallel_subagents` puo'
/// arrivare fino a qui; oltre e' considerato runaway e viene clampato.
const MAX_PARALLEL_HARD_CAP: u64 = 8;

/// Legge `orchestrator.max_parallel_subagents` (default 3) come tetto effettivo
/// del parallelismo dei sub-agenti. PUNTO UNICO condiviso col DAG scheduler
/// Python e col pannello admin "Agenti Paralleli".
async fn read_max_parallel_subagents(ctx: &AgentToolContext) -> u64 {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'orchestrator.max_parallel_subagents'")
            .fetch_optional(&*ctx.db)
            .await
            .ok()
            .flatten();
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, MAX_PARALLEL_HARD_CAP)
}

fn err(msg: &str) -> String {
    format!("\u{274C} [dispatch_subagent] {msg}")
}

/// PR-3: tool `nexus_subagent_poll` — leggi lo stato di un sub-agent run.
/// Usato dal main quando ha invocato un sub-agent background.
pub async fn tool_nexus_subagent_poll(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id = match input.get("subagent_run_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return format!(
                "\u{274C} [nexus_subagent_poll] parametro 'subagent_run_id' obbligatorio"
            )
        }
    };
    let row = sqlx::query(
        "SELECT id::text, status, kind, final_summary, artifacts, iterations,
                tokens_prompt, tokens_completion, cost_usd, depth, source, is_background
         FROM nexus_subagent_runs WHERE id::text = $1",
    )
    .bind(&run_id)
    .fetch_optional(&*ctx.db)
    .await;
    match row {
        Ok(Some(r)) => {
            let summary = json!({
                "subagent_run_id": r.try_get::<String, _>("id").unwrap_or_default(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "summary": r.try_get::<Option<String>, _>("final_summary").unwrap_or(None),
                "artifacts": r.try_get::<Option<Vec<String>>, _>("artifacts").unwrap_or(None).unwrap_or_default(),
                "iterations": r.try_get::<i32, _>("iterations").unwrap_or(0),
                "tokens": {
                    "prompt": r.try_get::<i32, _>("tokens_prompt").unwrap_or(0),
                    "completion": r.try_get::<i32, _>("tokens_completion").unwrap_or(0),
                },
                "cost_usd": r.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
                "depth": r.try_get::<i32, _>("depth").unwrap_or(1),
                "source": r.try_get::<String, _>("source").unwrap_or_else(|_| "db".into()),
                "is_background": r.try_get::<bool, _>("is_background").unwrap_or(false),
            });
            summary.to_string()
        }
        Ok(None) => format!("\u{274C} [nexus_subagent_poll] sub-agent run '{run_id}' non trovato"),
        Err(e) => format!("\u{274C} [nexus_subagent_poll] query fallita: {e}"),
    }
}

/// PR-3: tool `nexus_subagent_resume` — riprendi un sub-agent paused/background.
pub async fn tool_nexus_subagent_resume(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id = match input.get("subagent_run_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return format!(
                "\u{274C} [nexus_subagent_resume] parametro 'subagent_run_id' obbligatorio"
            )
        }
    };
    // Best-effort: aggiorna lo stato a 'running' e chiama l'endpoint brain
    // /agent/subagent-resume per re-dispatcharne l'esecuzione.
    let upd = sqlx::query(
        "UPDATE nexus_subagent_runs SET status='running' WHERE id::text = $1 AND status IN ('paused','running','timeout')"
    )
    .bind(&run_id)
    .execute(&*ctx.db)
    .await;
    if let Err(e) = upd {
        return format!("\u{274C} [nexus_subagent_resume] update fallito: {e}");
    }
    // Notifica al brain (best-effort).
    let brain_url =
        std::env::var("NEURAL_REST_URL").unwrap_or_else(|_| "http://localhost:8001".into());
    let resp = reqwest::Client::new()
        .post(format!("{brain_url}/agent/subagent-resume"))
        .json(&json!({"run_id": run_id}))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            format!("{{\"status\":\"resumed\",\"subagent_run_id\":\"{run_id}\"}}")
        }
        Ok(r) => format!(
            "{{\"status\":\"partial\",\"subagent_run_id\":\"{run_id}\",\"brain_status\":{}}}",
            r.status().as_u16()
        ),
        Err(e) => format!(
            "{{\"status\":\"db_updated_brain_unreachable\",\"subagent_run_id\":\"{run_id}\",\"error\":\"{}\"}}",
            e.to_string().replace('"', "'")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::err;

    #[test]
    fn err_format() {
        let m = err("boom");
        assert!(m.starts_with('\u{274C}'));
        assert!(m.contains("dispatch_subagent"));
    }
}
