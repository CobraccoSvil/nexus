//! Resolver TypeScript/JavaScript: parsing di `tsconfig.json` (e varianti
//! `tsconfig.app.json`, `tsconfig.build.json`, `tsconfig.node.json`) con
//! gestione di `extends` (ricorsivo + cycle detection) e `references`.
//!
//! Per i monorepo legge `package.json` (`workspaces`) e `pnpm-workspace.yaml`.
//! Usa `json5` per tollerare commenti e trailing comma comuni nei tsconfig.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::model::BuildGraphInfo;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TsConfigRaw {
    extends: Option<String>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    references: Vec<TsReference>,
    #[serde(default)]
    compiler_options: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
struct TsReference {
    path: String,
}

#[derive(Debug, Deserialize, Default)]
struct PackageJsonRaw {
    workspaces: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct PnpmWorkspaceRaw {
    #[serde(default)]
    packages: Vec<String>,
}

/// Risolve il build graph TypeScript.
pub async fn resolve_typescript(
    project_id: Uuid,
    project_root: &Path,
) -> anyhow::Result<BuildGraphInfo> {
    let mut sources: Vec<String> = Vec::new();
    let mut merged_include: HashSet<String> = HashSet::new();
    let mut merged_exclude: HashSet<String> = HashSet::new();
    let mut merged_files: HashSet<String> = HashSet::new();

    // Lista di candidati tsconfig: principale + varianti note.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for name in [
        "tsconfig.json",
        "tsconfig.app.json",
        "tsconfig.build.json",
        "tsconfig.node.json",
    ] {
        let p = project_root.join(name);
        if p.is_file() {
            candidates.push(p);
        }
    }
    // Pattern jsconfig.json (project JS senza TS) come fallback.
    if candidates.is_empty() {
        let p = project_root.join("jsconfig.json");
        if p.is_file() {
            candidates.push(p);
        }
    }
    if candidates.is_empty() {
        anyhow::bail!(
            "nessun tsconfig.json (o variante) trovato in {}",
            project_root.display()
        );
    }

    // Per references seguite ricorsivamente (project references TS) usiamo
    // un set visited globale: previene cicli e duplicazione lavoro.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    // BFS: parti dai candidati, processa references successive.
    let mut queue: Vec<PathBuf> = candidates.clone();
    while let Some(cfg_path) = queue.pop() {
        let canonical = cfg_path.canonicalize().unwrap_or_else(|_| cfg_path.clone());
        if !visited.insert(canonical.clone()) {
            continue;
        }
        // Parsa la catena extends di QUESTO file.
        let mut extends_chain: HashSet<PathBuf> = HashSet::new();
        let merged = match parse_tsconfig_with_extends(&cfg_path, &mut extends_chain).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %cfg_path.display(),
                    error = %e,
                    "build_graph.ts: skip tsconfig non parsabile"
                );
                continue;
            }
        };
        sources.push(cfg_path.to_string_lossy().into_owned());
        for s in &merged.include {
            merged_include.insert(s.clone());
        }
        for s in &merged.exclude {
            merged_exclude.insert(s.clone());
        }
        for s in &merged.files {
            merged_files.insert(s.clone());
        }
        // Project references: risolvi path relativo al tsconfig corrente.
        let base = cfg_path.parent().unwrap_or(project_root);
        for r in &merged.references {
            // Path TS reference puo' essere directory (allora cerca tsconfig.json)
            // o file diretto.
            let candidate = base.join(&r.path);
            let resolved = if candidate.is_dir() {
                candidate.join("tsconfig.json")
            } else {
                candidate
            };
            if resolved.is_file() {
                queue.push(resolved);
            }
        }
    }

    // Default TS per `include`: se vuoto, equivale a "**/*".
    if merged_include.is_empty() && merged_files.is_empty() {
        merged_include.insert("**/*".to_string());
    }
    // Default `exclude`: TS standard.
    if merged_exclude.is_empty() {
        merged_exclude.insert("node_modules/**".to_string());
        merged_exclude.insert("dist/**".to_string());
        merged_exclude.insert("build/**".to_string());
        merged_exclude.insert(".next/**".to_string());
    } else {
        // node_modules e dist sono sempre esclusi anche se non listati.
        merged_exclude.insert("node_modules/**".to_string());
    }

    // Monorepo via package.json workspaces.
    let mut monorepo_members: Vec<String> = Vec::new();
    let pkg_path = project_root.join("package.json");
    if pkg_path.is_file() {
        sources.push(pkg_path.to_string_lossy().into_owned());
        if let Ok(raw) = tokio::fs::read_to_string(&pkg_path).await {
            if let Ok(pkg) = serde_json::from_str::<PackageJsonRaw>(&raw) {
                if let Some(ws) = pkg.workspaces {
                    let patterns = extract_workspace_patterns(&ws);
                    for pat in patterns {
                        monorepo_members.push(pat.clone());
                        merged_include.insert(format!("{}/**", pat.trim_end_matches('/')));
                    }
                }
            }
        }
    }
    let pnpm_path = project_root.join("pnpm-workspace.yaml");
    if pnpm_path.is_file() {
        sources.push(pnpm_path.to_string_lossy().into_owned());
        if let Ok(raw) = tokio::fs::read_to_string(&pnpm_path).await {
            // Parse minimalista: cerchiamo righe "- pattern" o "packages:" YAML.
            // Non importiamo una dep YAML pesante: il formato e' semplicissimo.
            for pat in parse_pnpm_workspace_packages(&raw) {
                monorepo_members.push(pat.clone());
                merged_include.insert(format!("{}/**", pat.trim_end_matches('/')));
            }
        }
    }

    // Entry point convenzionali.
    let mut entry_points: Vec<String> = Vec::new();
    for ep in [
        "src/main.ts",
        "src/main.tsx",
        "src/index.ts",
        "src/index.tsx",
        "src/App.tsx",
        "src/app/page.tsx",
        "src/server.ts",
        "index.ts",
    ] {
        if project_root.join(ep).is_file() {
            entry_points.push(ep.to_string());
        }
    }
    // Entry point espliciti da `files`.
    for f in &merged_files {
        entry_points.push(f.clone());
    }

    let include_globs: Vec<String> = sort_set(&merged_include);
    let exclude_globs: Vec<String> = sort_set(&merged_exclude);

    Ok(BuildGraphInfo {
        project_id,
        language: "typescript".to_string(),
        include_globs,
        exclude_globs,
        entry_points,
        monorepo_members,
        generated_dirs: vec![
            "node_modules".to_string(),
            "dist".to_string(),
            "build".to_string(),
            ".next".to_string(),
            ".turbo".to_string(),
        ],
        sources,
        computed_at: Utc::now(),
    })
}

#[derive(Debug, Default)]
struct MergedTsConfig {
    include: Vec<String>,
    exclude: Vec<String>,
    files: Vec<String>,
    references: Vec<TsReference>,
}

/// Parsa un tsconfig.json risolvendo `extends` ricorsivamente.
/// `visited` tiene traccia dei file gia' visti nella catena extends per
/// rilevare cicli.
fn parse_tsconfig_with_extends<'a>(
    cfg_path: &'a Path,
    visited: &'a mut HashSet<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<MergedTsConfig>> + Send + 'a>>
{
    Box::pin(async move {
        let canonical = cfg_path
            .canonicalize()
            .unwrap_or_else(|_| cfg_path.to_path_buf());
        if !visited.insert(canonical.clone()) {
            anyhow::bail!("ciclo extends rilevato su tsconfig: {}", cfg_path.display());
        }
        let raw = tokio::fs::read_to_string(cfg_path)
            .await
            .map_err(|e| anyhow::anyhow!("lettura {}: {}", cfg_path.display(), e))?;
        // json5 tollera commenti e trailing comma comuni nei tsconfig generati
        // da framework (Next.js, CRA, Vite).
        let parsed: TsConfigRaw = json5::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse {} (json5): {}", cfg_path.display(), e))?;

        // Se c'e' extends, risolvi base + merge (le proprieta' del figlio sovrascrivono).
        let mut base = MergedTsConfig::default();
        if let Some(ref ext) = parsed.extends {
            let base_dir = cfg_path.parent().unwrap_or_else(|| Path::new("."));
            // `extends` puo' essere:
            //  - path relativo (./tsconfig.base.json)
            //  - path con estensione omessa (./tsconfig.base)
            //  - nome pacchetto npm (@tsconfig/node20/tsconfig.json) — best-effort:
            //    lo skippiamo (non possiamo risolvere senza node_modules walking).
            let candidate = if ext.starts_with('.') || ext.starts_with('/') {
                let mut p = base_dir.join(ext);
                if !p.exists() && p.extension().is_none() {
                    p.set_extension("json");
                }
                Some(p)
            } else {
                None
            };
            if let Some(p) = candidate {
                if p.is_file() {
                    match parse_tsconfig_with_extends(&p, visited).await {
                        Ok(parent_merged) => base = parent_merged,
                        Err(e) => tracing::debug!(error = %e, "extends parent skip"),
                    }
                }
            }
        }

        // Merge: include/exclude/files del figlio SOVRASCRIVONO il padre se non vuoti
        // (semantica TS standard).
        if !parsed.include.is_empty() {
            base.include = parsed.include;
        }
        if !parsed.exclude.is_empty() {
            base.exclude = parsed.exclude;
        }
        if !parsed.files.is_empty() {
            base.files = parsed.files;
        }
        if !parsed.references.is_empty() {
            // References si accumulano lungo la catena (non sovrascrivono).
            base.references.extend(parsed.references);
        }

        Ok(base)
    })
}

fn extract_workspace_patterns(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        serde_json::Value::Object(obj) => obj
            .get("packages")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_pnpm_workspace_packages(raw: &str) -> Vec<String> {
    // Tenta serde_yaml-like via parser ad-hoc: linee `  - "pattern"` o `  - pattern`
    // dopo la chiave `packages:`. Sufficiente per il formato canonico pnpm.
    let mut out = Vec::new();
    let mut in_packages = false;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages {
            if let Some(rest) = trimmed.strip_prefix('-') {
                let val = rest
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string();
                if !val.is_empty() {
                    out.push(val);
                }
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                // Linea senza '-' che non e' un commento → sezione finita.
                in_packages = false;
            }
        }
    }
    out
}

fn sort_set(s: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = s.iter().cloned().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn beauty_book_like_include_src() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("tsconfig.json"),
            r#"{
              // commento permesso da json5
              "compilerOptions": { "strict": true, },
              "include": ["src"],
            }"#,
        )
        .await
        .unwrap();
        let info = resolve_typescript(Uuid::nil(), root).await.unwrap();
        assert_eq!(info.language, "typescript");
        assert!(info.include_globs.contains(&"src".to_string()));
        assert!(info.exclude_globs.contains(&"node_modules/**".to_string()));
        assert!(info.generated_dirs.contains(&"node_modules".to_string()));
    }

    #[tokio::test]
    async fn extends_resolves_parent_include() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("tsconfig.base.json"),
            r#"{ "include": ["packages"], "exclude": ["packages/legacy/**"] }"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.join("tsconfig.json"),
            r#"{ "extends": "./tsconfig.base.json" }"#,
        )
        .await
        .unwrap();
        let info = resolve_typescript(Uuid::nil(), root).await.unwrap();
        assert!(info.include_globs.contains(&"packages".to_string()));
        assert!(info
            .exclude_globs
            .contains(&"packages/legacy/**".to_string()));
    }

    #[tokio::test]
    async fn pnpm_workspace_packages_parsed() {
        let raw = "packages:\n  - 'apps/*'\n  - \"packages/*\"\n";
        let out = parse_pnpm_workspace_packages(raw);
        assert_eq!(out, vec!["apps/*".to_string(), "packages/*".to_string()]);
    }

    #[tokio::test]
    async fn missing_tsconfig_is_error() {
        let dir = TempDir::new().unwrap();
        let res = resolve_typescript(Uuid::nil(), dir.path()).await;
        assert!(res.is_err());
    }
}
