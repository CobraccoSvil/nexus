// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/changelog.rs — Genera entry changelog/YYYY-MM-DD-*.md
//
// Pipeline:
//   1. Carica ultimo commit non-processato da nexus_meta_doc_changes
//   2. LLM call (purpose `changelog_significance`) per stimare significance 0-1
//   3. Se >= soglia settings('meta_docs.changelog_min_significance', default 0.4):
//        genera entry con sezioni "Cosa cambia", "Perche", "File toccati", "Impatto"
//
// Nota: in questo step inizialmente NON facciamo la LLM call diretta (richiede
// orchestrator/neural channel). Il generator viene wirato in step 12 con il
// MetaDocsRefreshWorker che gli passa un handle al neural client. Per ora
// implementiamo il fallback "significance euristica" basato su numero file
// toccati e parole-chiave nel commit_msg, sufficiente per il valore di default.
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;

pub struct ChangelogGenerator;

#[async_trait]
impl MetaDocGenerator for ChangelogGenerator {
    fn name(&self) -> &'static str {
        "changelog"
    }

    fn relevant_for(&self, _files: &[String]) -> bool {
        // Sempre rilevante; la generation effettiva e' gated dalla significance
        true
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        let Some(commit_sha) = ctx.commit_sha.as_ref() else {
            return Ok(Vec::new());
        };

        // Recupera info commit dalla riga gia' inserita
        let row = sqlx::query(
            r#"
            SELECT commit_msg, files_changed, processed_at
            FROM nexus_meta_doc_changes
            WHERE commit_sha = $1
            "#,
        )
        .bind(commit_sha)
        .fetch_optional(ctx.db)
        .await?;
        let Some(row) = row else {
            return Ok(Vec::new());
        };
        let commit_msg: String = row.try_get("commit_msg").unwrap_or_default();
        let files_changed: Vec<String> = row.try_get("files_changed").unwrap_or_default();
        let processed_at: DateTime<Utc> =
            row.try_get("processed_at").unwrap_or_else(|_| Utc::now());

        // Soglia significance
        let threshold: f32 = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'meta_docs.changelog_min_significance'",
        )
        .fetch_optional(ctx.db)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.4);

        // Significance euristica: parole-chiave + numero file
        let significance = compute_significance_heuristic(&commit_msg, &files_changed);
        // Aggiorna la riga in DB con la significance computata
        let _ = sqlx::query(
            "UPDATE nexus_meta_doc_changes SET significance = $1 WHERE commit_sha = $2",
        )
        .bind(significance)
        .bind(commit_sha)
        .execute(ctx.db)
        .await;

        if significance < threshold {
            tracing::debug!(
                significance,
                threshold,
                commit = %commit_sha,
                "changelog: skip per significance bassa"
            );
            return Ok(Vec::new());
        }

        // Genera entry
        let slug = super::super::vault::slugify(&commit_msg);
        let slug = if slug.is_empty() {
            format!("commit-{}", &commit_sha[..8.min(commit_sha.len())])
        } else {
            slug
        };
        let title = if commit_msg.is_empty() {
            format!("Commit {}", &commit_sha[..8.min(commit_sha.len())])
        } else {
            commit_msg.clone()
        };

        let mut body = String::new();
        body.push_str(&format!("# {title}\n\n"));
        body.push_str(&format!(
            "**Commit**: `{}` ({})\n\n",
            commit_sha,
            processed_at.format("%Y-%m-%d %H:%M UTC")
        ));
        body.push_str(&format!("**Significance**: {:.2}\n\n", significance));
        body.push_str("## File toccati\n\n");
        if files_changed.is_empty() {
            body.push_str("_(nessun file rilevato)_\n\n");
        } else {
            for f in &files_changed {
                body.push_str(&format!("- `{f}`\n"));
            }
            body.push('\n');
        }
        body.push_str("## Cosa cambia\n\n");
        body.push_str(&format!("{commit_msg}\n\n"));
        body.push_str("## Riferimenti\n\n");
        body.push_str("- Vedi diff git: `git show ");
        body.push_str(commit_sha);
        body.push_str("`\n");

        // Wikilink ai documenti correlati in base ai file toccati
        let mut related: Vec<&str> = Vec::new();
        if files_changed.iter().any(|f| f.starts_with("crates/")) {
            related.push("[[crates-rust]]");
        }
        if files_changed.iter().any(|f| f.starts_with("brain/")) {
            related.push("[[brain-python]]");
        }
        if files_changed.iter().any(|f| f.starts_with("apps/")) {
            related.push("[[frontend-nextjs]]");
        }
        if files_changed
            .iter()
            .any(|f| f.starts_with("db/migrations/"))
        {
            related.push("[[postgres-tables]]");
            related.push("[[migrations-log]]");
        }
        if files_changed
            .iter()
            .any(|f| f.contains("router") || f.contains("routes") || f.ends_with("_router.rs"))
        {
            related.push("[[rest-endpoints]]");
        }
        if files_changed
            .iter()
            .any(|f| f.contains("vector_memory") || f.contains("qdrant"))
        {
            related.push("[[qdrant-collections]]");
        }
        if files_changed.iter().any(|f| f.contains("knowledge")) {
            related.push("[[knowledge-base-funzionamento]]");
        }
        if files_changed
            .iter()
            .any(|f| f.contains("meta_docs") || f.contains("meta-docs"))
        {
            related.push("[[meta-vault-architettura]]");
        }
        if files_changed
            .iter()
            .any(|f| f.contains("routing") || f.contains("provider"))
        {
            related.push("[[multi-provider-routing]]");
            related.push("[[routing-matrix]]");
        }
        if !related.is_empty() {
            body.push_str("\n## Documenti correlati\n\n");
            for r in related {
                body.push_str("- ");
                body.push_str(r);
                body.push('\n');
            }
        }

        let vault_file_path =
            super::super::vault::build_vault_path("changelog", &slug, &processed_at);

        Ok(vec![GeneratedDoc {
            kind: "changelog".to_string(),
            title,
            slug,
            body_md: body,
            tags: vec!["changelog".to_string()],
            source_files: files_changed,
            source_commit: Some(commit_sha.clone()),
            vault_file_path,
            now: processed_at,
        }])
    }
}

/// Significance euristica 0-1. Pattern conventional commits (prefix at start):
///   - "feat:", "feature:", "fix:", "refactor:", "perf:", "breaking:", "security:" -> bonus
///   - "chore:", "docs:", "test:", "tests:", "wip:", "tmp:", "style:" -> malus
///   - numero file toccati: piu' file -> piu' significativo (capped a 20)
///   - migrazioni DB toccate -> bonus
fn compute_significance_heuristic(commit_msg: &str, files: &[String]) -> f32 {
    let msg_lower = commit_msg.to_lowercase();

    // Prefix del conventional commit (es. "feat: ..." o "feat(scope): ...")
    let prefix: &str = msg_lower
        .split(':')
        .next()
        .unwrap_or("")
        .split('(')
        .next()
        .unwrap_or("")
        .trim();

    let mut score: f32 = 0.3;

    // Bonus per tipi "forti"
    if matches!(
        prefix,
        "feat" | "feature" | "fix" | "refactor" | "perf" | "breaking" | "security" | "api"
    ) {
        score += 0.15;
    }

    // Malus per tipi "deboli"
    if matches!(
        prefix,
        "chore" | "docs" | "doc" | "test" | "tests" | "wip" | "tmp" | "style" | "ci" | "build"
    ) {
        score -= 0.15;
    }

    // Numero file (cap 20)
    let n = files.len().min(20) as f32;
    score += (n / 20.0) * 0.3;

    // Migrazioni DB: bonus deciso
    if files.iter().any(|f| f.starts_with("db/migrations/")) {
        score += 0.2;
    }

    score.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_feat_high() {
        let s = compute_significance_heuristic(
            "feat: add knowledge meta-docs vault",
            &["a.rs".into(), "b.rs".into()],
        );
        assert!(s >= 0.45, "got {s}");
    }

    #[test]
    fn heuristic_docs_low() {
        let s = compute_significance_heuristic("docs: fix typo", &["README.md".into()]);
        assert!(s <= 0.35, "got {s}");
    }

    #[test]
    fn heuristic_migration_bonus() {
        let s = compute_significance_heuristic("fix: schema", &["db/migrations/0177.sql".into()]);
        assert!(s >= 0.55, "got {s}");
    }
}
