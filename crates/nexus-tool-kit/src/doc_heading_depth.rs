//! `documentation::doc_heading_depth` — profondità massima/distribuzione heading in un .md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocHeadingDepthTool;

#[async_trait]
impl NexusToolHandler for DocHeadingDepthTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (path, content) = super::read_doc_file(ctx, args)?;
        let mut counts = [0usize; 7]; // index 1..=6
        let mut max = 0usize;
        for line in content.lines() {
            let trimmed = line.trim_start();
            let depth = trimmed.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&depth) && trimmed.chars().nth(depth) == Some(' ') {
                counts[depth] += 1;
                if depth > max {
                    max = depth;
                }
            }
        }
        Ok(json!({
            "ok": true,
            "path": path,
            "max_depth": max,
            "h1": counts[1],
            "h2": counts[2],
            "h3": counts[3],
            "h4": counts[4],
            "h5": counts[5],
            "h6": counts[6],
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
