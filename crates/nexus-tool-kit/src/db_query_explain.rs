//! `database::db_query_explain` — esegue `EXPLAIN (ANALYZE, FORMAT JSON)` su
//! una query PostgreSQL.
//!
//! **Sicurezza**: la query viene racchiusa in un blocco `EXPLAIN (ANALYZE OFF, FORMAT JSON)`
//! per default (nessuna esecuzione reale). L'utente può abilitare `analyze=true`
//! esplicitamente — in quel caso la query viene eseguita davvero.
//!
//! Solo query `SELECT` sono accettate, per whitelist; altre forme (INSERT/UPDATE/
//! DELETE/DDL) vengono rifiutate con `BadInput` per evitare effetti collaterali
//! accidentali.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

pub struct DbQueryExplainTool;

fn is_select_only(sql: &str) -> bool {
    let trimmed = sql.trim().to_lowercase();
    // Accetta SELECT o WITH ... SELECT
    trimmed.starts_with("select ")
        || trimmed.starts_with("select\n")
        || trimmed.starts_with("select(")
        || trimmed.starts_with("with ")
}

#[async_trait]
impl NexusToolHandler for DbQueryExplainTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let sql = args
            .get("sql")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("sql required".into()))?
            .to_string();
        let analyze = args
            .get("analyze")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if !is_select_only(&sql) {
            return Err(NexusToolError::BadInput(
                "Only SELECT or WITH ... SELECT queries are permitted for EXPLAIN".into(),
            ));
        }

        let db_url = std::env::var("DATABASE_URL")
            .map_err(|_| NexusToolError::BadInput("DATABASE_URL not set in environment".into()))?;

        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&db_url)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("db connect failed: {}", e)))?;

        let analyze_flag = if analyze { "TRUE" } else { "FALSE" };
        let explain_sql = format!(
            "EXPLAIN (ANALYZE {}, VERBOSE TRUE, COSTS TRUE, FORMAT JSON) {}",
            analyze_flag, sql
        );

        let rows = sqlx::query(&explain_sql)
            .fetch_all(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("explain failed: {}", e)))?;

        let mut plan: Vec<Value> = Vec::with_capacity(rows.len());
        for row in rows {
            // pg restituisce text row: il primo columno è JSON text o tipo JSON
            let line: String = row
                .try_get::<String, _>(0)
                .or_else(|_| row.try_get::<Value, _>(0).map(|v| v.to_string()))
                .unwrap_or_default();
            plan.push(serde_json::from_str(&line).unwrap_or(Value::String(line)));
        }

        pool.close().await;

        Ok(json!({
            "ok": true,
            "analyze": analyze,
            "plan": plan,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["sql"],
            "properties": {
                "sql": {"type": "string", "description": "SELECT query (o WITH ... SELECT)"},
                "analyze": {"type": "boolean", "description": "Esegui davvero la query (default false)"}
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_select_only_accepts_select() {
        assert!(is_select_only("SELECT 1"));
        assert!(is_select_only("select * from users"));
        assert!(is_select_only("WITH x AS (SELECT 1) SELECT * FROM x"));
    }

    #[test]
    fn test_is_select_only_rejects_writes() {
        assert!(!is_select_only("INSERT INTO users ..."));
        assert!(!is_select_only("UPDATE users SET ..."));
        assert!(!is_select_only("DELETE FROM users"));
        assert!(!is_select_only("DROP TABLE users"));
    }

    #[test]
    fn test_safety_readonly_network() {
        let s = DbQueryExplainTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
    }
}
