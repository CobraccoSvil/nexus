//! `dependencies::cargo_features_list` — parse `[features]` da Cargo.toml.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoFeaturesListTool;

#[async_trait]
impl NexusToolHandler for CargoFeaturesListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let manifest = ctx.project_root.join("Cargo.toml");
        let content = std::fs::read_to_string(&manifest).map_err(NexusToolError::Io)?;
        let mut features: Vec<(String, Vec<String>)> = Vec::new();
        let mut in_features = false;
        for raw in content.lines() {
            let line = raw.trim();
            if line == "[features]" {
                in_features = true;
                continue;
            }
            if in_features {
                if line.starts_with('[') {
                    in_features = false;
                    continue;
                }
                if let Some(eq) = line.find('=') {
                    let name = line[..eq].trim().to_string();
                    if name.is_empty() || name.starts_with('#') {
                        continue;
                    }
                    let rhs = line[eq + 1..].trim();
                    // Estrai elementi dell'array tra []
                    let mut deps: Vec<String> = Vec::new();
                    if let (Some(lb), Some(rb)) = (rhs.find('['), rhs.rfind(']')) {
                        for tok in rhs[lb + 1..rb].split(',') {
                            let t = tok.trim().trim_matches('"');
                            if !t.is_empty() {
                                deps.push(t.to_string());
                            }
                        }
                    }
                    features.push((name, deps));
                }
            }
        }
        let items: Vec<Value> = features
            .into_iter()
            .map(|(n, d)| json!({"name": n, "deps": d}))
            .collect();
        Ok(json!({"ok": true, "count": items.len(), "features": items}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
