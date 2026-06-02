// ======================================================================
// knowledge_workers.rs — Worker periodici per Knowledge Base
// ======================================================================
//
// Due worker background implementati come task `tokio::spawn`:
//
// 1. **KnowledgeLinkInferenceWorker** (periodico, default 600s):
//    Cerca note simili via embedding Qdrant e crea link automatici
//    nella tabella `project_knowledge_links`.
//
// 2. **KnowledgeCleanupWorker** (periodico, giornaliero 86400s):
//    Archivia note draft vecchie oltre la soglia configurabile.
//
// I worker girano in mcp-core (non in nexus-orchestrator) perche'
// necessitano di accesso diretto al PgPool, non disponibile nel
// LearningContext del framework LearningWorker.

use std::time::Duration;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::orchestrator::NeuralCoreClient;
use crate::settings;

// ======================================================================
// Costanti di default (sovrascritte da settings DB)
// ======================================================================

const DEFAULT_LINK_INTERVAL_SECS: u64 = 600;
const DEFAULT_AUTOLINK_THRESHOLD: f64 = 0.65;
const DEFAULT_CLEANUP_DRAFT_DAYS: i64 = 30;
const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 86400;

// ======================================================================
// KnowledgeLinkInferenceWorker
// ======================================================================

/// Avvia il worker di inferenza link in background. Restituisce subito.
///
/// Per ogni nota recente con embedding Qdrant, cerca note simili e crea
/// link automatici con `rel_type='relates'`. Il threshold e l'intervallo
/// sono configurabili via settings DB:
///   - `knowledge.autolink_threshold` (default 0.65)
///   - `knowledge.link_worker_interval_secs` (default 600)
pub fn start_knowledge_link_worker(
    db: PgPool,
    neural: NeuralCoreClient,
    project_channels: nexus_events::ProjectChannels,
) {
    tokio::spawn(async move {
        // Delay iniziale per non sovraccaricare il boot.
        tokio::time::sleep(Duration::from_secs(30)).await;

        loop {
            let interval = read_interval_setting(
                &db,
                "knowledge.link_worker_interval_secs",
                DEFAULT_LINK_INTERVAL_SECS,
            )
            .await;

            if let Err(e) = link_inference_tick(&db, &neural, &project_channels).await {
                tracing::warn!("knowledge_link_worker: tick fallito: {e}");
            }

            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });

    tracing::info!("knowledge_link_worker: avviato");
}

/// Un singolo tick del worker di inferenza link.
async fn link_inference_tick(
    db: &PgPool,
    neural: &NeuralCoreClient,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    let threshold = read_f64_setting(
        db,
        "knowledge.autolink_threshold",
        DEFAULT_AUTOLINK_THRESHOLD,
    )
    .await;

    // Seleziona note recenti (draft/active aggiornate nell'ultima ora) con embedding.
    let notes = sqlx::query_as::<_, NoteForLinking>(
        r#"
        SELECT id, project_id, title, body_md, qdrant_point_id
        FROM project_knowledge_notes
        WHERE status IN ('draft', 'active')
          AND updated_at > NOW() - INTERVAL '1 hour'
          AND qdrant_point_id IS NOT NULL
        ORDER BY updated_at DESC
        LIMIT 100
        "#,
    )
    .fetch_all(db)
    .await
    .context("lettura note per link inference fallita")?;

    if notes.is_empty() {
        tracing::debug!("knowledge_link_worker: nessuna nota recente da processare");
        return Ok(());
    }

    let mut links_created: usize = 0;

    for note in &notes {
        // Genera embedding dal body (troncato a 2000 caratteri).
        let embed_text = if note.body_md.len() > 2000 {
            &note.body_md[..2000]
        } else {
            &note.body_md
        };

        let vector = match neural.embed_text("", embed_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(
                    note_id = %note.id,
                    "knowledge_link_worker: embedding fallito, skip: {e}"
                );
                continue;
            }
        };

        // Cerca note simili in Qdrant (top 10, esclusa se stessa).
        let hits = match crate::vector_memory::search_knowledge_points(
            db,
            vector,
            note.project_id,
            10,
        )
        .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(
                    note_id = %note.id,
                    "knowledge_link_worker: ricerca fallita, skip: {e}"
                );
                continue;
            }
        };

        for hit in &hits {
            if hit.score < threshold {
                continue;
            }

            // Estrai note_id dal payload Qdrant.
            let target_note_id = match hit
                .payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                Some(id) if id != note.id => id,
                _ => continue, // stessa nota o payload malformato
            };

            let link_id = Uuid::new_v4();
            let confidence = hit.score as f32;

            // INSERT con ON CONFLICT per idempotenza (triplet: from, to, rel_type).
            let result = sqlx::query(
                r#"
                INSERT INTO project_knowledge_links (
                    id, from_note_id, to_note_id,
                    rel_type, created_by, confidence, created_at
                )
                VALUES ($1, $2, $3, 'relates', 'auto', $4, NOW())
                ON CONFLICT (from_note_id, to_note_id, rel_type)
                DO UPDATE SET confidence = GREATEST(
                    project_knowledge_links.confidence,
                    EXCLUDED.confidence
                )
                "#,
            )
            .bind(link_id)
            .bind(note.id)
            .bind(target_note_id)
            .bind(confidence)
            .execute(db)
            .await;

            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    links_created += 1;

                    // Emit SSE per il link creato.
                    let _ = nexus_events::dispatcher::emit(
                        project_channels,
                        note.project_id,
                        nexus_events::ProjectEvent::KnowledgeLinkCreated {
                            link_id,
                            from: note.id,
                            to: target_note_id,
                            rel_type: "relates".to_string(),
                            created_by: "auto".to_string(),
                        },
                    );
                }
                Ok(_) => {} // nessuna riga toccata (link gia' esistente con confidence >= nuova)
                Err(e) => {
                    tracing::debug!(
                        from = %note.id,
                        to = %target_note_id,
                        "knowledge_link_worker: insert link fallito: {e}"
                    );
                }
            }
        }
    }

    if links_created > 0 {
        tracing::info!(
            "knowledge_link_worker: {links_created} link creati/aggiornati su {} note",
            notes.len()
        );
    }

    Ok(())
}

/// Riga proiettata da `project_knowledge_notes` per il worker di linking.
#[derive(sqlx::FromRow)]
struct NoteForLinking {
    id: Uuid,
    project_id: Uuid,
    #[allow(dead_code)]
    title: String,
    body_md: String,
    #[allow(dead_code)]
    qdrant_point_id: Option<String>,
}

/// Esegue il calcolo link su TUTTE le note di un progetto specifico (senza filtro temporale).
/// Usato dall'endpoint `POST /api/projects/:id/knowledge/recompute-links`.
///
/// Restituisce `(notes_processed, links_created)`.
pub async fn recompute_links_for_project(
    db: &PgPool,
    neural: &NeuralCoreClient,
    project_channels: &nexus_events::ProjectChannels,
    project_id: Uuid,
) -> anyhow::Result<(usize, usize)> {
    let threshold = read_f64_setting(
        db,
        "knowledge.autolink_threshold",
        DEFAULT_AUTOLINK_THRESHOLD,
    )
    .await;

    let notes = sqlx::query_as::<_, NoteForLinking>(
        r#"
        SELECT id, project_id, title, body_md, qdrant_point_id
        FROM project_knowledge_notes
        WHERE project_id = $1
          AND status IN ('draft', 'active')
          AND qdrant_point_id IS NOT NULL
        ORDER BY updated_at DESC
        LIMIT 500
        "#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .context("recompute: lettura note fallita")?;

    let mut links_created: usize = 0;

    for note in &notes {
        // Preferisci il vettore GIA' stoccato in Qdrant (zero costo brain).
        // Fallback a ri-embedding via brain solo se il point Qdrant manca.
        let vector = if let Some(point_id) = note.qdrant_point_id.as_ref() {
            match crate::vector_memory::get_knowledge_point_vector(db, point_id).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(
                        note_id = %note.id,
                        point_id = %point_id,
                        "recompute_links: get_point fallito, fallback embed: {e}"
                    );
                    let embed_text = if note.body_md.len() > 2000 {
                        &note.body_md[..2000]
                    } else {
                        &note.body_md
                    };
                    match neural.embed_text("", embed_text).await {
                        Ok(v) => v,
                        Err(_) => continue,
                    }
                }
            }
        } else {
            let embed_text = if note.body_md.len() > 2000 {
                &note.body_md[..2000]
            } else {
                &note.body_md
            };
            match neural.embed_text("", embed_text).await {
                Ok(v) => v,
                Err(_) => continue,
            }
        };

        let hits = match crate::vector_memory::search_knowledge_points(
            db,
            vector,
            note.project_id,
            10,
        )
        .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(note_id = %note.id, error = %e, "recompute: search failed");
                continue;
            }
        };
        tracing::info!(
            note_id = %note.id,
            hits_count = hits.len(),
            top_score = hits.first().map(|h| h.score).unwrap_or(0.0),
            threshold,
            "recompute: hits ricevuti"
        );

        for hit in &hits {
            if hit.score < threshold {
                continue;
            }
            let target_note_id = match hit
                .payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                Some(id) if id != note.id => id,
                _ => {
                    tracing::debug!(
                        from_note = %note.id,
                        score = hit.score,
                        payload_note_id = ?hit.payload.get("note_id"),
                        "recompute: hit skipped (same note or bad payload)"
                    );
                    continue;
                }
            };

            let link_id = Uuid::new_v4();
            let confidence = hit.score as f32;
            let result = sqlx::query(
                r#"
                INSERT INTO project_knowledge_links (
                    id, from_note_id, to_note_id,
                    rel_type, created_by, confidence, created_at
                )
                VALUES ($1, $2, $3, 'relates', 'auto', $4, NOW())
                ON CONFLICT (from_note_id, to_note_id, rel_type)
                DO UPDATE SET confidence = GREATEST(
                    project_knowledge_links.confidence,
                    EXCLUDED.confidence
                )
                "#,
            )
            .bind(link_id)
            .bind(note.id)
            .bind(target_note_id)
            .bind(confidence)
            .execute(db)
            .await;

            match result {
                Ok(r) => {
                    if r.rows_affected() > 0 {
                        links_created += 1;
                        let _ = nexus_events::dispatcher::emit(
                            project_channels,
                            note.project_id,
                            nexus_events::ProjectEvent::KnowledgeLinkCreated {
                                link_id,
                                from: note.id,
                                to: target_note_id,
                                rel_type: "relates".to_string(),
                                created_by: "auto".to_string(),
                            },
                        );
                    } else {
                        tracing::warn!(
                            from = %note.id,
                            to = %target_note_id,
                            score = hit.score,
                            "recompute: INSERT rows_affected=0 (conflict no-op)"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        from = %note.id,
                        to = %target_note_id,
                        error = %e,
                        "recompute: INSERT link fallito"
                    );
                }
            }
        }
    }

    tracing::info!(
        project_id = %project_id,
        notes = notes.len(),
        links_created,
        "recompute_links_for_project: completato"
    );

    Ok((notes.len(), links_created))
}

// ======================================================================
// KnowledgeCleanupWorker
// ======================================================================

/// Avvia il worker di cleanup in background. Restituisce subito.
///
/// Archivia note in stato `draft` piu' vecchie di N giorni (configurabile
/// via `knowledge.cleanup_draft_days`, default 30).
pub fn start_knowledge_cleanup_worker(db: PgPool) {
    tokio::spawn(async move {
        // Delay iniziale: non serve fretta per il cleanup.
        tokio::time::sleep(Duration::from_secs(120)).await;

        loop {
            let interval = read_interval_setting(
                &db,
                "knowledge.cleanup_worker_interval_secs",
                DEFAULT_CLEANUP_INTERVAL_SECS,
            )
            .await;

            if let Err(e) = cleanup_tick(&db).await {
                tracing::warn!("knowledge_cleanup_worker: tick fallito: {e}");
            }

            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });

    tracing::info!("knowledge_cleanup_worker: avviato");
}

/// Un singolo tick del worker di cleanup.
async fn cleanup_tick(db: &PgPool) -> anyhow::Result<()> {
    let draft_days = read_i64_setting(
        db,
        "knowledge.cleanup_draft_days",
        DEFAULT_CLEANUP_DRAFT_DAYS,
    )
    .await;

    // Archivia note draft scadute (M14.5, sempre attivo).
    let result = sqlx::query(
        r#"
        UPDATE project_knowledge_notes
        SET status = 'archived', archived_at = NOW(), updated_at = NOW()
        WHERE status = 'draft'
          AND created_at < NOW() - ($1 || ' days')::INTERVAL
        "#,
    )
    .bind(draft_days.to_string())
    .execute(db)
    .await
    .context("cleanup draft knowledge notes fallito")?;

    let archived = result.rows_affected();
    if archived > 0 {
        tracing::info!(
            "knowledge_cleanup_worker: {archived} note draft archiviate (soglia: {draft_days} giorni)"
        );
    } else {
        tracing::debug!("knowledge_cleanup_worker: nessuna nota draft da archiviare");
    }

    // M14.5 — Archivia note active inattive. Gated da
    // knowledge.cleanup_inactive_enabled (OFF di default) per non archiviare
    // note attive a sorpresa su installazioni esistenti. Soglia in giorni
    // dall'ultimo updated_at (regola G: niente costanti hardcoded).
    let inactive_enabled = read_bool_setting(db, "knowledge.cleanup_inactive_enabled", false).await;
    if inactive_enabled {
        let inactive_days = read_i64_setting(db, "knowledge.cleanup_inactive_days", 90).await;
        let res2 = sqlx::query(
            r#"
            UPDATE project_knowledge_notes
            SET status = 'archived', archived_at = NOW(), updated_at = NOW()
            WHERE status = 'active'
              AND updated_at < NOW() - ($1 || ' days')::INTERVAL
            "#,
        )
        .bind(inactive_days.to_string())
        .execute(db)
        .await
        .context("cleanup active knowledge notes fallito")?;
        let n = res2.rows_affected();
        if n > 0 {
            tracing::info!(
                "knowledge_cleanup_worker: {n} note active inattive archiviate (soglia: {inactive_days} giorni)"
            );
        }
    }

    Ok(())
}

// ======================================================================
// Helper per lettura settings dal DB con fallback
// ======================================================================

async fn read_interval_setting(db: &PgPool, key: &str, default: u64) -> u64 {
    settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

async fn read_f64_setting(db: &PgPool, key: &str, default: f64) -> f64 {
    settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

async fn read_i64_setting(db: &PgPool, key: &str, default: i64) -> i64 {
    settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

async fn read_bool_setting(db: &PgPool, key: &str, default: bool) -> bool {
    settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(default)
}
