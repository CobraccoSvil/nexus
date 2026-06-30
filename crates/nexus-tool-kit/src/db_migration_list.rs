//! `database::db_migration_list` — lista file di migrazione in `db/migrations` o `migrations`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbMigrationListTool;

#[async_trait]
impl NexusToolHandler for DbMigrationListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let candidates = ["db/migrations", "migrations"];
        let mut found_dir: Option<String> = None;
        let mut files: Vec<String> = vec![];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.ends_with(".sql") {
                            files.push(name);
                        }
                    }
                }
                files.sort();
                found_dir = Some((*c).to_string());
                break;
            }
        }
        Ok(json!({"ok": true, "dir": found_dir, "count": files.len(), "files": files}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
