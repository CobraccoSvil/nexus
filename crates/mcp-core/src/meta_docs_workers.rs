// ═══════════════════════════════════════════════════════════════════════════
// meta_docs_workers.rs — Worker periodici per il meta-vault Nexus
//
// MetaDocsRefreshWorker (failsafe periodico, default 900s):
//   - Trova commit recenti non ancora in `nexus_meta_doc_changes`
//   - Per ognuno: chiama internamente la pipeline di ingest-commit
//   - Importante per: push diretti su main da GitHub, merge senza hook locale,
//     casi in cui mcp-core era down al momento del commit.
// ═══════════════════════════════════════════════════════════════════════════

use crate::meta_docs::apply::{apply_generated_doc, resolve_vault_root};
use crate::meta_docs::generators::{all_generators, MetaDocContext};
use crate::AppState;
use sqlx::Row;
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 900;
const DEFAULT_INITIAL_DELAY_SECS: u64 = 60;

/// Avvia il worker periodico di refresh meta-docs. Restituisce subito.
pub fn start_meta_docs_refresh_worker(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(DEFAULT_INITIAL_DELAY_SECS)).await;

        loop {
            let interval = read_interval(&state).await;
            if let Err(e) = tick(&state).await {
                tracing::warn!(error = %e, "meta_docs_refresh_worker: tick error");
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
    tracing::info!("meta_docs_refresh_worker: avviato");
}

async fn read_interval(state: &AppState) -> u64 {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'meta_docs.refresh_worker_interval_secs'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse::<u64>().ok())
    .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS)
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let enabled: bool = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'meta_docs.enabled'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(true);

    if !enabled {
        return Ok(());
    }

    let repo_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());

    // Estrai ultimi 20 commit SHA da git
    let out = Command::new("git")
        .args(["log", "-20", "--pretty=%H"])
        .current_dir(&repo_root)
        .output()
        .await?;
    if !out.status.success() {
        return Ok(());
    }
    let recent_shas: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if recent_shas.is_empty() {
        return Ok(());
    }

    // Filtra quelli gia' processati
    let rows = sqlx::query(
        "SELECT commit_sha FROM nexus_meta_doc_changes WHERE commit_sha = ANY($1)",
    )
    .bind(&recent_shas)
    .fetch_all(&state.db)
    .await?;
    let processed: std::collections::HashSet<String> = rows
        .into_iter()
        .filter_map(|r| r.try_get::<String, _>("commit_sha").ok())
        .collect();

    let to_process: Vec<String> = recent_shas
        .into_iter()
        .filter(|s| !processed.contains(s))
        .collect();

    if to_process.is_empty() {
        return Ok(());
    }

    tracing::info!(
        commits = to_process.len(),
        "meta_docs_refresh_worker: processo commit non ancora ingeriti"
    );

    let vault_root = resolve_vault_root(state).await;

    for sha in &to_process {
        // Estrai commit msg, files
        let msg_out = Command::new("git")
            .args(["log", "-1", "--pretty=%s", sha])
            .current_dir(&repo_root)
            .output()
            .await
            .ok();
        let commit_msg = msg_out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        let files_out = Command::new("git")
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", sha])
            .current_dir(&repo_root)
            .output()
            .await
            .ok();
        let files_changed: Vec<String> = files_out
            .as_ref()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // INSERT idempotente
        let _ = sqlx::query(
            r#"
            INSERT INTO nexus_meta_doc_changes (commit_sha, commit_msg, files_changed, significance)
            VALUES ($1, $2, $3, 0.5)
            ON CONFLICT (commit_sha) DO NOTHING
            "#,
        )
        .bind(sha)
        .bind(&commit_msg)
        .bind(&files_changed)
        .execute(&state.db)
        .await;

        // Esegui generators
        let ctx = MetaDocContext {
            db: &state.db,
            repo_root: repo_root.clone(),
            vault_root: vault_root.clone(),
            commit_sha: Some(sha.clone()),
            files_changed: files_changed.clone(),
        };
        for gen in all_generators() {
            if !gen.relevant_for(&files_changed) {
                continue;
            }
            if let Ok(docs) = gen.generate(&ctx).await {
                for doc in &docs {
                    let _ = apply_generated_doc(state, &vault_root, doc).await;
                }
            }
        }
    }

    Ok(())
}
