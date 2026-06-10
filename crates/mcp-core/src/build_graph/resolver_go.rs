//! Resolver Go: legge `go.mod` (singolo modulo) o `go.work` (workspace).
//!
//! Tutti i `.go` sotto la module root sono nel build graph. Per il file
//! workspace `go.work` ogni `use ./path` aggiunge un module sub-directory.

use std::path::Path;

use chrono::Utc;
use uuid::Uuid;

use super::model::BuildGraphInfo;

pub async fn resolve_go(project_id: Uuid, project_root: &Path) -> anyhow::Result<BuildGraphInfo> {
    let go_work = project_root.join("go.work");
    let go_mod = project_root.join("go.mod");

    let mut sources: Vec<String> = Vec::new();
    let mut include_globs: Vec<String> = Vec::new();
    let mut monorepo_members: Vec<String> = Vec::new();
    let mut entry_points: Vec<String> = Vec::new();

    if go_work.is_file() {
        sources.push(go_work.to_string_lossy().into_owned());
        let raw = tokio::fs::read_to_string(&go_work).await?;
        for path in parse_go_work_uses(&raw) {
            let normalized = path.trim_start_matches("./").trim_end_matches('/');
            if normalized.is_empty() || normalized == "." {
                include_globs.push("**/*.go".to_string());
            } else {
                include_globs.push(format!("{}/**", normalized));
                monorepo_members.push(normalized.to_string());
            }
            // Cerca main.go nel sub-module.
            let candidate = project_root.join(normalized).join("main.go");
            if candidate.is_file() {
                entry_points.push(format!("{}/main.go", normalized));
            }
        }
    } else if go_mod.is_file() {
        sources.push(go_mod.to_string_lossy().into_owned());
        include_globs.push("**/*.go".to_string());
        if project_root.join("main.go").is_file() {
            entry_points.push("main.go".to_string());
        }
    } else {
        anyhow::bail!("nessun go.mod o go.work in {}", project_root.display());
    }

    Ok(BuildGraphInfo {
        project_id,
        language: "go".to_string(),
        include_globs,
        exclude_globs: vec!["vendor/**".to_string()],
        entry_points,
        monorepo_members,
        generated_dirs: vec!["bin".to_string(), "vendor".to_string()],
        sources,
        computed_at: Utc::now(),
    })
}

/// Estrae i path da `use (...)` o `use ./path` in un `go.work`.
fn parse_go_work_uses(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("use") {
            let rest = rest.trim();
            if let Some(_block_start) = rest.strip_prefix('(') {
                in_block = true;
                continue;
            }
            if !rest.is_empty() {
                let val = rest.trim_matches(|c| c == '"' || c == '\'');
                out.push(val.to_string());
            }
            continue;
        }
        if in_block {
            if trimmed.starts_with(')') {
                in_block = false;
                continue;
            }
            if !trimmed.is_empty() {
                let val = trimmed.trim_matches(|c| c == '"' || c == '\'');
                out.push(val.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn single_module_go_mod() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(root.join("go.mod"), "module example.com/x\ngo 1.21\n")
            .await
            .unwrap();
        tokio::fs::write(root.join("main.go"), "package main\nfunc main(){}\n")
            .await
            .unwrap();
        let info = resolve_go(Uuid::nil(), root).await.unwrap();
        assert_eq!(info.language, "go");
        assert!(info.include_globs.contains(&"**/*.go".to_string()));
        assert!(info.entry_points.contains(&"main.go".to_string()));
        assert!(info.exclude_globs.contains(&"vendor/**".to_string()));
    }

    #[tokio::test]
    async fn workspace_go_work_block() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n  ./api\n  ./worker\n)\n",
        )
        .await
        .unwrap();
        let info = resolve_go(Uuid::nil(), root).await.unwrap();
        assert!(info.include_globs.contains(&"api/**".to_string()));
        assert!(info.include_globs.contains(&"worker/**".to_string()));
        assert!(info.monorepo_members.contains(&"api".to_string()));
    }
}
