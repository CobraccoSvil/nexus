// ======================================================================
// knowledge_watcher.rs — Watcher bidirezionale filesystem <-> DB
// ======================================================================
//
// Monitora la directory `.nexus/knowledge/notes/` di un progetto per
// rilevare modifiche esterne ai file Markdown della Knowledge Base.
//
// Operazioni:
//   - Modify: ricalcola SHA-256; se diverso dal DB aggiorna titolo/body/tags.
//   - Create: se l'id nel frontmatter non esiste in DB, inserisce nota orfana.
//   - Delete: segna la nota come archived (mai hard delete).
//
// Usa il crate `notify` con pattern canale tokio (come file_watcher.rs).
// Debounce configurabile via `knowledge.vault_watcher_debounce_ms` (default 500).
// Loop detection: se l'hash del file corrisponde all'ultimo push dal DB, ignora.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::knowledge::{sha256_hex, extract_tags, title_from_content};
use crate::knowledge::vault::{parse_frontmatter, extract_wikilinks};
use crate::orchestrator::NeuralCoreClient;
use crate::settings;

/// Debounce predefinito in millisecondi.
const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// Avvia il watcher vault in background. Restituisce subito.
///
/// Monitora `<repo_root>/.nexus/knowledge/notes/` per modifiche,
/// creazioni e cancellazioni di file Markdown. Gli eventi vengono
/// riconciliati con il DB e, se necessario, viene emesso un SSE.
pub fn start_vault_watcher(
    db: PgPool,
    neural: NeuralCoreClient,
    project_id: Uuid,
    repo_root: String,
    project_channels: nexus_events::ProjectChannels,
) {
    let watch_dir = format!("{repo_root}/.nexus/knowledge/notes");
    let watch_path = PathBuf::from(&watch_dir);

    if !watch_path.exists() {
        tracing::debug!(
            "knowledge_watcher: directory non esiste ancora, skip: {watch_dir}"
        );
        return;
    }

    tokio::spawn(async move {
        let result = run_vault_watcher(
            db,
            neural,
            project_id,
            watch_path,
            project_channels,
        )
        .await;

        if let Err(e) = result {
            tracing::warn!(
                project_id = %project_id,
                "knowledge_watcher: watcher terminato con errore: {e}"
            );
        }
    });

    tracing::info!(
        "knowledge_watcher: avviato per project={project_id} dir={watch_dir}"
    );
}

/// Ciclo principale del watcher. Ritorna solo in caso di errore grave o shutdown.
async fn run_vault_watcher(
    db: PgPool,
    neural: NeuralCoreClient,
    project_id: Uuid,
    watch_dir: PathBuf,
    project_channels: nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    // Canale tokio per ricevere gli eventi `notify` dal thread OS.
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(512);

    let tx_sync = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx_sync.blocking_send(res);
    })?;

    watcher.watch(&watch_dir, RecursiveMode::Recursive)?;
    tracing::info!(
        "knowledge_watcher: watching attivo su {} (project={project_id})",
        watch_dir.display()
    );

    // Struttura debounce: path -> ultimo evento + timestamp.
    let mut pending: HashMap<PathBuf, PendingEvent> = HashMap::new();
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let debounce_ms = read_debounce_ms(&db).await;
        let debounce_dur = Duration::from_millis(debounce_ms);

        let timeout = deadline.map(|d| {
            let now = tokio::time::Instant::now();
            if d > now { d - now } else { Duration::ZERO }
        });

        let recv_fut = rx.recv();

        if let Some(timeout_dur) = timeout {
            tokio::select! {
                maybe_event = recv_fut => {
                    match maybe_event {
                        None => break,
                        Some(Ok(event)) => {
                            enqueue_event(&mut pending, &event, &watch_dir);
                            deadline = Some(tokio::time::Instant::now() + debounce_dur);
                        }
                        Some(Err(e)) => {
                            tracing::debug!("knowledge_watcher: notify error: {e}");
                        }
                    }
                }
                _ = tokio::time::sleep(timeout_dur) => {
                    flush_pending(
                        &db,
                        &neural,
                        project_id,
                        &mut pending,
                        &project_channels,
                    )
                    .await;
                    deadline = None;
                }
            }
        } else {
            match recv_fut.await {
                None => break,
                Some(Ok(event)) => {
                    enqueue_event(&mut pending, &event, &watch_dir);
                    deadline = Some(tokio::time::Instant::now() + debounce_dur);
                }
                Some(Err(e)) => {
                    tracing::debug!("knowledge_watcher: notify error: {e}");
                }
            }
        }
    }

    Ok(())
}

// ======================================================================
// Tipo di evento pendente
// ======================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaultEventKind {
    CreateOrModify,
    Remove,
}

struct PendingEvent {
    kind: VaultEventKind,
}

// ======================================================================
// Enqueue / Flush
// ======================================================================

/// Filtra e accoda gli eventi notify rilevanti.
fn enqueue_event(
    pending: &mut HashMap<PathBuf, PendingEvent>,
    event: &Event,
    watch_dir: &Path,
) {
    let kind = match &event.kind {
        EventKind::Create(_) => VaultEventKind::CreateOrModify,
        EventKind::Modify(notify::event::ModifyKind::Data(_))
        | EventKind::Modify(notify::event::ModifyKind::Any) => VaultEventKind::CreateOrModify,
        EventKind::Remove(_) => VaultEventKind::Remove,
        _ => return,
    };

    for path in &event.paths {
        // Solo file .md nella directory monitorata.
        if !path.starts_with(watch_dir) {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "md" {
            continue;
        }
        pending.insert(path.clone(), PendingEvent { kind });
    }
}

/// Processa tutti gli eventi pendenti dopo il debounce.
async fn flush_pending(
    db: &PgPool,
    neural: &NeuralCoreClient,
    project_id: Uuid,
    pending: &mut HashMap<PathBuf, PendingEvent>,
    project_channels: &nexus_events::ProjectChannels,
) {
    let events: Vec<(PathBuf, PendingEvent)> = pending.drain().collect();

    for (path, evt) in events {
        match evt.kind {
            VaultEventKind::CreateOrModify => {
                if let Err(e) = handle_create_or_modify(
                    db, neural, project_id, &path, project_channels,
                )
                .await
                {
                    tracing::debug!(
                        path = %path.display(),
                        "knowledge_watcher: errore su create/modify: {e}"
                    );
                }
            }
            VaultEventKind::Remove => {
                if let Err(e) = handle_remove(db, project_id, &path, project_channels).await {
                    tracing::debug!(
                        path = %path.display(),
                        "knowledge_watcher: errore su remove: {e}"
                    );
                }
            }
        }
    }
}

// ======================================================================
// Handler per evento Create / Modify
// ======================================================================

async fn handle_create_or_modify(
    db: &PgPool,
    neural: &NeuralCoreClient,
    project_id: Uuid,
    path: &Path,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(path)
        .await
        .context("lettura file vault fallita")?;

    let file_hash = sha256_hex(&content);

    // Parse frontmatter per estrarre id e metadati.
    let (frontmatter, body) = match parse_frontmatter(&content) {
        Some(pair) => pair,
        None => {
            tracing::debug!(
                path = %path.display(),
                "knowledge_watcher: nessun frontmatter, skip"
            );
            return Ok(());
        }
    };

    let note_id_str = frontmatter
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let note_id = match note_id_str.parse::<Uuid>() {
        Ok(id) => id,
        Err(_) => {
            tracing::debug!(
                path = %path.display(),
                "knowledge_watcher: id frontmatter non valido, skip"
            );
            return Ok(());
        }
    };

    // Loop detection: confronta hash con vault_file_hash in DB.
    let db_hash: Option<String> = sqlx::query_scalar(
        "SELECT vault_file_hash FROM project_knowledge_notes WHERE id = $1 AND project_id = $2",
    )
    .bind(note_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .context("lettura hash vault DB fallita")?;

    if db_hash.as_deref() == Some(&file_hash) {
        // Hash identico: modifica causata dal nostro stesso push -> ignora.
        return Ok(());
    }

    // Estrai titolo, body, tags dal contenuto.
    let title = frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| title_from_content(&body, 80));

    let tags = extract_tags(&body);
    let _wikilinks = extract_wikilinks(&body);

    // Controlla se la nota esiste nel DB.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_knowledge_notes WHERE id = $1 AND project_id = $2)",
    )
    .bind(note_id)
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(false);

    if exists {
        // UPDATE nota esistente.
        sqlx::query(
            r#"
            UPDATE project_knowledge_notes
            SET title = $1,
                body_md = $2,
                tags = $3,
                vault_file_hash = $4,
                updated_at = NOW()
            WHERE id = $5 AND project_id = $6
            "#,
        )
        .bind(&title)
        .bind(&body)
        .bind(&tags)
        .bind(&file_hash)
        .bind(note_id)
        .bind(project_id)
        .execute(db)
        .await
        .context("update nota da vault fallito")?;

        // Re-embedding se il body e' cambiato.
        let embed_text = if body.len() > 2000 { &body[..2000] } else { &body };
        if let Ok(vector) = neural.embed_text("", embed_text).await {
            // Recupera il point_id Qdrant per l'upsert.
            let point_id: Option<String> = sqlx::query_scalar(
                "SELECT qdrant_point_id FROM project_knowledge_notes WHERE id = $1",
            )
            .bind(note_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

            if let Some(pid) = point_id {
                let payload = serde_json::json!({
                    "project_id": project_id.to_string(),
                    "note_id": note_id.to_string(),
                    "status": "active",
                });
                let _ = crate::vector_memory::upsert_knowledge_point(
                    db, &pid, vector, payload,
                )
                .await;
            }
        }

        // Emit SSE.
        let status = frontmatter
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            .to_string();

        let _ = nexus_events::dispatcher::emit(
            project_channels,
            project_id,
            nexus_events::ProjectEvent::KnowledgeNoteUpdated {
                note_id,
                status,
            },
        );

        tracing::debug!(
            note_id = %note_id,
            "knowledge_watcher: nota aggiornata da filesystem"
        );
    } else {
        // CREATE nota orfana (file creato esternamente con id nel frontmatter).
        let status = frontmatter
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("draft");

        let intent = frontmatter
            .get("intent")
            .and_then(|v| v.as_str());

        // Genera embedding.
        let embed_text = if body.len() > 2000 { &body[..2000] } else { &body };
        let point_id = Uuid::new_v4().to_string();
        let mut qdrant_point: Option<String> = None;

        if let Ok(vector) = neural.embed_text("", embed_text).await {
            let payload = serde_json::json!({
                "project_id": project_id.to_string(),
                "note_id": note_id.to_string(),
                "status": status,
            });
            if crate::vector_memory::upsert_knowledge_point(
                db, &point_id, vector, payload,
            )
            .await
            .is_ok()
            {
                qdrant_point = Some(point_id);
            }
        }

        // Costruisci vault_file_path relativo.
        let vault_file_path = path
            .to_str()
            .and_then(|s| {
                s.find(".nexus/knowledge/notes/")
                    .map(|idx| s[idx..].to_string())
            });

        sqlx::query(
            r#"
            INSERT INTO project_knowledge_notes (
                id, project_id, intent, title, body_md,
                status, qdrant_point_id, tags, vault_file_path,
                vault_file_hash, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(note_id)
        .bind(project_id)
        .bind(intent)
        .bind(&title)
        .bind(&body)
        .bind(status)
        .bind(&qdrant_point)
        .bind(&tags)
        .bind(&vault_file_path)
        .bind(&file_hash)
        .execute(db)
        .await
        .context("insert nota orfana da vault fallito")?;

        // Emit SSE per nota creata.
        let _ = nexus_events::dispatcher::emit(
            project_channels,
            project_id,
            nexus_events::ProjectEvent::KnowledgeNoteCreated {
                note_id,
                title: title.clone(),
                intent: intent.map(|s| s.to_string()),
            },
        );

        tracing::info!(
            note_id = %note_id,
            "knowledge_watcher: nota orfana creata da filesystem"
        );
    }

    Ok(())
}

// ======================================================================
// Handler per evento Remove
// ======================================================================

async fn handle_remove(
    db: &PgPool,
    project_id: Uuid,
    path: &Path,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    // Cerchiamo la nota dal vault_file_path (relativo).
    let rel_path = path
        .to_str()
        .and_then(|s| {
            s.find(".nexus/knowledge/notes/")
                .map(|idx| s[idx..].to_string())
        });

    let rel = match rel_path {
        Some(r) => r,
        None => return Ok(()),
    };

    // Trova note_id dal path relativo.
    let note_row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT id, status FROM project_knowledge_notes
        WHERE project_id = $1 AND vault_file_path = $2
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(&rel)
    .fetch_optional(db)
    .await
    .context("ricerca nota per path vault fallita")?;

    let (note_id, current_status) = match note_row {
        Some(r) => r,
        None => return Ok(()), // nessuna nota associata
    };

    if current_status == "archived" {
        return Ok(()); // gia' archiviata
    }

    // Soft delete: archivia.
    sqlx::query(
        r#"
        UPDATE project_knowledge_notes
        SET status = 'archived', updated_at = NOW()
        WHERE id = $1 AND project_id = $2
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .execute(db)
    .await
    .context("archiviazione nota per rimozione file fallita")?;

    let _ = nexus_events::dispatcher::emit(
        project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeNoteUpdated {
            note_id,
            status: "archived".to_string(),
        },
    );

    tracing::info!(
        note_id = %note_id,
        "knowledge_watcher: nota archiviata per rimozione file"
    );

    Ok(())
}

// ======================================================================
// Helper
// ======================================================================

async fn read_debounce_ms(db: &PgPool) -> u64 {
    settings::get_setting(db, "knowledge.vault_watcher_debounce_ms")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DEBOUNCE_MS)
}
