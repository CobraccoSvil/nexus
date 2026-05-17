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
    let context_blob = input.get("context").and_then(Value::as_str).unwrap_or("").to_string();
    let expected_format = input.get("expected_output_format").and_then(Value::as_str).unwrap_or("").to_string();

    let project_id = ctx.project_id;

    // 2. Setting guards (lettura diretta da DB via ctx.db)
    let (enabled, whitelist_csv, max_depth, cost_cap_usd, default_timeout): (bool, String, i64, f64, i64) = match read_subagent_settings(ctx).await {
        Ok(v) => v,
        Err(e) => return err(&format!("lettura settings fallita: {e}")),
    };

    if !enabled {
        return err("sub-agents disabilitati (orchestrator.subagents_enabled=false)");
    }
    let whitelist: Vec<&str> = whitelist_csv.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if !whitelist.contains(&kind.as_str()) {
        return err(&format!("kind '{kind}' non in whitelist: {whitelist:?}"));
    }

    // 3. Carica definition kind
    let row = sqlx::query(
        "SELECT prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background, is_enabled
         FROM nexus_subagent_definitions WHERE kind = $1 LIMIT 1",
    )
    .bind(&kind)
    .fetch_optional(&*ctx.db)
    .await
    .map_err(|e| format!("query definition: {e}"));
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => return err(&format!("kind '{kind}' non trovato in nexus_subagent_definitions")),
        Err(e) => return err(&e),
    };
    let is_enabled: bool = row.get::<bool, _>("is_enabled");
    if !is_enabled {
        return err(&format!("kind '{kind}' disabilitato"));
    }
    let timeout_s: i64 = row.get::<i32, _>("timeout_s") as i64;
    let timeout_s = if timeout_s > 0 { timeout_s } else { default_timeout };
    let is_background: bool = row.get::<bool, _>("is_background");

    // 4. Depth guard: leggi dal parent_run_id quanti sub-agent gia' sopra di noi.
    // Se ctx.parent_run_id is None siamo il main → depth=1 per il nuovo sub.
    let parent_run_id = ctx.parent_run_id.unwrap_or_else(|| ctx.session_id.unwrap_or(Uuid::nil()));
    let current_depth = if ctx.parent_run_id.is_some() { 2_i64 } else { 1_i64 };
    if current_depth > max_depth {
        return err(&format!("depth {current_depth} > max {max_depth}: sub-agent annidamento eccessivo"));
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
        return err(&format!("cost cap parent_run_id={parent_run_id} raggiunto ({cumulative:.4} >= {cost_cap_usd:.4})"));
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
    .bind(&kind)
    .bind(&task)
    .bind(&context_blob)
    .bind(&expected_format)
    .bind(is_background)
    .bind(current_depth as i32)
    .fetch_one(&*ctx.db)
    .await
    {
        Ok(id) => id,
        Err(e) => return err(&format!("INSERT nexus_subagent_runs: {e}")),
    };

    // 7. Chiama il brain endpoint /agent/subagent-run per attivare la sub-run.
    // L'endpoint e' bloccante per is_background=false, fire-and-forget per true.
    let brain_url = std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://localhost:8001".to_string());
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
            let body: serde_json::Value = r.json().await.unwrap_or_else(|_| json!({"summary": "(no body)"}));
            // Ritorna compact summary al main
            let summary = body.get("summary").and_then(Value::as_str).unwrap_or("(no summary)");
            let status = body.get("status").and_then(Value::as_str).unwrap_or("completed");
            json!({
                "subagent_run_id": subagent_run_id.to_string(),
                "kind": body.get("kind").cloned().unwrap_or(json!(null)),
                "status": status,
                "summary": summary,
                "artifacts": body.get("artifacts").cloned().unwrap_or(json!([])),
                "iterations": body.get("iterations").cloned().unwrap_or(json!(0)),
                "cost_usd": body.get("cost_usd").cloned().unwrap_or(json!(0)),
                "tokens": body.get("tokens").cloned().unwrap_or(json!({})),
            })
            .to_string()
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
            err(&format!("brain endpoint HTTP {status}: {}", body.chars().take(200).collect::<String>()))
        }
        Err(e) => {
            let _ = sqlx::query("UPDATE nexus_subagent_runs SET status = 'failed', completed_at = NOW(), final_summary = $1 WHERE id = $2")
                .bind(format!("[brain unreachable: {e}]"))
                .bind(subagent_run_id)
                .execute(&*ctx.db)
                .await;
            err(&format!("brain endpoint unreachable: {e}"))
        }
    }
}

async fn read_subagent_settings(ctx: &AgentToolContext) -> Result<(bool, String, i64, f64, i64), String> {
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
            "orchestrator.subagents_enabled" => enabled = matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on"),
            "orchestrator.subagent_kinds_whitelist" => whitelist = v,
            "orchestrator.subagent_max_depth" => max_depth = v.trim().parse().unwrap_or(2),
            "orchestrator.subagent_cost_cap_per_run_usd" => cost_cap = v.trim().parse().unwrap_or(5.0),
            "orchestrator.subagent_default_timeout_s" => default_timeout = v.trim().parse().unwrap_or(300),
            _ => {}
        }
    }
    Ok((enabled, whitelist, max_depth, cost_cap, default_timeout))
}

fn err(msg: &str) -> String {
    format!("\u{274C} [dispatch_subagent] {msg}")
}

/// PR-3: tool `nexus_subagent_poll` — leggi lo stato di un sub-agent run.
/// Usato dal main quando ha invocato un sub-agent background.
pub async fn tool_nexus_subagent_poll(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id = match input.get("subagent_run_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return format!("\u{274C} [nexus_subagent_poll] parametro 'subagent_run_id' obbligatorio"),
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
        _ => return format!("\u{274C} [nexus_subagent_resume] parametro 'subagent_run_id' obbligatorio"),
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
    let brain_url = std::env::var("NEURAL_REST_URL").unwrap_or_else(|_| "http://localhost:8001".into());
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
