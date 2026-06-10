//! API runtime: dato un file path, determina se appartiene al build graph
//! del progetto. Usata dal preflight di `write_file`/`edit_file`.
//!
//! Logica:
//! 1. Path relativo a `repository_root_path`.
//! 2. Se il primo componente e' in `generated_dirs` → `Generated`.
//! 3. Se uno degli `exclude_globs` matcha → `OutOfGraph`.
//! 4. Se path == uno degli `entry_points` → `Entrypoint`.
//! 5. Se uno degli `include_globs` matcha → `InGraph`.
//! 6. Altrimenti → `OutOfGraph`.
//!
//! Glob compilati al volo tramite `globset` (`GlobSet`).

use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSetBuilder};
use sqlx::Row;
use uuid::Uuid;

use super::cache::BuildGraphCache;
use super::model::{BuildGraphInfo, BuildGraphMembership};

/// Verifica la membership di `file_path` (relativo o assoluto) rispetto al
/// build graph del progetto `project_id`.
///
/// `file_path` puo' essere assoluto o relativo: se assoluto, viene
/// strippato dal `repository_root_path`.
pub async fn is_in_build_graph(
    project_id: Uuid,
    file_path: &Path,
) -> anyhow::Result<BuildGraphMembership> {
    let cache = BuildGraphCache::global()
        .ok_or_else(|| anyhow::anyhow!("BuildGraphCache non inizializzato"))?;
    let info = cache.get_or_compute(project_id).await?;

    let rel = normalize_rel_path(&cache, project_id, file_path).await?;

    membership_for(&info, &rel)
}

/// Variante sincrona della logica di matching, usata dai test e da
/// `is_in_build_graph` dopo aver recuperato info + rel path.
pub(crate) fn membership_for(
    info: &BuildGraphInfo,
    rel: &Path,
) -> anyhow::Result<BuildGraphMembership> {
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let rel_str = rel_str.trim_start_matches("./").to_string();

    // 1) Generated dirs: check sui primi componenti.
    let first_component = rel
        .components()
        .find_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    for gen_dir in &info.generated_dirs {
        // Match esatto sul primo segmento oppure pattern glob (per *.egg-info).
        if gen_dir == &first_component {
            return Ok(BuildGraphMembership::Generated {
                reason: format!("path in directory generata '{}'", gen_dir),
            });
        }
        if gen_dir.contains('*') {
            if let Ok(g) = Glob::new(gen_dir) {
                if g.compile_matcher().is_match(&first_component) {
                    return Ok(BuildGraphMembership::Generated {
                        reason: format!(
                            "path in directory generata (pattern '{}' matcha primo segmento '{}')",
                            gen_dir, first_component
                        ),
                    });
                }
            }
        }
    }

    // Stessa logica anche per node_modules embedded sotto sub-package (monorepo).
    if rel_str.contains("/node_modules/") || rel_str.starts_with("node_modules/") {
        return Ok(BuildGraphMembership::Generated {
            reason: "path in directory generata 'node_modules' (presente come segmento)"
                .to_string(),
        });
    }

    // 2) Exclude globs.
    if !info.exclude_globs.is_empty() {
        let mut excl_builder = GlobSetBuilder::new();
        let mut excl_kept: Vec<String> = Vec::new();
        for raw in &info.exclude_globs {
            let pat = normalize_glob(raw);
            if let Ok(g) = Glob::new(&pat) {
                excl_builder.add(g);
                excl_kept.push(pat);
            }
        }
        if let Ok(excl_set) = excl_builder.build() {
            let matches = excl_set.matches(&rel_str);
            if let Some(idx) = matches.first() {
                let pat = excl_kept
                    .get(*idx)
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".to_string());
                return Ok(BuildGraphMembership::OutOfGraph {
                    reason: format!("matcha exclude glob '{}'", pat),
                });
            }
        }
    }

    // 3) Entry point.
    for ep in &info.entry_points {
        if ep == &rel_str {
            return Ok(BuildGraphMembership::Entrypoint {
                reason: format!("entrypoint riconosciuto: {}", ep),
            });
        }
    }

    // 4) Include globs.
    if info.include_globs.is_empty() {
        return Ok(BuildGraphMembership::Unknown {
            reason: "nessun include glob disponibile (config non riconosciuto)".to_string(),
        });
    }
    let mut incl_builder = GlobSetBuilder::new();
    let mut incl_kept: Vec<String> = Vec::new();
    for raw in &info.include_globs {
        let pat = normalize_glob(raw);
        if let Ok(g) = Glob::new(&pat) {
            incl_builder.add(g);
            incl_kept.push(pat);
        }
    }
    let incl_set = incl_builder
        .build()
        .map_err(|e| anyhow::anyhow!("compilazione include globs fallita: {}", e))?;
    let matches = incl_set.matches(&rel_str);
    if let Some(idx) = matches.first() {
        let pat = incl_kept
            .get(*idx)
            .cloned()
            .unwrap_or_else(|| "<unknown>".to_string());
        return Ok(BuildGraphMembership::InGraph {
            reason: format!("matcha include glob '{}'", pat),
        });
    }

    Ok(BuildGraphMembership::OutOfGraph {
        reason: format!(
            "nessun include glob matchato (include: {})",
            info.include_globs.join(", ")
        ),
    })
}

/// Risolve il path relativo del file rispetto al `repository_root_path`.
async fn normalize_rel_path(
    cache: &BuildGraphCache,
    project_id: Uuid,
    file_path: &Path,
) -> anyhow::Result<PathBuf> {
    if file_path.is_relative() {
        return Ok(file_path.to_path_buf());
    }
    let row = sqlx::query("SELECT repository_root_path FROM projects WHERE id = $1 LIMIT 1")
        .bind(project_id)
        .fetch_optional(cache.db())
        .await?
        .ok_or_else(|| anyhow::anyhow!("project_id {} non trovato", project_id))?;
    let root_str: String = row.try_get("repository_root_path").unwrap_or_default();
    let root = PathBuf::from(root_str);
    Ok(file_path
        .strip_prefix(&root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| file_path.to_path_buf()))
}

/// Normalizza un pattern glob in stile "directory" verso un pattern globset.
/// - "src" → "src/**" (consente sia directory che descendant)
/// - "src/**" rimane invariato
/// - Pattern con `*` rimane invariato.
fn normalize_glob(raw: &str) -> String {
    let p = raw.replace('\\', "/");
    if p.contains('*') || p.contains('?') {
        return p;
    }
    // Pattern non glob: tratto come directory → match path con prefisso.
    format!("{}/**", p.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_graph::model::BuildGraphInfo;
    use chrono::Utc;

    fn sample_info() -> BuildGraphInfo {
        BuildGraphInfo {
            project_id: Uuid::nil(),
            language: "typescript".into(),
            include_globs: vec!["src".into()],
            exclude_globs: vec!["src/legacy/**".into()],
            entry_points: vec!["src/main.ts".into()],
            monorepo_members: vec![],
            generated_dirs: vec!["node_modules".into(), "dist".into(), "build".into()],
            sources: vec!["/tmp/tsconfig.json".into()],
            computed_at: Utc::now(),
        }
    }

    #[test]
    fn beauty_book_src_is_in_graph() {
        let info = sample_info();
        let m = membership_for(&info, Path::new("src/app/pages/BookingPage.tsx")).unwrap();
        assert!(
            matches!(m, BuildGraphMembership::InGraph { .. }),
            "got {:?}",
            m
        );
    }

    #[test]
    fn figma_export_out_of_graph() {
        let info = sample_info();
        let m = membership_for(
            &info,
            Path::new("figma_export/src/app/pages/BookingPage.tsx"),
        )
        .unwrap();
        assert!(
            matches!(m, BuildGraphMembership::OutOfGraph { .. }),
            "got {:?}",
            m
        );
    }

    #[test]
    fn node_modules_is_generated() {
        let info = sample_info();
        let m = membership_for(&info, Path::new("node_modules/react/index.js")).unwrap();
        assert!(
            matches!(m, BuildGraphMembership::Generated { .. }),
            "got {:?}",
            m
        );
    }

    #[test]
    fn entrypoint_detected() {
        let info = sample_info();
        let m = membership_for(&info, Path::new("src/main.ts")).unwrap();
        assert!(
            matches!(m, BuildGraphMembership::Entrypoint { .. }),
            "got {:?}",
            m
        );
    }

    #[test]
    fn exclude_glob_wins() {
        let info = sample_info();
        let m = membership_for(&info, Path::new("src/legacy/oldfile.ts")).unwrap();
        assert!(
            matches!(m, BuildGraphMembership::OutOfGraph { .. }),
            "got {:?}",
            m
        );
    }

    #[test]
    fn rust_workspace_pattern() {
        let info = BuildGraphInfo {
            project_id: Uuid::nil(),
            language: "rust".into(),
            include_globs: vec!["crates/*/**".into()],
            exclude_globs: vec!["target/**".into()],
            entry_points: vec![],
            monorepo_members: vec!["crates/*".into()],
            generated_dirs: vec!["target".into()],
            sources: vec![],
            computed_at: Utc::now(),
        };
        let m = membership_for(&info, Path::new("crates/mcp-core/src/lib.rs")).unwrap();
        assert!(
            matches!(m, BuildGraphMembership::InGraph { .. }),
            "got {:?}",
            m
        );
        let m2 = membership_for(&info, Path::new("docs/readme.md")).unwrap();
        assert!(
            matches!(m2, BuildGraphMembership::OutOfGraph { .. }),
            "got {:?}",
            m2
        );
        let m3 = membership_for(&info, Path::new("target/debug/foo")).unwrap();
        assert!(
            matches!(m3, BuildGraphMembership::Generated { .. }),
            "got {:?}",
            m3
        );
    }
}
