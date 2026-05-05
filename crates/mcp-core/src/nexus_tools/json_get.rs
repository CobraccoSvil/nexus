//! `utility::json_get` — estrae un valore da una struttura JSON con
//! notazione dot-path semplificata (es. `foo.bar[0].baz`).
//!
//! Input: `{json_content | path, query}`
//! - `json_content`: stringa JSON inline
//! - `path`: alternativa — file relativo al project_root
//! - `query`: dot-path con bracket index (`users[0].name`)
//!
//! Output: `{ok, found, value?, type?}`

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct JsonGetTool;

/// Parser dot-path minimo: `a.b[0].c` → `["a", "b", "0", "c"]` ma
/// tiene traccia del tipo (index vs key) tramite un enum.
enum Segment {
    Key(String),
    Index(usize),
}

fn parse_query(q: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = q.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !buf.is_empty() {
                    out.push(Segment::Key(std::mem::take(&mut buf)));
                }
            }
            '[' => {
                if !buf.is_empty() {
                    out.push(Segment::Key(std::mem::take(&mut buf)));
                }
                let mut idx = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == ']' {
                        chars.next();
                        break;
                    }
                    idx.push(nc);
                    chars.next();
                }
                if let Ok(n) = idx.parse::<usize>() {
                    out.push(Segment::Index(n));
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(Segment::Key(buf));
    }
    out
}

fn resolve<'a>(root: &'a Value, segs: &[Segment]) -> Option<&'a Value> {
    let mut cur = root;
    for s in segs {
        match s {
            Segment::Key(k) => cur = cur.get(k)?,
            Segment::Index(i) => cur = cur.get(*i)?,
        }
    }
    Some(cur)
}

#[async_trait]
impl NexusToolHandler for JsonGetTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("query required".into()))?;
        let content = if let Some(c) = args.get("json_content").and_then(Value::as_str) {
            c.to_string()
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            let full = ctx.project_root.join(path);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read_to_string(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput(
                "json_content or path required".into(),
            ));
        };

        let root: Value = serde_json::from_str(&content)
            .map_err(|e| NexusToolError::BadInput(format!("invalid json: {}", e)))?;
        let segs = parse_query(query);
        let found = resolve(&root, &segs);

        Ok(json!({
            "ok": true,
            "query": query,
            "found": found.is_some(),
            "value": found.cloned().unwrap_or(Value::Null),
            "type": found.map(|v| match v {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            }).unwrap_or("missing"),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "json_content": {"type": "string"},
                "path": {"type": "string"},
                "query": {"type": "string", "description": "Dot-path es: 'a.b[0].c'"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_json_get_nested() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = JsonGetTool
            .execute(
                &ctx,
                &json!({
                    "json_content": "{\"users\":[{\"name\":\"alice\"}]}",
                    "query": "users[0].name"
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["found"], true);
        assert_eq!(out["value"], "alice");
    }

    #[tokio::test]
    async fn test_json_get_missing() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = JsonGetTool
            .execute(
                &ctx,
                &json!({"json_content": "{\"a\":1}", "query": "b.c"}),
            )
            .await
            .unwrap();
        assert_eq!(out["found"], false);
    }
}
