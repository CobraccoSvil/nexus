//! PR-3 (Codex pattern) — Cache + watcher per `.nexus/project-instructions.md`
//! dei progetti utente.
//!
//! Sorgenti dati:
//!   * File system: `<project_root>/.nexus/project-instructions.md`
//!   * Cache DB: tabella `nexus_project_instructions` (vedi mig 0157)
//!
//! Il `router_node` Python legge la cache DB via psycopg2; questo modulo Rust
//! garantisce che la cache sia in sync con il FS:
//!   * `refresh_for_project(pool, project_id, root)` — legge il file, calcola
//!     hash SHA-256, aggiorna la cache se cambiato
//!   * Chiamato dall'endpoint admin `POST /admin/projects/:id/project-instructions/refresh`
//!   * Chiamato anche dal listener `notify` su modifiche del file (best-effort,
//!     non-bloccante; se notify fallisce su WSL/Docker, il refresh manuale OK)
//!
//! Sicurezza: il path e' sempre derivato da `workspaces.absolute_path`; nessun
//! traversal. Troncamento a `orchestrator.project_instructions_max_chars` (def 8000).

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::PathBuf;
use uuid::Uuid;

const DEFAULT_FILE: &str = ".nexus/project-instructions.md";
const DEFAULT_MAX_CHARS: usize = 8_000;

#[derive(Debug, Clone)]
pub struct ProjectInstructions {
    pub content: String,
    pub content_hash: String,
    pub file_path: String,
    pub source: &'static str, // "fs" | "cache" | "none"
}

/// Legge il setting `orchestrator.project_instructions_file` con fallback.
async fn lookup_setting(pool: &PgPool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = $1 LIMIT 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| default.to_string())
}

/// Refresh idempotente: legge il file dal FS e aggiorna la cache DB se cambia
/// l'hash. Ritorna le istruzioni correnti o None se il file non esiste.
pub async fn refresh_for_project(
    pool: &PgPool,
    project_id: Uuid,
    project_root: &str,
) -> Option<ProjectInstructions> {
    let file_rel = lookup_setting(pool, "orchestrator.project_instructions_file", DEFAULT_FILE).await;
    let max_chars: usize = lookup_setting(pool, "orchestrator.project_instructions_max_chars", "8000")
        .await
        .parse::<usize>()
        .unwrap_or(DEFAULT_MAX_CHARS);

    let path = PathBuf::from(project_root).join(&file_rel);
    if !path.is_file() {
        // File non presente: invalida cache (best-effort).
        let _ = sqlx::query("DELETE FROM nexus_project_instructions WHERE project_id = $1")
            .bind(project_id)
            .execute(pool)
            .await;
        return None;
    }
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("project_instructions: read fallita su {:?}: {}", path, e);
            return None;
        }
    };
    let truncated = if content.len() > max_chars {
        let mut t = content[..max_chars.saturating_sub(30)].to_string();
        t.push_str("\n[... truncated by orchestrator]");
        t
    } else {
        content.clone()
    };
    let mut hasher = Sha256::new();
    hasher.update(truncated.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let short_hash = &hash[..16.min(hash.len())];

    if let Err(e) = sqlx::query(
        r#"INSERT INTO nexus_project_instructions
              (project_id, file_path, content_cache, content_hash, updated_at)
           VALUES ($1, $2, $3, $4, NOW())
           ON CONFLICT (project_id) DO UPDATE
              SET file_path = EXCLUDED.file_path,
                  content_cache = EXCLUDED.content_cache,
                  content_hash = EXCLUDED.content_hash,
                  updated_at = NOW()
           WHERE nexus_project_instructions.content_hash IS DISTINCT FROM EXCLUDED.content_hash"#,
    )
    .bind(project_id)
    .bind(&file_rel)
    .bind(&truncated)
    .bind(short_hash)
    .execute(pool)
    .await
    {
        tracing::warn!("project_instructions: upsert fallito per {}: {}", project_id, e);
    }

    Some(ProjectInstructions {
        content: truncated,
        content_hash: short_hash.to_string(),
        file_path: file_rel,
        source: "fs",
    })
}
