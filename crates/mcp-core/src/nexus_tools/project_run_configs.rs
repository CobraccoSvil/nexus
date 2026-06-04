//! `project_run_configs` — configurazioni di esecuzione (comandi) disponibili per il progetto.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tools::db_helper::get_pool;
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectRunConfigsTool;

#[async_trait]
impl NexusToolHandler for ProjectRunConfigsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let rows = sqlx::query(
            r#"SELECT id, label, kind, command, args, cwd, env
               FROM run_configurations
               WHERE project_id = $1
               ORDER BY label"#,
        )
        .bind(ctx.project_id)
        .fetch_all(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("query: {}", e)))?;

        pool.close().await;

        if rows.is_empty() {
            return Ok(json!({
                "ok": true,
                "configs": [],
                "message": "Nessuna configurazione di esecuzione trovata per questo progetto."
            }));
        }

        let configs: Vec<Value> = rows
            .iter()
            .map(|r| {
                let id: uuid::Uuid = r.try_get("id").unwrap_or_default();
                let label: String = r.try_get("label").unwrap_or_default();
                let kind: Option<String> = r.try_get("kind").unwrap_or(None);
                let command: String = r.try_get("command").unwrap_or_default();
                let args: Option<Vec<String>> = r.try_get("args").unwrap_or(None);
                let cwd: Option<String> = r.try_get("cwd").unwrap_or(None);
                let env: Option<Value> = r.try_get("env").unwrap_or(None);

                json!({
                    "id": id.to_string(),
                    "label": label,
                    "kind": kind,
                    "command": command,
                    "args": args,
                    "cwd": cwd,
                    "env": env,
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "configs": configs,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}
