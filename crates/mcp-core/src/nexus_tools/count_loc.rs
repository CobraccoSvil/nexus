//! `code_analysis::count_loc` — conta le righe di codice per linguaggio.
//!
//! Versione self-contained senza tokei/cloc: walk del project_root,
//! classifica per estensione file, conta lines (totale, blank, commentate
//! a prima approssimazione per linguaggi con `//` / `#` / `--`).
//!
//! Output: `{ok, total_files, total_lines, by_language: [{lang, files, lines, blank, comments}]}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub struct CountLocTool;

fn lang_of(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "rb" => "ruby",
        "c" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "swift" => "swift",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "md" => "markdown",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        _ => return None,
    })
}

fn comment_prefix(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "rust" | "typescript" | "javascript" | "go" | "java" | "kotlin" | "c" | "cpp"
        | "csharp" | "swift" | "scss" => "//",
        "python" | "ruby" | "shell" | "yaml" | "toml" => "#",
        "sql" => "--",
        _ => return None,
    })
}

#[derive(Default, Clone)]
struct Stat {
    files: usize,
    lines: usize,
    blank: usize,
    comments: usize,
}

fn walk_count(
    root: &Path,
    stats: &mut HashMap<String, Stat>,
    total: &mut (usize, usize),
    cap: usize,
) {
    if total.0 >= cap {
        return;
    }
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if total.0 >= cap {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
            || name == "vendor"
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_count(&path, stats, total, cap);
            continue;
        }
        if meta.len() > 2 * 1024 * 1024 {
            continue; // skip huge files
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        let Some(lang) = lang_of(&ext) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let prefix = comment_prefix(lang);
        let mut s = Stat {
            files: 1,
            ..Default::default()
        };
        for line in content.lines() {
            s.lines += 1;
            let t = line.trim();
            if t.is_empty() {
                s.blank += 1;
            } else if let Some(p) = prefix {
                if t.starts_with(p) {
                    s.comments += 1;
                }
            }
        }
        let entry = stats.entry(lang.to_string()).or_default();
        entry.files += s.files;
        entry.lines += s.lines;
        entry.blank += s.blank;
        entry.comments += s.comments;
        total.0 += 1;
        total.1 += s.lines;
    }
}

#[async_trait]
impl NexusToolHandler for CountLocTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let dir = args.get("dir").and_then(Value::as_str).unwrap_or("");
        let cap = args
            .get("max_files")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(10_000)
            .min(50_000);

        let full = ctx.project_root.join(dir);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }

        let mut stats: HashMap<String, Stat> = HashMap::new();
        let mut total = (0usize, 0usize);
        walk_count(&full, &mut stats, &mut total, cap);

        let mut by_language: Vec<Value> = stats
            .into_iter()
            .map(|(lang, s)| {
                json!({
                    "lang": lang,
                    "files": s.files,
                    "lines": s.lines,
                    "blank": s.blank,
                    "comments": s.comments,
                    "code": s.lines.saturating_sub(s.blank + s.comments),
                })
            })
            .collect();
        by_language.sort_by(|a, b| {
            b["lines"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["lines"].as_u64().unwrap_or(0))
        });

        Ok(json!({
            "ok": true,
            "total_files": total.0,
            "total_lines": total.1,
            "by_language": by_language,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "dir": {"type": "string"},
                "max_files": {"type": "integer", "minimum": 1, "maximum": 50000}
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
    async fn test_count_rust_file() {
        let tmp = std::env::temp_dir().join(format!("cloc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("x.rs"),
            "// comment\nfn main() {\n\n    let a = 1;\n}\n",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = CountLocTool.execute(&ctx, &json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert!(out["total_files"].as_u64().unwrap() >= 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
