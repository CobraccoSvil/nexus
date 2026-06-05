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
use std::collections::HashMap;
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
        return err(&format!(
            "action '{action}' non valida (create|check|add|update)"
        ));
    }

    let run_id = match input
        .get("run_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
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
    let run_check: Option<(Uuid,)> =
        sqlx::query_as("SELECT project_id FROM agent_runs WHERE id = $1 LIMIT 1")
            .bind(run_id)
            .fetch_optional(&*ctx.db)
            .await
            .ok()
            .flatten();
    match run_check {
        Some((p,)) if p == project_id => {} // OK
        Some(_) => {
            return err("run_id non appartiene al project_id corrente (isolation violation)")
        }
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
    let planner_model = input
        .get("planner_model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

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

    // Cluster 1: contesto decisionale del planner (mig 0206). Best-effort:
    // colonne nullable con default, se assenti dal payload restano vuote.
    let plan_rationale = input
        .get("rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let plan_constraints = input
        .get("constraints")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let plan_alternatives = input
        .get("alternatives")
        .cloned()
        .unwrap_or_else(|| json!([]));
    // Intent e behavior_mode di creazione del piano (mig 0328): permettono al
    // planner di invalidare il riuso quando l'intent corrente diverge (fix
    // plan-reuse intent-aware). Nullable: i piani senza questi campi restano
    // riusabili come prima.
    let plan_user_intent = input.get("user_intent").and_then(|v| v.as_str());
    let plan_behavior_mode = input.get("behavior_mode").and_then(|v| v.as_str());

    let mut tx = match ctx.db.begin().await {
        Ok(t) => t,
        Err(e) => return err(&format!("begin tx fallita: {e}")),
    };

    // Upsert del plan (PRIMARY KEY = run_id, quindi ON CONFLICT su run_id).
    let plan_res = sqlx::query(
        r#"INSERT INTO nexus_agent_plans
             (run_id, project_id, thread_id, acceptance_criteria, planner_model,
              rationale, constraints, alternatives, user_intent, behavior_mode)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           ON CONFLICT (run_id) DO UPDATE SET
             acceptance_criteria = EXCLUDED.acceptance_criteria,
             planner_model = EXCLUDED.planner_model,
             rationale = EXCLUDED.rationale,
             constraints = EXCLUDED.constraints,
             alternatives = EXCLUDED.alternatives,
             user_intent = EXCLUDED.user_intent,
             behavior_mode = EXCLUDED.behavior_mode,
             plan_revisions = nexus_agent_plans.plan_revisions"#,
    )
    .bind(run_id)
    .bind(project_id)
    .bind(&thread_id)
    .bind(&plan_acceptance)
    .bind(planner_model)
    .bind(plan_rationale)
    .bind(&plan_constraints)
    .bind(&plan_alternatives)
    .bind(plan_user_intent)
    .bind(plan_behavior_mode)
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

    // Insert dei nuovi todos. Comp.3a: raccogliamo node_key->uuid e i dep_keys
    // logici per risolvere le dipendenze del DAG in un secondo passaggio (il
    // planner non conosce gli UUID generati, ragiona su chiavi logiche).
    let mut inserted_ids: Vec<String> = Vec::with_capacity(todos_in.len());
    let mut key_to_id: HashMap<String, Uuid> = HashMap::new();
    let mut todo_deps: Vec<(Uuid, Vec<String>)> = Vec::with_capacity(todos_in.len());
    for (idx, t) in todos_in.iter().enumerate() {
        let content = match t.get("content").and_then(Value::as_str) {
            Some(c) if !c.trim().is_empty() => c,
            _ => return err(&format!("todo[{idx}]: 'content' obbligatorio e non vuoto")),
        };
        let status = t.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !VALID_STATUSES.contains(&status) {
            return err(&format!("todo[{idx}]: status '{status}' non valido"));
        }
        let priority = t
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("normal");
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
        // Comp.3a: chiave logica del nodo + chiavi delle dipendenze.
        let node_key = t
            .get("node_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let dep_keys: Vec<String> = t
            .get("dep_keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let row = sqlx::query(
            r#"INSERT INTO nexus_agent_todos
               (run_id, project_id, seq, content, status, priority, acceptance_criteria, node_key, dep_keys)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING id"#,
        )
        .bind(run_id)
        .bind(project_id)
        .bind(seq)
        .bind(content)
        .bind(status)
        .bind(priority)
        .bind(&acceptance)
        .bind(node_key)
        .bind(&dep_keys)
        .fetch_one(&mut *tx)
        .await;
        match row {
            Ok(r) => {
                let id: Uuid = r.get("id");
                if let Some(k) = node_key {
                    key_to_id.insert(k.to_string(), id);
                }
                todo_deps.push((id, dep_keys));
                inserted_ids.push(id.to_string());
            }
            Err(e) => return err(&format!("INSERT todo[{idx}] fallita: {e}")),
        }
    }

    // Comp.3a: risolvi dep_keys -> depends_on (UUID[]) con cycle detection.
    let deps_set = match resolve_and_persist_deps(&mut tx, &key_to_id, &todo_deps).await {
        Ok(n) => n,
        Err(e) => return err(&format!("risoluzione dipendenze DAG fallita: {e}")),
    };

    if let Err(e) = tx.commit().await {
        return err(&format!("commit tx fallita: {e}"));
    }

    json!({
        "ok": true,
        "action": "create",
        "dag_deps_resolved": deps_set,
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
    let plan_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nexus_agent_plans WHERE run_id = $1)")
            .bind(run_id)
            .fetch_one(&*ctx.db)
            .await
            .unwrap_or(false);
    if !plan_exists {
        return err(&format!(
            "plan inesistente per run_id={run_id}: chiama action='create' prima"
        ));
    }

    let max_seq: Option<i32> =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM nexus_agent_todos WHERE run_id = $1")
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
        let priority = t
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("normal");
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
    // M15.1: traccia (id, status) aggiornati per emettere TodoUpdated dopo il commit.
    let mut updated: Vec<(Uuid, String)> = Vec::new();
    for (idx, t) in todos_in.iter().enumerate() {
        let id = match t
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            Some(u) => u,
            None => {
                return err(&format!(
                    "todo[{idx}]: 'id' uuid obbligatorio per check/update"
                ))
            }
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
            Ok(r) => {
                affected += r.rows_affected() as usize;
                updated.push((id, new_status.clone()));
            }
            Err(e) => return err(&format!("UPDATE todo[{idx}] fallita: {e}")),
        }
    }

    if let Err(e) = tx.commit().await {
        return err(&format!("commit tx fallita: {e}"));
    }

    // M15.1 — Progresso todo live: emette TodoUpdated per ogni todo aggiornato,
    // cosi' la checklist in chat (agent-meta-step-card.tsx) si spunta in tempo
    // reale durante l'esecuzione (gated agent.todos.live_events).
    let live_events = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.todos.live_events' LIMIT 1",
    )
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten()
    .map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        )
    })
    .unwrap_or(true);
    if live_events {
        for (id, status) in &updated {
            nexus_events::dispatcher::emit_global(
                ctx.project_id,
                nexus_events::event::ProjectEvent::TodoUpdated {
                    run_id: run_id.to_string(),
                    todo_id: id.to_string(),
                    seq: None,
                    status: status.clone(),
                },
            );
        }
        // M15 — PlanUpdated: avanzamento aggregato del piano (totale/completati)
        // cosi' la UI aggiorna il contatore senza ricaricare la checklist.
        if let Ok((total, completed)) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*), COUNT(*) FILTER (WHERE status = 'completed') \
             FROM nexus_agent_todos WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&*ctx.db)
        .await
        {
            nexus_events::dispatcher::emit_global(
                ctx.project_id,
                nexus_events::event::ProjectEvent::PlanUpdated {
                    run_id: run_id.to_string(),
                    total: total as i32,
                    completed: completed as i32,
                },
            );
        }
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

/// Comp.3a: risolve i dep_keys logici in depends_on (UUID[]) e li scrive sui
/// todo, dentro la transazione corrente. Scarta i dep_keys che non
/// corrispondono a nessun node_key del piano (riferimenti fantasma) e i
/// self-link. Se il grafo risultante contiene un ciclo, azzera tutte le
/// dipendenze (fallback all'esecuzione lineare per seq) invece di rischiare un
/// deadlock dello scheduler. Ritorna il numero di todo con almeno una dipendenza.
async fn resolve_and_persist_deps(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key_to_id: &HashMap<String, Uuid>,
    todo_deps: &[(Uuid, Vec<String>)],
) -> Result<usize, sqlx::Error> {
    let mut resolved: Vec<(Uuid, Vec<Uuid>)> = Vec::with_capacity(todo_deps.len());
    for (id, dep_keys) in todo_deps {
        let mut deps: Vec<Uuid> = Vec::new();
        for k in dep_keys {
            if let Some(dep_id) = key_to_id.get(k) {
                if dep_id != id && !deps.contains(dep_id) {
                    deps.push(*dep_id);
                }
            }
        }
        resolved.push((*id, deps));
    }

    if detect_cycle(&resolved) {
        tracing::warn!(
            "nexus_todo_write: ciclo rilevato nel DAG dei todo, fallback lineare (depends_on non applicati)"
        );
        return Ok(0);
    }

    let mut updated = 0usize;
    for (id, deps) in &resolved {
        if deps.is_empty() {
            continue;
        }
        sqlx::query("UPDATE nexus_agent_todos SET depends_on = $1 WHERE id = $2")
            .bind(deps.as_slice())
            .bind(id)
            .execute(&mut **tx)
            .await?;
        updated += 1;
    }
    Ok(updated)
}

/// Kahn topological sort: ritorna true se il grafo (id -> depends_on) contiene
/// un ciclo. depends_on[n] = nodi che devono precedere n.
fn detect_cycle(nodes: &[(Uuid, Vec<Uuid>)]) -> bool {
    let ids: std::collections::HashSet<Uuid> = nodes.iter().map(|(id, _)| *id).collect();
    let mut in_degree: HashMap<Uuid, usize> = HashMap::new();
    let mut adj: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (id, deps) in nodes {
        in_degree.entry(*id).or_insert(0);
        for d in deps {
            if ids.contains(d) {
                *in_degree.entry(*id).or_insert(0) += 1;
                adj.entry(*d).or_default().push(*id);
            }
        }
    }
    let mut queue: Vec<Uuid> = in_degree
        .iter()
        .filter(|(_, &v)| v == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut visited = 0usize;
    while let Some(n) = queue.pop() {
        visited += 1;
        if let Some(children) = adj.get(&n).cloned() {
            for c in children {
                if let Some(v) = in_degree.get_mut(&c) {
                    *v -= 1;
                    if *v == 0 {
                        queue.push(c);
                    }
                }
            }
        }
    }
    visited < nodes.len()
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

    #[test]
    fn detect_cycle_acyclic_chain() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        // c depends_on b, b depends_on a: catena lineare, niente ciclo.
        let nodes = vec![(a, vec![]), (b, vec![a]), (c, vec![b])];
        assert!(!detect_cycle(&nodes));
    }

    #[test]
    fn detect_cycle_with_cycle() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        // a depends_on b e b depends_on a: ciclo.
        let nodes = vec![(a, vec![b]), (b, vec![a])];
        assert!(detect_cycle(&nodes));
    }

    #[test]
    fn detect_cycle_diamond_is_acyclic() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let d = Uuid::new_v4();
        // d <- b,c <- a (diamante): nessun ciclo.
        let nodes = vec![(a, vec![]), (b, vec![a]), (c, vec![a]), (d, vec![b, c])];
        assert!(!detect_cycle(&nodes));
    }
}
