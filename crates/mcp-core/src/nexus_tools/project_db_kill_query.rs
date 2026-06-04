//! `project_db_kill_query` — termina una query bloccante sul DB progetto.
//!
//! Usa pg_cancel_backend (graceful) o pg_terminate_backend (force).
//! Opera solo su connessioni del DB del progetto.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbKillQueryTool;

#[async_trait]
impl NexusToolHandler for ProjectDbKillQueryTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let pid = args.get("pid").and_then(Value::as_i64).ok_or_else(|| {
            NexusToolError::BadInput("Parametro 'pid' obbligatorio (intero)".into())
        })? as i32;

        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(|e| NexusToolError::BadInput(e))?;

        nexus_pool.close().await;

        // Prima verifica che il PID appartenga al DB del progetto
        let query_info = sqlx::query(
            r#"SELECT pid, datname, usename, state, query, query_start
               FROM pg_stat_activity
               WHERE pid = $1"#,
        )
        .bind(pid)
        .fetch_optional(&project_pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup pid: {}", e)))?;

        let row = match query_info {
            Some(r) => r,
            None => {
                project_pool.close().await;
                return Err(NexusToolError::BadInput(format!(
                    "PID {} non trovato nelle connessioni attive",
                    pid
                )));
            }
        };

        let datname: String = row.try_get("datname").unwrap_or_default();
        let usename: String = row.try_get("usename").unwrap_or_default();
        let state: String = row.try_get("state").unwrap_or_default();
        let query: String = row.try_get("query").unwrap_or_default();

        // Esegui cancel o terminate
        let sql = if force {
            "SELECT pg_terminate_backend($1)"
        } else {
            "SELECT pg_cancel_backend($1)"
        };

        let result: bool = sqlx::query_scalar(sql)
            .bind(pid)
            .fetch_one(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("kill query: {}", e)))?;

        project_pool.close().await;

        let action = if force { "terminate" } else { "cancel" };
        let truncated_query = if query.len() > 500 {
            format!("{}...", &query[..500])
        } else {
            query
        };

        Ok(json!({
            "ok": result,
            "pid": pid,
            "action": action,
            "database": datname,
            "user": usename,
            "state": state,
            "query": truncated_query,
            "message": if result {
                format!("Query (pid={}) {} con successo", pid, action)
            } else {
                format!("Impossibile {} la query (pid={})", action, pid)
            },
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pid"],
            "properties": {
                "pid": {
                    "type": "integer",
                    "description": "PID del processo backend PostgreSQL da terminare"
                },
                "force": {
                    "type": "boolean",
                    "description": "Se true, usa pg_terminate_backend (SIGTERM). Se false (default), usa pg_cancel_backend (graceful)."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety() {
        let s = ProjectDbKillQueryTool.safety();
        assert!(!s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_requires_pid() {
        let s = ProjectDbKillQueryTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "pid"));
    }
}
