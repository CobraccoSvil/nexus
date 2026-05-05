//! `code_analysis::ca_generic_count` — count generic parameter usage.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaGenericCountTool;

#[async_trait]
impl NexusToolHandler for CaGenericCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["<T>", "<T,", "<T:", "where ", "PhantomData"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "generic_t": counts[0],
            "generic_t_comma": counts[1],
            "generic_t_bound": counts[2],
            "where_clause": counts[3],
            "phantom_data": counts[4],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
