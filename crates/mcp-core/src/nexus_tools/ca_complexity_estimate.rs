//! `code_analysis::ca_complexity_estimate` — rough cyclomatic-complexity estimate.
//!
//! Counts decision points (`if`, `else if`, `match` arms, `&&`, `||`, `?`) across
//! the workspace `.rs` files and returns a heuristic total. Not a true CC, but
//! useful for trend tracking.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaComplexityEstimateTool;

#[async_trait]
impl NexusToolHandler for CaComplexityEstimateTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["if ", "else if ", " => ", " && ", " || ", "?;"],
        );
        let total: usize = counts.iter().sum();
        let avg = if files == 0 { 0.0 } else { total as f64 / files as f64 };
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "if_count": counts[0],
            "else_if_count": counts[1],
            "match_arm_count": counts[2],
            "and_and_count": counts[3],
            "or_or_count": counts[4],
            "try_op_count": counts[5],
            "total_decision_points": total,
            "avg_per_file": (avg * 100.0).round() / 100.0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
