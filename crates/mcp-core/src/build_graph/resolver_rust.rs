//! Resolver Rust: legge `Cargo.toml` (workspace o singolo crate) e deriva
//! include glob, exclude, entry point e generated dirs.
//!
//! Strategie:
//! - Workspace: `[workspace].members` → ogni member (anche pattern glob come
//!   `"crates/*"`) entra negli include come `"<member>/**"`. `[workspace].exclude`
//!   diventa exclude. Entry point per ogni member: `<member>/src/main.rs`,
//!   `<member>/src/lib.rs`.
//! - Crate singolo: `src/**` come include base. `[[bin]] path` e `[lib] path`
//!   custom vengono aggiunti come glob singoli.
//! - Generated dirs: sempre `["target"]`.

use std::path::Path;

use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::model::BuildGraphInfo;

#[derive(Debug, Deserialize, Default)]
struct CargoTomlRoot {
    workspace: Option<CargoWorkspace>,
    package: Option<CargoPackage>,
    bin: Option<Vec<CargoBin>>,
    lib: Option<CargoLib>,
}

#[derive(Debug, Deserialize, Default)]
struct CargoWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct CargoPackage {
    #[allow(dead_code)]
    name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct CargoBin {
    #[allow(dead_code)]
    name: Option<String>,
    path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct CargoLib {
    path: Option<String>,
}

/// Risolve il build graph di un progetto Rust.
///
/// Errori: ritorna `Err` se `Cargo.toml` non esiste o non e' parsabile come TOML.
pub async fn resolve_rust(project_id: Uuid, project_root: &Path) -> anyhow::Result<BuildGraphInfo> {
    let cargo_path = project_root.join("Cargo.toml");
    let raw = tokio::fs::read_to_string(&cargo_path)
        .await
        .map_err(|e| anyhow::anyhow!("lettura Cargo.toml '{}': {}", cargo_path.display(), e))?;
    let parsed: CargoTomlRoot = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("parse Cargo.toml '{}': {}", cargo_path.display(), e))?;

    let mut include_globs: Vec<String> = Vec::new();
    let mut exclude_globs: Vec<String> = vec!["target/**".to_string()];
    let mut entry_points: Vec<String> = Vec::new();
    let mut monorepo_members: Vec<String> = Vec::new();
    let mut sources: Vec<String> = vec![cargo_path.to_string_lossy().into_owned()];

    if let Some(ws) = parsed.workspace {
        for member in &ws.members {
            // I member possono essere glob ("crates/*") o path concreti ("apps/cli").
            // Normalizziamo aggiungendo "/**" per la regola di membership.
            let normalized = member.trim_end_matches('/').to_string();
            include_globs.push(format!("{}/**", normalized));
            monorepo_members.push(normalized.clone());
            // Best-effort discovery degli entry point per member concreti (non glob).
            // Per pattern come `crates/*` non possiamo enumerare senza listare il FS;
            // l'enumerazione esplicita resta fuori scope (i workspace member glob
            // sono comunque coperti dall'include).
            if !normalized.contains('*') {
                let main_rs = format!("{}/src/main.rs", normalized);
                let lib_rs = format!("{}/src/lib.rs", normalized);
                let main_abs = project_root.join(&main_rs);
                let lib_abs = project_root.join(&lib_rs);
                if main_abs.exists() {
                    entry_points.push(main_rs);
                }
                if lib_abs.exists() {
                    entry_points.push(lib_rs);
                }
            }
        }
        for excl in &ws.exclude {
            exclude_globs.push(format!("{}/**", excl.trim_end_matches('/')));
        }
    }

    // Crate singolo (anche dentro un workspace puo' coesistere come root crate).
    if parsed.package.is_some() {
        if include_globs.is_empty() {
            include_globs.push("src/**".to_string());
        }
        // Entry point standard: src/main.rs, src/lib.rs.
        for ep in ["src/main.rs", "src/lib.rs"] {
            if project_root.join(ep).exists() {
                entry_points.push(ep.to_string());
            }
        }
    }

    // Bin custom paths.
    if let Some(bins) = parsed.bin {
        for bin in bins {
            if let Some(path) = bin.path {
                include_globs.push(path.clone());
                entry_points.push(path);
            }
        }
    }
    // Lib custom path.
    if let Some(lib) = parsed.lib {
        if let Some(path) = lib.path {
            include_globs.push(path.clone());
            entry_points.push(path);
        }
    }

    // Se siamo in un workspace puro senza package root, include_globs ha solo
    // i pattern dei member. Va bene cosi: i file fuori dai member sono OOG.
    if include_globs.is_empty() {
        include_globs.push("src/**".to_string());
    }

    // De-duplica preservando ordine.
    dedup_preserve_order(&mut include_globs);
    dedup_preserve_order(&mut exclude_globs);
    dedup_preserve_order(&mut entry_points);
    dedup_preserve_order(&mut monorepo_members);
    dedup_preserve_order(&mut sources);

    Ok(BuildGraphInfo {
        project_id,
        language: "rust".to_string(),
        include_globs,
        exclude_globs,
        entry_points,
        monorepo_members,
        generated_dirs: vec!["target".to_string()],
        sources,
        computed_at: Utc::now(),
    })
}

fn dedup_preserve_order(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn workspace_with_glob_members() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["crates/*", "apps/cli"]
exclude = ["legacy/old"]
resolver = "2"
"#,
        )
        .await
        .unwrap();

        let info = resolve_rust(Uuid::nil(), root).await.unwrap();
        assert_eq!(info.language, "rust");
        assert!(info.include_globs.contains(&"crates/*/**".to_string()));
        assert!(info.include_globs.contains(&"apps/cli/**".to_string()));
        assert!(info.exclude_globs.contains(&"legacy/old/**".to_string()));
        assert!(info.exclude_globs.contains(&"target/**".to_string()));
        assert_eq!(info.generated_dirs, vec!["target".to_string()]);
        assert!(info.monorepo_members.contains(&"crates/*".to_string()));
    }

    #[tokio::test]
    async fn single_crate_with_main_rs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("Cargo.toml"),
            r#"
[package]
name = "foo"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::write(root.join("src/main.rs"), "fn main() {}")
            .await
            .unwrap();

        let info = resolve_rust(Uuid::nil(), root).await.unwrap();
        assert!(info.include_globs.contains(&"src/**".to_string()));
        assert!(info.entry_points.contains(&"src/main.rs".to_string()));
    }

    #[tokio::test]
    async fn missing_cargo_toml_is_error() {
        let dir = TempDir::new().unwrap();
        let res = resolve_rust(Uuid::nil(), dir.path()).await;
        assert!(res.is_err());
    }
}
