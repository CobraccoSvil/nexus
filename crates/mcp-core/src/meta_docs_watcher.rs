// ═══════════════════════════════════════════════════════════════════════════
// meta_docs_watcher.rs — File watcher bidirezionale per docs/.nexus-vault/
//
// Direzione PULL (filesystem -> DB):
//   - Modify: re-parsa frontmatter+body; UPDATE DB se sha256 differente.
//     Se la modifica viene da utente (auto_generated DB era TRUE), passa a FALSE.
//   - Create: INSERT nota orfana se l'id frontmatter non esiste in DB.
//   - Delete: soft delete (UPDATE tags = tags || 'archived').
//
// Direzione PUSH (DB -> filesystem): gestita da `meta_docs::apply::apply_generated_doc`
// e dai generator. Il watcher rileva i propri write tramite hash check (loop detection).
//
// Usa il crate `notify` come `knowledge_watcher.rs` e `projects/file_watcher.rs`.
// ═══════════════════════════════════════════════════════════════════════════

use crate::meta_docs::vault;
use anyhow::Context;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use sqlx::{PgPool, Row};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// Avvia il watcher meta-docs vault in background. Restituisce subito.
pub fn start_meta_docs_watcher(db: PgPool, vault_root: String) {
    let watch_path = PathBuf::from(&vault_root);
    if !watch_path.exists() {
        tracing::warn!(
            vault = %vault_root,
            "meta_docs_watcher: directory inesistente, skip avvio"
        );
        return;
    }

    tokio::spawn(async move {
        if let Err(e) = run_meta_docs_watcher(db, watch_path).await {
            tracing::warn!(error = %e, "meta_docs_watcher: terminato con errore");
        }
    });

    tracing::info!(vault = %vault_root, "meta_docs_watcher: avviato");
}

async fn run_meta_docs_watcher(db: PgPool, vault_root: PathBuf) -> anyhow::Result<()> {
    let debounce_ms = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'meta_docs.watcher_debounce_ms'",
    )
    .fetch_optional(&db)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse::<u64>().ok())
    .unwrap_or(DEFAULT_DEBOUNCE_MS);

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .context("creazione watcher notify")?;
    watcher
        .watch(&vault_root, RecursiveMode::Recursive)
        .context("watch vault root")?;

    tracing::info!(
        vault = ?vault_root,
        debounce_ms,
        "meta_docs_watcher: in ascolto"
    );

    // Loop con debounce: accumula eventi per `debounce_ms` poi processa il piu' recente per path.
    use std::collections::HashMap;
    let mut pending: HashMap<PathBuf, (EventKind, std::time::Instant)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(debounce_ms));

    loop {
        tokio::select! {
            ev = rx.recv() => {
                if let Some(event) = ev {
                    for path in event.paths.iter() {
                        if !should_track(path) {
                            continue;
                        }
                        pending.insert(path.clone(), (event.kind, std::time::Instant::now()));
                    }
                }
            }
            _ = tick.tick() => {
                let now = std::time::Instant::now();
                let due: Vec<(PathBuf, EventKind)> = pending
                    .iter()
                    .filter(|(_, (_, ts))| now.duration_since(*ts).as_millis() as u64 >= debounce_ms)
                    .map(|(p, (k, _))| (p.clone(), *k))
                    .collect();
                for (path, kind) in due {
                    pending.remove(&path);
                    let db_clone = db.clone();
                    let vault_root_clone = vault_root.clone();
                    tokio::spawn(async move {
                        if let Err(e) = process_event(&db_clone, &vault_root_clone, &path, kind).await {
                            tracing::debug!(?path, error = %e, "meta_docs_watcher: evento ignorato");
                        }
                    });
                }
            }
        }
    }
}

fn should_track(path: &Path) -> bool {
    // Solo file .md, escludi .obsidian/ e altre dir di config
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
    {
        return false;
    }
    let p = path.to_string_lossy();
    !p.contains("/.obsidian/")
}

async fn process_event(
    db: &PgPool,
    vault_root: &Path,
    path: &Path,
    kind: EventKind,
) -> anyhow::Result<()> {
    let rel = path
        .strip_prefix(vault_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());

    match kind {
        EventKind::Remove(_) => {
            // Soft delete: aggiunge tag "archived" e mantiene la riga
            sqlx::query(
                r#"
                UPDATE nexus_meta_docs
                SET tags = array_append(tags, 'archived'),
                    updated_at = NOW()
                WHERE vault_file_path = $1
                  AND NOT ('archived' = ANY(tags))
                "#,
            )
            .bind(&rel)
            .execute(db)
            .await?;
            tracing::info!(path = %rel, "meta_docs_watcher: file removed -> archived");
        }
        EventKind::Create(_) | EventKind::Modify(_) => {
            // Leggi file da disco
            let content = match tokio::fs::read_to_string(path).await {
                Ok(c) => c,
                Err(_) => return Ok(()), // file gia' rimosso o non leggibile
            };
            let file_hash = vault::sha256_hex(&content);

            // Controlla se gia' presente in DB
            let existing = sqlx::query(
                "SELECT id, vault_file_hash FROM nexus_meta_docs WHERE vault_file_path = $1",
            )
            .bind(&rel)
            .fetch_optional(db)
            .await?;

            if let Some(row) = existing {
                let db_hash: String = row.try_get("vault_file_hash").unwrap_or_default();
                if db_hash == file_hash {
                    // Loop detection: il file e' quello che abbiamo appena scritto noi
                    return Ok(());
                }
                // Differente: l'utente ha modificato. Aggiorna DB e segna auto_generated=false
                let id: Uuid = row.try_get("id")?;
                let (fm, body) = vault::parse_frontmatter(&content)
                    .unwrap_or_else(|| (serde_json::json!({}), content.clone()));
                let title = fm
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(senza titolo)")
                    .to_string();
                let tags: Vec<String> = fm
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                sqlx::query(
                    r#"
                    UPDATE nexus_meta_docs SET
                        title = $1,
                        body_md = $2,
                        vault_file_hash = $3,
                        tags = $4,
                        auto_generated = FALSE,
                        updated_at = NOW()
                    WHERE id = $5
                    "#,
                )
                .bind(&title)
                .bind(&body)
                .bind(&file_hash)
                .bind(&tags)
                .bind(id)
                .execute(db)
                .await?;
                tracing::info!(path = %rel, id = %id, "meta_docs_watcher: file aggiornato manualmente");
            } else {
                // Nuovo file orfano (creato manualmente dall'utente in Obsidian)
                let (fm, body) = vault::parse_frontmatter(&content)
                    .unwrap_or_else(|| (serde_json::json!({}), content.clone()));
                let id = fm
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .unwrap_or_else(Uuid::new_v4);
                let kind_str = fm.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
                let title = fm
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(senza titolo)")
                    .to_string();
                let slug = fm
                    .get("slug")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| vault::slugify(&title));
                let tags: Vec<String> = fm
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                sqlx::query(
                    r#"
                    INSERT INTO nexus_meta_docs
                        (id, kind, title, slug, body_md, vault_file_path, vault_file_hash,
                         tags, auto_generated)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, FALSE)
                    ON CONFLICT (vault_file_path) DO NOTHING
                    "#,
                )
                .bind(id)
                .bind(kind_str)
                .bind(&title)
                .bind(&slug)
                .bind(&body)
                .bind(&rel)
                .bind(&file_hash)
                .bind(&tags)
                .execute(db)
                .await?;
                tracing::info!(path = %rel, id = %id, "meta_docs_watcher: nuovo file user-created");
            }
        }
        _ => {}
    }

    Ok(())
}
