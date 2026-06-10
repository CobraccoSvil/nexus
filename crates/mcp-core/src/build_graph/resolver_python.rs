//! Resolver Python: priorita' `pyproject.toml` (PEP 518) > `setup.py` > `setup.cfg`.
//!
//! Per pyproject supportiamo i tre layout piu' diffusi:
//!  - poetry:    `[tool.poetry].packages`
//!  - setuptools: `[tool.setuptools.packages.find].include/exclude` o `[tool.setuptools].packages`
//!  - hatchling:  `[tool.hatch.build.targets.wheel].packages`
//!
//! Fallback `setup.py`: parser euristico (cerca `find_packages(...)`); se non
//! riesce, ripiega su "tutte le directory top-level con `__init__.py`".

use std::path::Path;

use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::model::BuildGraphInfo;

#[derive(Debug, Deserialize, Default)]
struct PyprojectRoot {
    #[serde(default)]
    tool: PyprojectTool,
    #[serde(default)]
    project: PyprojectProject,
}

#[derive(Debug, Deserialize, Default)]
struct PyprojectTool {
    #[serde(default)]
    poetry: Option<PoetryConfig>,
    #[serde(default)]
    setuptools: Option<SetuptoolsConfig>,
    #[serde(default)]
    hatch: Option<HatchConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct PyprojectProject {
    #[serde(default)]
    scripts: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
struct PoetryConfig {
    #[serde(default)]
    packages: Vec<PoetryPackage>,
}

#[derive(Debug, Deserialize, Default)]
struct PoetryPackage {
    include: String,
    #[serde(default)]
    from: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SetuptoolsConfig {
    #[serde(default)]
    packages: SetuptoolsPackages,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum SetuptoolsPackages {
    #[default]
    None,
    List(Vec<String>),
    Find {
        find: SetuptoolsFind,
    },
}

#[derive(Debug, Deserialize, Default)]
struct SetuptoolsFind {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default, rename = "where")]
    where_: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct HatchConfig {
    #[serde(default)]
    build: HatchBuild,
}

#[derive(Debug, Deserialize, Default)]
struct HatchBuild {
    #[serde(default)]
    targets: HatchTargets,
}

#[derive(Debug, Deserialize, Default)]
struct HatchTargets {
    #[serde(default)]
    wheel: HatchWheel,
}

#[derive(Debug, Deserialize, Default)]
struct HatchWheel {
    #[serde(default)]
    packages: Vec<String>,
}

pub async fn resolve_python(
    project_id: Uuid,
    project_root: &Path,
) -> anyhow::Result<BuildGraphInfo> {
    let mut include_globs: Vec<String> = Vec::new();
    let mut exclude_globs: Vec<String> = vec![
        "__pycache__/**".to_string(),
        "*.egg-info/**".to_string(),
        ".venv/**".to_string(),
        "venv/**".to_string(),
    ];
    let mut entry_points: Vec<String> = Vec::new();
    let mut sources: Vec<String> = Vec::new();

    let pyproject = project_root.join("pyproject.toml");
    if pyproject.is_file() {
        sources.push(pyproject.to_string_lossy().into_owned());
        let raw = tokio::fs::read_to_string(&pyproject).await?;
        // Parse tollerante: se fallisce passiamo al fallback.
        if let Ok(parsed) = toml::from_str::<PyprojectRoot>(&raw) {
            extract_pyproject_packages(&parsed, &mut include_globs, &mut exclude_globs);
            // scripts in [project.scripts] sono potenziali entry point
            // (formato "name = module:func", non un path concreto, ma utile come hint).
            for name in parsed.project.scripts.keys() {
                entry_points.push(format!("[script:{}]", name));
            }
        }
    } else if project_root.join("setup.py").is_file() {
        let setup_path = project_root.join("setup.py");
        sources.push(setup_path.to_string_lossy().into_owned());
        if let Ok(content) = tokio::fs::read_to_string(&setup_path).await {
            extract_setup_py_packages(&content, &mut include_globs);
        }
    } else if project_root.join("setup.cfg").is_file() {
        let cfg_path = project_root.join("setup.cfg");
        sources.push(cfg_path.to_string_lossy().into_owned());
        // setup.cfg parsing minimo: cerca `packages = find:` o lista esplicita.
        if let Ok(content) = tokio::fs::read_to_string(&cfg_path).await {
            extract_setup_cfg_packages(&content, &mut include_globs);
        }
    } else {
        anyhow::bail!(
            "nessun pyproject.toml / setup.py / setup.cfg in {}",
            project_root.display()
        );
    }

    // Fallback finale: se nessun pacchetto rilevato, cerca top-level con __init__.py.
    if include_globs.is_empty() {
        if let Ok(mut rd) = tokio::fs::read_dir(project_root).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                if entry.path().is_dir() {
                    if entry.path().join("__init__.py").exists() {
                        if let Some(name) = entry.file_name().to_str() {
                            include_globs.push(format!("{}/**", name));
                        }
                    }
                }
            }
        }
        if include_globs.is_empty() {
            // Ultimo fallback: include tutti i .py.
            include_globs.push("**/*.py".to_string());
        }
    }

    // Discovery euristica entry point: cerca file top-level main.py / app.py / __main__.py.
    for ep in ["main.py", "app.py", "__main__.py", "manage.py"] {
        if project_root.join(ep).is_file() {
            entry_points.push(ep.to_string());
        }
    }

    // De-duplica.
    dedup_preserve_order(&mut include_globs);
    dedup_preserve_order(&mut exclude_globs);
    dedup_preserve_order(&mut entry_points);
    dedup_preserve_order(&mut sources);

    Ok(BuildGraphInfo {
        project_id,
        language: "python".to_string(),
        include_globs,
        exclude_globs,
        entry_points,
        monorepo_members: vec![],
        generated_dirs: vec![
            "dist".to_string(),
            "build".to_string(),
            "__pycache__".to_string(),
            ".pytest_cache".to_string(),
            ".tox".to_string(),
            "*.egg-info".to_string(),
        ],
        sources,
        computed_at: Utc::now(),
    })
}

fn extract_pyproject_packages(
    parsed: &PyprojectRoot,
    include: &mut Vec<String>,
    exclude: &mut Vec<String>,
) {
    // Poetry.
    if let Some(poetry) = &parsed.tool.poetry {
        for pkg in &poetry.packages {
            let base = pkg
                .from
                .clone()
                .map(|f| format!("{}/{}", f.trim_end_matches('/'), pkg.include))
                .unwrap_or_else(|| pkg.include.clone());
            include.push(format!("{}/**", base.trim_end_matches('/')));
        }
    }
    // Setuptools.
    if let Some(setup) = &parsed.tool.setuptools {
        match &setup.packages {
            SetuptoolsPackages::None => {}
            SetuptoolsPackages::List(list) => {
                for pkg in list {
                    // Pacchetti come "foo.bar" → "foo/bar/**".
                    include.push(format!("{}/**", pkg.replace('.', "/")));
                }
            }
            SetuptoolsPackages::Find { find } => {
                for w in &find.where_ {
                    include.push(format!("{}/**", w.trim_end_matches('/')));
                }
                for inc in &find.include {
                    // Pattern "foo*" → glob "foo*/**".
                    include.push(format!("{}/**", inc.trim_end_matches('/')));
                }
                for exc in &find.exclude {
                    exclude.push(format!("{}/**", exc.trim_end_matches('/')));
                }
            }
        }
    }
    // Hatchling.
    if let Some(hatch) = &parsed.tool.hatch {
        for pkg in &hatch.build.targets.wheel.packages {
            include.push(format!("{}/**", pkg.trim_end_matches('/')));
        }
    }
}

fn extract_setup_py_packages(content: &str, include: &mut Vec<String>) {
    // Euristica: cerca `find_packages(...)` o `packages=[...]`.
    // Se troviamo find_packages senza argomenti specifici → include "**/*.py".
    if content.contains("find_packages(") || content.contains("find_namespace_packages(") {
        include.push("**/*.py".to_string());
        return;
    }
    // packages=["foo", "bar.baz"]
    if let Some(start) = content.find("packages=[") {
        let rest = &content[start + 10..];
        if let Some(end) = rest.find(']') {
            let list = &rest[..end];
            for raw in list.split(',') {
                let pkg = raw.trim().trim_matches(|c| c == '"' || c == '\'');
                if !pkg.is_empty() {
                    include.push(format!("{}/**", pkg.replace('.', "/")));
                }
            }
        }
    }
}

fn extract_setup_cfg_packages(content: &str, include: &mut Vec<String>) {
    let mut in_options = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_options = trimmed == "[options]";
            continue;
        }
        if !in_options {
            continue;
        }
        if let Some(val) = trimmed.strip_prefix("packages") {
            let val = val.trim_start_matches(|c: char| c == '=' || c.is_whitespace());
            if val == "find:" || val == "find_namespace:" {
                include.push("**/*.py".to_string());
            } else {
                for pkg in val.split(|c: char| c == ',' || c.is_whitespace()) {
                    let pkg = pkg.trim();
                    if !pkg.is_empty() {
                        include.push(format!("{}/**", pkg.replace('.', "/")));
                    }
                }
            }
        }
    }
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
    async fn poetry_packages_resolved() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("pyproject.toml"),
            r#"
[tool.poetry]
name = "demo"
version = "0.1.0"

[[tool.poetry.packages]]
include = "demo_pkg"
from = "src"
"#,
        )
        .await
        .unwrap();
        let info = resolve_python(Uuid::nil(), root).await.unwrap();
        assert_eq!(info.language, "python");
        assert!(info.include_globs.iter().any(|g| g.contains("demo_pkg")));
    }

    #[tokio::test]
    async fn setuptools_find_uses_where() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("pyproject.toml"),
            r#"
[project]
name = "x"

[tool.setuptools.packages.find]
where = ["src"]
include = ["mypkg*"]
exclude = ["tests*"]
"#,
        )
        .await
        .unwrap();
        let info = resolve_python(Uuid::nil(), root).await.unwrap();
        assert!(info.include_globs.contains(&"src/**".to_string()));
        assert!(info.exclude_globs.iter().any(|g| g.starts_with("tests")));
    }

    #[tokio::test]
    async fn fallback_init_py_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n")
            .await
            .unwrap();
        tokio::fs::create_dir(root.join("mypkg")).await.unwrap();
        tokio::fs::write(root.join("mypkg/__init__.py"), "")
            .await
            .unwrap();
        let info = resolve_python(Uuid::nil(), root).await.unwrap();
        assert!(info.include_globs.iter().any(|g| g.starts_with("mypkg")));
    }
}
