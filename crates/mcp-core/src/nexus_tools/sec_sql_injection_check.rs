//! `security::sec_sql_injection_check` — find string interpolation in SQL queries.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecSqlInjectionCheckTool;

#[async_trait]
impl NexusToolHandler for SecSqlInjectionCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "format!(\"SELECT",
                "format!(\"INSERT",
                "format!(\"UPDATE",
                "format!(\"DELETE",
                "sqlx::query(",
                "sqlx::query_as(",
            ],
        );
        let interp_total = counts[0] + counts[1] + counts[2] + counts[3];
        let parameterized_total = counts[4] + counts[5];
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "format_select": counts[0],
            "format_insert": counts[1],
            "format_update": counts[2],
            "format_delete": counts[3],
            "sqlx_query": counts[4],
            "sqlx_query_as": counts[5],
            "interpolated_total": interp_total,
            "parameterized_total": parameterized_total,
            "warning": interp_total > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
