//! `performance::perf_arc_mutex` — conta pattern di synchronization condivisa.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfArcMutexTool;

#[async_trait]
impl NexusToolHandler for PerfArcMutexTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "Arc<Mutex<",
                "Arc<RwLock<",
                "Arc::new(",
                "Mutex::new(",
                "RwLock::new(",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "arc_mutex": counts[0],
            "arc_rwlock": counts[1],
            "arc_new": counts[2],
            "mutex_new": counts[3],
            "rwlock_new": counts[4],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
