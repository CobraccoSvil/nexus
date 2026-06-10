// ═══════════════════════════════════════════════════════════════════════════
// wiki/reingest.rs — Worker one-shot di re-ingest dai vault Markdown.
//
// Implementa la Fase 3 dell'ADR 0017 v2: ricarica `wiki_docs` (e Qdrant
// `wiki_content`) a partire dai file `.md` presenti nei vault:
//   - `WikiScope::Meta`    -> `docs/.nexus-vault/` (risolto via `vault_root_for_scope`)
//   - `WikiScope::Project` -> `<repository_root_path>/.nexus-vault/` per ogni
//                              riga di `projects` con `repository_root_path`
//                              non NULL e directory `.nexus-vault` esistente.
//
// Politica di conflict: idempotente. `ON CONFLICT (scope, COALESCE(project_id::text,''), slug)`
// (stessa expression dell'indice UNIQUE `uq_wiki_docs_slug` della mig 0295)
// aggiorna body/title/tags/timestamps e mantiene `id`. La revisione viene
// registrata solo se `record_revision` rileva un body_hash nuovo (dedup
// automatico via CTE in `storage::record_revision`).
//
// Embedding + upsert Qdrant: best-effort. Se il brain e' down il documento
// viene comunque salvato in `wiki_docs` ma senza `qdrant_point_id`; un re-run
// successivo lo completera'.
// ═══════════════════════════════════════════════════════════════════════════

use crate::wiki::model::WikiScope;
use crate::wiki::storage::record_revision;
use crate::wiki::vault::{parse_frontmatter, sha256_hex, slugify, vault_root_for_scope};
use crate::AppState;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

/// Report finale del re-ingest. Serializzabile per restituirlo dall'endpoint
/// admin sincrono o per logarlo a INFO al termine del bootstrap automatico.
#[derive(Debug, Default, Serialize, Clone)]
pub struct ReingestReport {
    pub meta_docs_ingested: usize,
    pub project_docs_ingested_by_project: HashMap<String, usize>,
    pub files_skipped: usize,
    pub errors: Vec<String>,
    pub elapsed_ms: u128,
}

/// Entry-point principale. Esegue il re-ingest secondo i filtri richiesti.
///
/// - `scope_filter = None` -> meta + tutti i progetti registrati.
/// - `scope_filter = Some(Meta)` -> solo vault meta.
/// - `scope_filter = Some(Project)` + `project_id_filter = Some(p)` -> solo
///   quel progetto.
/// - `scope_filter = Some(Project)` + `project_id_filter = None` -> tutti i
///   progetti registrati.
pub async fn reingest_all(
    state: &AppState,
    scope_filter: Option<WikiScope>,
    project_id_filter: Option<Uuid>,
) -> Result<ReingestReport> {
    let started = Instant::now();
    let mut report = ReingestReport::default();

    let do_meta = matches!(scope_filter, None | Some(WikiScope::Meta));
    let do_projects = matches!(scope_filter, None | Some(WikiScope::Project));

    // ── Vault meta ────────────────────────────────────────────────────────
    if do_meta && project_id_filter.is_none() {
        match reingest_scope(state, WikiScope::Meta, None, &mut report).await {
            Ok(n) => {
                report.meta_docs_ingested = n;
            }
            Err(e) => {
                tracing::warn!(error = %e, "wiki.reingest: meta-vault fallito");
                report.errors.push(format!("meta: {e}"));
            }
        }
    }

    // ── Vault per progetto ────────────────────────────────────────────────
    if do_projects {
        // Lista progetti target: o uno solo (se filter passa un id), o tutti
        // quelli con `repository_root_path` non vuoto.
        let project_rows: Vec<(Uuid, Option<String>)> = if let Some(pid) = project_id_filter {
            sqlx::query_as::<_, (Uuid, Option<String>)>(
                "SELECT id, repository_root_path FROM projects WHERE id = $1",
            )
            .bind(pid)
            .fetch_all(&state.db)
            .await
            .context("SELECT projects per reingest filtrato")?
        } else {
            sqlx::query_as::<_, (Uuid, Option<String>)>(
                "SELECT id, repository_root_path FROM projects \
                 WHERE repository_root_path IS NOT NULL AND repository_root_path <> ''",
            )
            .fetch_all(&state.db)
            .await
            .context("SELECT projects per reingest globale")?
        };

        for (pid, root) in project_rows {
            // Skip progetti senza repository_root_path (succede su quelli mock).
            if root.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                continue;
            }
            match reingest_scope(state, WikiScope::Project, Some(pid), &mut report).await {
                Ok(n) => {
                    if n > 0 {
                        report
                            .project_docs_ingested_by_project
                            .insert(pid.to_string(), n);
                    }
                }
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %e, "wiki.reingest: progetto fallito");
                    report.errors.push(format!("project {pid}: {e}"));
                }
            }
        }
    }

    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Esegue il re-ingest di un singolo vault (meta oppure un progetto).
/// Ritorna il numero di file effettivamente ingestiti (UPSERT andato a buon
/// fine), aggiornando i contatori `files_skipped` / `errors` del report.
async fn reingest_scope(
    state: &AppState,
    scope: WikiScope,
    project_id: Option<Uuid>,
    report: &mut ReingestReport,
) -> Result<usize> {
    let vault_root_str = vault_root_for_scope(state, scope, project_id)
        .await
        .with_context(|| format!("vault_root_for_scope({}, {:?})", scope.as_str(), project_id))?;
    let vault_root = PathBuf::from(&vault_root_str);

    if !vault_root.exists() {
        tracing::info!(
            scope = scope.as_str(),
            project_id = ?project_id,
            vault = %vault_root_str,
            "wiki.reingest: vault assente, skip"
        );
        return Ok(0);
    }
    if !vault_root.is_dir() {
        return Err(anyhow::anyhow!(
            "vault root non e' una directory: {vault_root_str}"
        ));
    }

    let files = collect_markdown_files(&vault_root)?;
    tracing::info!(
        scope = scope.as_str(),
        project_id = ?project_id,
        vault = %vault_root_str,
        candidates = files.len(),
        "wiki.reingest: scan completato"
    );

    let mut ingested = 0usize;
    for abs_path in files {
        let rel_path = abs_path
            .strip_prefix(&vault_root)
            .unwrap_or(&abs_path)
            .to_string_lossy()
            .to_string();
        match ingest_one_file(state, scope, project_id, &abs_path, &rel_path).await {
            Ok(true) => ingested += 1,
            Ok(false) => report.files_skipped += 1,
            Err(e) => {
                tracing::warn!(
                    scope = scope.as_str(),
                    file = %rel_path,
                    error = %e,
                    "wiki.reingest: file fallito"
                );
                report.errors.push(format!("{rel_path}: {e}"));
            }
        }
    }

    Ok(ingested)
}

/// Camminata ricorsiva di una directory raccogliendo tutti i file `.md`.
/// BFS deterministica (ordine alfabetico per leggibilita' dei log).
/// Saltati: file nascosti (`.`), directory `node_modules`, `target`, `.git`.
fn collect_markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(root.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "wiki.reingest: read_dir fallita");
                continue;
            }
        };
        let mut children: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') && name != "." && name != ".." {
                    // Permetti `.nexus-vault` root, ma salta `.git`, `.obsidian`, ecc.
                    if path == root {
                        // root stesso: continue normalmente
                    } else if name == ".git" || name == ".obsidian" {
                        continue;
                    }
                }
                if name == "node_modules" || name == "target" {
                    continue;
                }
            }
            children.push(path);
        }
        children.sort();
        for path in children {
            if path.is_dir() {
                queue.push_back(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Reingest di un singolo file (API pubblica usata dal watcher).
///
/// `abs_path` deve essere assoluto. `vault_root` e' la radice del vault dello
/// scope (`docs/.nexus-vault/` per meta, `<repo>/.nexus-vault/` per project)
/// e serve solo a calcolare il `vault_file_path` relativo memorizzato nel DB.
/// Se `abs_path` non e' figlio di `vault_root`, viene comunque accettato e
/// il rel_path coincide con `abs_path` (best-effort).
///
/// Errori del filesystem o di DB risalgono; lo skip silenzioso (slug vuoto,
/// estensione non `.md`) ritorna `Ok(false)`.
pub async fn reingest_path(
    state: &AppState,
    scope: WikiScope,
    project_id: Option<Uuid>,
    abs_path: &Path,
    vault_root: &Path,
) -> Result<bool> {
    // Filtra subito i file non .md: il watcher puo' triggerare su qualunque
    // file della cartella; vogliamo essere robusti.
    if abs_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("md"))
        .unwrap_or(true)
    {
        return Ok(false);
    }
    if !abs_path.is_file() {
        return Ok(false);
    }
    let rel_path = abs_path
        .strip_prefix(vault_root)
        .unwrap_or(abs_path)
        .to_string_lossy()
        .to_string();
    ingest_one_file(state, scope, project_id, abs_path, &rel_path).await
}

/// Ingest di un singolo file. Ritorna `Ok(true)` se la riga e' stata creata o
/// aggiornata in DB, `Ok(false)` se il file e' stato saltato (frontmatter
/// invalido, slug vuoto, ecc.).
async fn ingest_one_file(
    state: &AppState,
    scope: WikiScope,
    project_id: Option<Uuid>,
    abs_path: &Path,
    rel_path: &str,
) -> Result<bool> {
    let raw = tokio::fs::read_to_string(abs_path)
        .await
        .with_context(|| format!("read_to_string({})", abs_path.display()))?;

    // Frontmatter opzionale: per file senza --- usiamo il filename come fallback.
    let (frontmatter, body_md) = match parse_frontmatter(&raw) {
        Some((fm, body)) => (fm, body),
        None => (serde_json::json!({}), raw.clone()),
    };

    // Derivazione campi dal frontmatter con fallback ragionevoli.
    let fm = frontmatter
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);

    let title = fm
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            abs_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Senza titolo")
                .to_string()
        });

    let slug_raw = fm
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // Fallback dal filename, poi slugify del titolo.
            let stem = abs_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if !stem.is_empty() {
                stem.to_string()
            } else {
                slugify(&title)
            }
        });
    let slug = slugify(&slug_raw);
    if slug.is_empty() {
        tracing::debug!(file = rel_path, "wiki.reingest: slug vuoto, skip");
        return Ok(false);
    }

    // Kind: frontmatter `kind` -> `intent` -> directory di primo livello.
    let kind = fm
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            fm.get("intent")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| infer_kind_from_path(rel_path));

    let intent = fm
        .get("intent")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let tags: Vec<String> = fm
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // public_read e' rilevante solo per scope=meta (CHECK SQL lo enforce).
    let public_read = scope == WikiScope::Meta
        && fm
            .get("public_read")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    let body_hash = sha256_hex(&body_md);
    let auto_generated = fm
        .get("auto_generated")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // ── UPSERT ────────────────────────────────────────────────────────────
    //
    // Il vincolo UNIQUE e' `(scope, COALESCE(project_id::text,''), slug)` —
    // indice su espressione. PostgreSQL accetta `ON CONFLICT` su espressione
    // identica al definition dell'indice.
    let row: (Uuid, bool) = sqlx::query_as::<_, (Uuid, bool)>(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags, vault_file_path,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read
        ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            'none', '{}', FALSE,
            $6, NULL,
            1, $11, $12
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title           = EXCLUDED.title,
            body_md         = EXCLUDED.body_md,
            body_hash       = EXCLUDED.body_hash,
            kind            = EXCLUDED.kind,
            intent          = EXCLUDED.intent,
            tags            = EXCLUDED.tags,
            vault_file_path = EXCLUDED.vault_file_path,
            -- Manteniamo il generated_hash come baseline auto solo se il doc
            -- NON e' mai stato modificato a mano (manually_edited=false).
            generated_hash  = CASE
                                WHEN wiki_docs.manually_edited THEN wiki_docs.generated_hash
                                ELSE EXCLUDED.body_hash
                              END,
            updated_at      = NOW()
        RETURNING id, (xmax = 0) AS inserted
        "#,
    )
    .bind(scope.as_str())
    .bind(project_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(&kind)
    .bind(intent.as_deref())
    .bind(&tags)
    .bind(rel_path)
    .bind(auto_generated)
    .bind(public_read)
    .fetch_one(&state.db)
    .await
    .with_context(|| format!("UPSERT wiki_docs slug={slug}"))?;

    let (doc_id, inserted) = row;

    // Registra revisione (source='import'). `record_revision` deduplica
    // automaticamente sul body_hash, quindi un re-run senza modifiche e' no-op.
    if let Err(e) = record_revision(
        &state.db,
        doc_id,
        &title,
        &body_md,
        &tags,
        "import",
        Some("wiki.reingest"),
        Some(if inserted {
            "import iniziale dal vault"
        } else {
            "re-import dal vault"
        }),
    )
    .await
    {
        tracing::debug!(slug = %slug, error = %e, "wiki.reingest: record_revision fallita");
    }

    // ── Embedding + upsert Qdrant (best-effort) ───────────────────────────
    // Tronca il body a 2000 char come fanno meta_docs/apply.rs e knowledge.
    let snippet = if body_md.len() > 2000 {
        &body_md[..2000]
    } else {
        body_md.as_str()
    };
    let combined = format!("{title}\n\n{snippet}");
    match state.orchestrator.neural.embed_text("", &combined).await {
        Ok(vector) => {
            let point_id = doc_id.to_string();
            let payload = serde_json::json!({
                "scope": scope.as_str(),
                "doc_id": point_id,
                "project_id": project_id.map(|p| p.to_string()),
                "title": title,
                "tags": tags,
                "kind": kind,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            });
            if let Err(e) = crate::vector_memory::upsert_wiki_content_point(
                &state.db, &point_id, vector, payload,
            )
            .await
            {
                tracing::debug!(slug = %slug, error = %e, "wiki.reingest: upsert Qdrant fallito");
            } else {
                let _ = sqlx::query("UPDATE wiki_docs SET qdrant_point_id = $1 WHERE id = $2")
                    .bind(&point_id)
                    .bind(doc_id)
                    .execute(&state.db)
                    .await;
            }
        }
        Err(e) => {
            tracing::debug!(slug = %slug, error = %e, "wiki.reingest: embed_text fallito");
        }
    }

    Ok(true)
}

/// Inferisce il kind dal path relativo (prima componente di directory) quando
/// il frontmatter non lo specifica. Riusa la classificazione di
/// `docs_core::vault::build_vault_path`.
fn infer_kind_from_path(rel_path: &str) -> String {
    let first = rel_path
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first.as_str() {
        "adr" | "api" | "schema" | "runbook" | "architecture" | "changelog" | "concepts"
        | "decisions" => {
            // Singolare in DB: "concepts" -> "concept", "decisions" -> "decision".
            match first.as_str() {
                "concepts" => "concept".to_string(),
                "decisions" => "decision".to_string(),
                other => other.to_string(),
            }
        }
        _ => "note".to_string(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_kind_dirs() {
        assert_eq!(infer_kind_from_path("adr/0017.md"), "adr");
        assert_eq!(infer_kind_from_path("api/rest.md"), "api");
        assert_eq!(infer_kind_from_path("concepts/foo.md"), "concept");
        assert_eq!(
            infer_kind_from_path("decisions/2026-01-01-x.md"),
            "decision"
        );
        assert_eq!(infer_kind_from_path("README.md"), "note");
        assert_eq!(infer_kind_from_path("misc/x.md"), "note");
    }

    #[test]
    fn collect_skips_hidden_and_target() {
        let tmp = std::env::temp_dir().join(format!("wiki-reingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("adr")).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        std::fs::create_dir_all(tmp.join("target")).unwrap();
        std::fs::write(tmp.join("adr/a.md"), "# a").unwrap();
        std::fs::write(tmp.join(".git/b.md"), "# b").unwrap();
        std::fs::write(tmp.join("target/c.md"), "# c").unwrap();
        std::fs::write(tmp.join("d.md"), "# d").unwrap();

        let files = collect_markdown_files(&tmp).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(&tmp).unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("a.md")));
        assert!(names.iter().any(|n| n.ends_with("d.md")));
        assert!(!names.iter().any(|n| n.contains(".git")));
        assert!(!names.iter().any(|n| n.contains("target")));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
