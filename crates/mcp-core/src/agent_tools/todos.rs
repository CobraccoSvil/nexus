//! PR-1 Plan/Act/Verify: handler MCP `nexus_todo_write`.
//!
//! Tool riservato al planner_node (e in casi limitati all'executor per
//! aggiornare lo status di un singolo todo). Persiste su `nexus_agent_plans`
//! + `nexus_agent_todos` con isolation per project_id.
//!
//! Schema input:
//! {
//!   "action": "create" | "check" | "add" | "update",
//!   "run_id": "uuid",                  // obbligatorio: run_id dell'agent_run corrente
//!   "todos": [
//!     {
//!       "id": "uuid",                  // opzionale per "create"/"add", obbligatorio per "check"/"update"
//!       "seq": int,                    // opzionale, derivato per "add"
//!       "content": "string",
//!       "status": "pending"|"in_progress"|"completed"|"blocked"|"skipped",
//!       "priority": "high"|"normal"|"low",
//!       "acceptance_criteria": [...]   // opzionale, array di check spec JSON
//!     }
//!   ],
//!   "planner_model": "string"          // opzionale, per action=create (default 'unknown')
//! }
//!
//! Output: JSON con `{ok, action, affected, plan_id, todo_ids[]}`.

use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::AgentToolContext;

/// Stati ammessi (mirror del CHECK constraint in mig 0148).
const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed", "blocked", "skipped"];
const VALID_PRIORITIES: &[&str] = &["high", "normal", "low"];

pub async fn tool_nexus_todo_write(ctx: &AgentToolContext, input: &Value) -> String {
    // 1. Parse e validazione input.
    let action = match input.get("action").and_then(Value::as_str) {
        Some(a) => a,
        None => return err("parametro 'action' obbligatorio (create|check|add|update)"),
    };
    if !matches!(action, "create" | "check" | "add" | "update") {
        return err(&format!("action '{action}' non valida (create|check|add|update)"));
    }

    let run_id = match input.get("run_id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) {
        Some(r) => r,
        None => return err("parametro 'run_id' obbligatorio (uuid dell'agent_run corrente)"),
    };

    let todos_in = match input.get("todos").and_then(Value::as_array) {
        Some(t) if !t.is_empty() => t,
        Some(_) => return err("parametro 'todos' vuoto: passa almeno un todo"),
        None => return err("parametro 'todos' obbligatorio (array)"),
    };

    let project_id = ctx.project_id;

    // 2. Verifica che il run_id appartenga al project_id (isolation multi-tenant).
    let run_check: Option<(Uuid,)> = sqlx::query_as(
        "SELECT project_id FROM agent_runs WHERE id = $1 LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten();
    match run_check {
        Some((p,)) if p == project_id => {} // OK
        Some(_) => return err("run_id non appartiene al project_id corrente (isolation violation)"),
        None => return err(&format!("run_id {run_id} non trovato in agent_runs")),
    };

    // 3. Dispatch per action.
    match action {
        "create" => create_plan(ctx, run_id, project_id, todos_in, input).await,
        "check" => update_status(ctx, run_id, project_id, todos_in, true).await,
        "add" => add_todos(ctx, run_id, project_id, todos_in).await,
        "update" => update_status(ctx, run_id, project_id, todos_in, false).await,
        _ => unreachable!(),
    }
}

async fn create_plan(
    ctx: &AgentToolContext,
    run_id: Uuid,
    project_id: Uuid,
    todos_in: &[Value],
    input: &Value,
) -> String {
    let planner_model = input.get("planner_model").and_then(Value::as_str).unwrap_or("unknown");

    // Ricava thread_id (sessione) per indicizzazione.
    let thread_id = ctx
        .session_id
        .map(|u| u.to_string())
        .unwrap_or_else(|| run_id.to_string());

    // Acceptance criteria globali del plan: estratti dal payload se presenti.
    let plan_acceptance = input
        .get("plan_acceptance_criteria")
        .cloned()
        .unwrap_or_else(|| json!([]));

    let mut tx = match ctx.db.begin().await {
        Ok(t) => t,
        Err(e) => return err(&format!("begin tx fallita: {e}")),
    };

    // Upsert del plan (PRIMARY KEY = run_id, quindi ON CONFLICT su run_id).
    let plan_res = sqlx::query(
        r#"INSERT INTO nexus_agent_plans (run_id, project_id, thread_id, acceptance_criteria, planner_model)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (run_id) DO UPDATE SET
             acceptance_criteria = EXCLUDED.acceptance_criteria,
             planner_model = EXCLUDED.planner_model,
             plan_revisions = nexus_agent_plans.plan_revisions"#,
    )
    .bind(run_id)
    .bind(project_id)
    .bind(&thread_id)
    .bind(&plan_acceptance)
    .bind(planner_model)
    .execute(&mut *tx)
    .await;
    if let Err(e) = plan_res {
        return err(&format!("INSERT nexus_agent_plans fallita: {e}"));
    }

    // Cancella i todos esistenti del run (create = reset completo).
    if let Err(e) = sqlx::query("DELETE FROM nexus_agent_todos WHERE run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await
    {
        return err(&format!("DELETE precedenti todos fallita: {e}"));
    }

    // Insert dei nuovi todos.
    let mut inserted_ids: Vec<String> = Vec::with_capacity(todos_in.len());
    for (idx, t) in todos_in.iter().enumerate() {
        let content = match t.get("content").and_then(Value::as_str) {
            Some(c) if !c.trim().is_empty() => c,
            _ => return err(&format!("todo[{idx}]: 'content' obbligatorio e non vuoto")),
        };
        let status = t.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !VALID_STATUSES.contains(&status) {
            return err(&format!("todo[{idx}]: status '{status}' non valido"));
        }
        let priority = t.get("priority").and_then(Value::as_str).unwrap_or("normal");
        if !VALID_PRIORITIES.contains(&priority) {
            return err(&format!("todo[{idx}]: priority '{priority}' non valida"));
        }
        let seq = t
            .get("seq")
            .and_then(Value::as_i64)
            .unwrap_or((idx + 1) as i64) as i32;
        let acceptance = t
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));

        let row = sqlx::query(
            r#"INSERT INTO nexus_agent_todos
               (run_id, project_id, seq, content, status, priority, acceptance_criteria)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               RETURNING id"#,
        )
        .bind(run_id)
        .bind(project_id)
        .bind(seq)
        .bind(content)
        .bind(status)
        .bind(priority)
        .bind(&acceptance)
        .fetch_one(&mut *tx)
        .await;
        match row {
            Ok(r) => {
                let id: Uuid = r.get("id");
                inserted_ids.push(id.to_string());
            }
            Err(e) => return err(&format!("INSERT todo[{idx}] fallita: {e}")),
        }
    }

    if let Err(e) = tx.commit().await {
        return err(&format!("commit tx fallita: {e}"));
    }

    json!({
        "ok": true,
        "action": "create",
        "affected": inserted_ids.len(),
        "plan_id": run_id.to_string(),
        "todo_ids": inserted_ids,
    })
    .to_string()
}

async fn add_todos(
    ctx: &AgentToolContext,
    run_id: Uuid,
    project_id: Uuid,
    todos_in: &[Value],
) -> String {
    // Verifica che il plan esista.
    let plan_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM nexus_agent_plans WHERE run_id = $1)",
    )
    .bind(run_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(false);
    if !plan_exists {
        return err(&format!(
            "plan inesistente per run_id={run_id}: chiama action='create' prima"
        ));
    }

    let max_seq: Option<i32> = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) FROM nexus_agent_todos WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&*ctx.db)
    .await
    .ok();
    let base = max_seq.unwrap_or(0);

    let mut tx = match ctx.db.begin().await {
        Ok(t) => t,
        Err(e) => return err(&format!("begin tx fallita: {e}")),
    };
    let mut inserted_ids: Vec<String> = Vec::new();
    for (idx, t) in todos_in.iter().enumerate() {
        let content = match t.get("content").and_then(Value::as_str) {
            Some(c) if !c.trim().is_empty() => c,
            _ => return err(&format!("todo[{idx}]: 'content' obbligatorio")),
        };
        let status = t.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !VALID_STATUSES.contains(&status) {
            return err(&format!("todo[{idx}]: status '{status}' non valido"));
        }
        let priority = t.get("priority").and_then(Value::as_str).unwrap_or("normal");
        let acceptance = t
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let seq = base + 1 + idx as i32;

        let row = sqlx::query(
            r#"INSERT INTO nexus_agent_todos
               (run_id, project_id, seq, content, status, priority, acceptance_criteria)
               VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id"#,
        )
        .bind(run_id)
        .bind(project_id)
        .bind(seq)
        .bind(content)
        .bind(status)
        .bind(priority)
        .bind(&acceptance)
        .fetch_one(&mut *tx)
        .await;
        match row {
            Ok(r) => {
                let id: Uuid = r.get("id");
                inserted_ids.push(id.to_string());
            }
            Err(e) => return err(&format!("INSERT add todo[{idx}] fallita: {e}")),
        }
    }
    if let Err(e) = tx.commit().await {
        return err(&format!("commit tx fallita: {e}"));
    }
    json!({
        "ok": true,
        "action": "add",
        "affected": inserted_ids.len(),
        "todo_ids": inserted_ids,
    })
    .to_string()
}

/// Aggiorna lo status (e altri campi) di todos esistenti.
/// `check_mode=true` significa azione "check" (target esplicito = mark completed).
async fn update_status(
    ctx: &AgentToolContext,
    run_id: Uuid,
    _project_id: Uuid,
    todos_in: &[Value],
    check_mode: bool,
) -> String {
    let mut tx = match ctx.db.begin().await {
        Ok(t) => t,
        Err(e) => return err(&format!("begin tx fallita: {e}")),
    };

    let mut affected = 0_usize;
    for (idx, t) in todos_in.iter().enumerate() {
        let id = match t.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) {
            Some(u) => u,
            None => return err(&format!("todo[{idx}]: 'id' uuid obbligatorio per check/update")),
        };
        let new_status = if check_mode {
            "completed".to_string()
        } else {
            match t.get("status").and_then(Value::as_str) {
                Some(s) if VALID_STATUSES.contains(&s) => s.to_string(),
                _ => return err(&format!("todo[{idx}]: status mancante o non valido")),
            }
        };

        let res = sqlx::query(
            r#"UPDATE nexus_agent_todos
               SET status = $1, updated_at = NOW()
               WHERE id = $2 AND run_id = $3"#,
        )
        .bind(&new_status)
        .bind(id)
        .bind(run_id)
        .execute(&mut *tx)
        .await;
        match res {
            Ok(r) if r.rows_affected() == 0 => {
                return err(&format!(
                    "todo {id} non trovato (o non appartiene a run_id={run_id})"
                ))
            }
            Ok(r) => affected += r.rows_affected() as usize,
            Err(e) => return err(&format!("UPDATE todo[{idx}] fallita: {e}")),
        }
    }

    if let Err(e) = tx.commit().await {
        return err(&format!("commit tx fallita: {e}"));
    }

    json!({
        "ok": true,
        "action": if check_mode { "check" } else { "update" },
        "affected": affected,
    })
    .to_string()
}

fn err(msg: &str) -> String {
    format!("\u{274C} [nexus_todo_write] {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_statuses_include_canonical_set() {
        for s in ["pending", "in_progress", "completed", "blocked", "skipped"] {
            assert!(VALID_STATUSES.contains(&s), "manca status {s}");
        }
    }

    #[test]
    fn valid_priorities_include_canonical_set() {
        for p in ["high", "normal", "low"] {
            assert!(VALID_PRIORITIES.contains(&p));
        }
    }

    #[test]
    fn err_marks_with_failure_glyph() {
        let m = err("boom");
        assert!(m.starts_with('\u{274C}'));
        assert!(m.contains("[nexus_todo_write]"));
        assert!(m.contains("boom"));
    }
}
