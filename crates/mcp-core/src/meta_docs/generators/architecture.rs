// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/architecture.rs — Genera architecture/*.md
//
// Output:
//   - architecture/crates-rust.md   (mappa per crate dal Cargo.toml workspace)
//   - architecture/brain-python.md  (mappa modulare brain/)
//   - architecture/frontend-nextjs.md (apps/* package.json)
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

pub struct ArchitectureGenerator;

#[async_trait]
impl MetaDocGenerator for ArchitectureGenerator {
    fn name(&self) -> &'static str {
        "architecture"
    }

    fn relevant_for(&self, files: &[String]) -> bool {
        files.is_empty()
            || files.iter().any(|f| {
                f.starts_with("crates/")
                    || f.starts_with("brain/")
                    || f.starts_with("apps/")
                    || f == "Cargo.toml"
                    || f.ends_with("/Cargo.toml")
                    || f.ends_with("/package.json")
            })
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        let now = Utc::now();
        let mut docs = Vec::new();

        if let Ok(body) = generate_crates_rust(ctx).await {
            docs.push(GeneratedDoc {
                kind: "architecture".to_string(),
                title: "Crates Rust".to_string(),
                slug: "crates-rust".to_string(),
                body_md: body,
                tags: vec!["architecture".to_string(), "rust".to_string()],
                source_files: vec!["Cargo.toml".to_string(), "crates/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "architecture/crates-rust.md".to_string(),
                now,
            });
        }

        if let Ok(body) = generate_brain_python(ctx).await {
            docs.push(GeneratedDoc {
                kind: "architecture".to_string(),
                title: "Brain Python".to_string(),
                slug: "brain-python".to_string(),
                body_md: body,
                tags: vec!["architecture".to_string(), "python".to_string()],
                source_files: vec!["brain/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "architecture/brain-python.md".to_string(),
                now,
            });
        }

        if let Ok(body) = generate_frontend(ctx).await {
            docs.push(GeneratedDoc {
                kind: "architecture".to_string(),
                title: "Frontend Next.js".to_string(),
                slug: "frontend-nextjs".to_string(),
                body_md: body,
                tags: vec!["architecture".to_string(), "frontend".to_string()],
                source_files: vec!["apps/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "architecture/frontend-nextjs.md".to_string(),
                now,
            });
        }

        Ok(docs)
    }
}

async fn generate_crates_rust(ctx: &MetaDocContext<'_>) -> Result<String> {
    let mut out = String::new();
    out.push_str("Mappa dei crate Rust nel workspace `ideai`. Generato automaticamente dai `Cargo.toml`.\n\n");

    // Legge il Cargo.toml workspace per ottenere i members
    let workspace_path = format!("{}/Cargo.toml", ctx.repo_root);
    let workspace_content = tokio::fs::read_to_string(&workspace_path).await.unwrap_or_default();

    let mut members: Vec<String> = Vec::new();
    let mut in_members = false;
    for line in workspace_content.lines() {
        let t = line.trim();
        // Detect "members = [" (parte dell'array workspace.members)
        if !in_members {
            // Match esatto "members = [" o "members=[" eventuale spazi
            let normalized = t.replace(' ', "");
            if normalized == "members=[" {
                in_members = true;
            }
            continue;
        }
        // In array members:
        if t == "]" || t.starts_with(']') {
            break;
        }
        let s = t.trim_matches(|c: char| c == ',' || c == '"' || c.is_whitespace());
        // Skip righe vuote, commenti, e righe che iniziano con `[` (sezioni TOML)
        if s.is_empty() || s.starts_with('#') || s.starts_with('[') {
            continue;
        }
        members.push(s.to_string());
    }
    members.sort();

    out.push_str(&format!("Workspace members: **{}**\n\n", members.len()));
    out.push_str("| Crate | Path | Descrizione |\n");
    out.push_str("|---|---|---|\n");

    for m in &members {
        let cargo_path = format!("{}/{m}/Cargo.toml", ctx.repo_root);
        let cargo_content = tokio::fs::read_to_string(&cargo_path).await.unwrap_or_default();
        // Estrae `name` (puo' differire dal path) e `description`
        let name = extract_toml_field(&cargo_content, "name").unwrap_or_else(|| m.clone());
        let desc = extract_toml_field(&cargo_content, "description")
            .unwrap_or_else(|| "(senza descrizione)".to_string());
        out.push_str(&format!("| `{name}` | `{m}` | {desc} |\n"));
    }

    out.push_str("\n---\n\nVedi anche:\n- [[overview]]\n- [[brain-python]]\n- [[frontend-nextjs]]\n- [[nexus-architetturale]]\n- [[pattern-learning-worker]]\n- [[pattern-mcp-tool]]\n- [[multi-provider-routing]]\n");
    Ok(out)
}

fn extract_toml_field(content: &str, field: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package && t.starts_with(&format!("{field} ")) || in_package && t.starts_with(&format!("{field}=")) {
            let value = t.split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
            let value = value.trim_matches('"').trim_matches('\'').to_string();
            return Some(value);
        }
        if in_package && t.starts_with(field) && t.contains('=') {
            let value = t.split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
            let value = value.trim_matches('"').trim_matches('\'').to_string();
            return Some(value);
        }
    }
    None
}

async fn generate_brain_python(ctx: &MetaDocContext<'_>) -> Result<String> {
    let brain_dir = format!("{}/brain", ctx.repo_root);
    let mut top_modules: Vec<String> = Vec::new();

    if let Ok(mut rd) = tokio::fs::read_dir(&brain_dir).await {
        while let Ok(Some(ent)) = rd.next_entry().await {
            if ent.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let name = ent.file_name().to_string_lossy().to_string();
                if !name.starts_with('.') && !name.starts_with('_') && name != "__pycache__" {
                    top_modules.push(name);
                }
            }
        }
    }
    top_modules.sort();

    let mut out = String::new();
    out.push_str("Mappa modulare di `brain/` (Python + FastAPI + LangGraph). Generato automaticamente.\n\n");
    out.push_str("Vedi anche: [[crates-rust]], [[overview]], [[multi-provider-routing]], [[nexus-architetturale]].\n\n");
    out.push_str("## Top-level modules\n\n");
    for m in &top_modules {
        out.push_str(&format!("### `{m}/`\n\n"));
        // Cerca un README.md o un docstring nel __init__.py
        let readme_path = format!("{brain_dir}/{m}/README.md");
        let init_path = format!("{brain_dir}/{m}/__init__.py");
        let readme = tokio::fs::read_to_string(&readme_path).await.ok();
        if let Some(r) = readme {
            // Prendi i primi 200 char come summary
            let summary: String = r.chars().take(200).collect();
            out.push_str(&format!("{summary}{}\n\n", if r.len() > 200 { "..." } else { "" }));
        } else if let Ok(init) = tokio::fs::read_to_string(&init_path).await {
            // Estrai docstring del modulo
            if let Some(doc) = extract_python_docstring(&init) {
                out.push_str(&format!("{doc}\n\n"));
            } else {
                out.push_str("_(senza docstring)_\n\n");
            }
        } else {
            out.push_str("_(modulo senza README ne docstring)_\n\n");
        }
    }
    Ok(out)
}

fn extract_python_docstring(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    let delims = ["\"\"\"", "'''"];
    for delim in &delims {
        if trimmed.starts_with(delim) {
            let after = &trimmed[delim.len()..];
            if let Some(end) = after.find(delim) {
                return Some(after[..end].trim().to_string());
            }
        }
    }
    None
}

async fn generate_frontend(ctx: &MetaDocContext<'_>) -> Result<String> {
    let apps_dir = format!("{}/apps", ctx.repo_root);
    let mut out = String::new();
    out.push_str("Mappa delle app frontend / TS in `apps/`. Generato automaticamente dai `package.json`.\n\n");
    out.push_str("Vedi anche: [[crates-rust]], [[overview]], [[nexus-architetturale]], [[knowledge-base-funzionamento]].\n\n");
    out.push_str("| App | Name | Versione | Descrizione |\n");
    out.push_str("|---|---|---|---|\n");

    if let Ok(mut rd) = tokio::fs::read_dir(&apps_dir).await {
        let mut entries: Vec<String> = Vec::new();
        while let Ok(Some(ent)) = rd.next_entry().await {
            if ent.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                entries.push(ent.file_name().to_string_lossy().to_string());
            }
        }
        entries.sort();
        for app in entries {
            let pkg_path = format!("{apps_dir}/{app}/package.json");
            let pkg = tokio::fs::read_to_string(&pkg_path).await.ok();
            let (name, version, desc) = if let Some(p) = pkg {
                let v: serde_json::Value = serde_json::from_str(&p).unwrap_or(serde_json::json!({}));
                (
                    v.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                    v.get("version").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                    v.get("description").and_then(|x| x.as_str()).unwrap_or("—").to_string(),
                )
            } else {
                ("(no package.json)".to_string(), "—".to_string(), "—".to_string())
            };
            out.push_str(&format!("| `{app}` | `{name}` | {version} | {desc} |\n"));
        }
    }

    Ok(out)
}
