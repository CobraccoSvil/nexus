// ═══════════════════════════════════════════════════════════════════════════
// wiki/chat_note_worker.rs — ADR 0017 v2 TODO 6.
//
// Worker periodico che ingesta messaggi user dalla chat come wiki_doc
// (scope='project', kind='chat_note'). Scopo: "learning by chat" — l'agente
// accumula contesto dai turni dell'utente cosi' che il RAG possa richiamare
// promesse, decisioni e brief discussi in conversazione.
//
// Idempotenza: ogni messaggio processato e' marcato con `chat_messages.kb_ingested`
// (mig 0303). Il worker scansiona solo righe con `kb_ingested IS NULL`. Sia
// le ingestioni con successo sia gli scarti (filtro lunghezza, regex banali,
// progetto inesistente) impostano `kb_ingested = TRUE` per evitare retry.
//
// Settings DB-driven (mig 0305 -> agent.wiki.chat_note_*):
//   - agent.wiki.chat_note_worker_enabled       (default true)
//   - agent.wiki.chat_note_worker_interval_secs (default 30)
//   - agent.wiki.chat_note_min_body_chars       (default 100)
//   - agent.wiki.chat_note_skip_patterns        (default regex banali)
//   - agent.wiki.chat_note_max_per_minute       (default 50)
//
// Niente fallback hardcoded sui modelli (regola G): il worker usa solo
// l'embedding via `state.orchestrator.neural` che a sua volta legge il purpose
// model dalla routing matrix DB-driven. Se l'embedding fallisce il doc resta
// in DB senza qdrant_point_id (re-embed possibile a posteriori).
// ═══════════════════════════════════════════════════════════════════════════

use crate::deps::WikiDeps;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::json;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern allineato a links_worker)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChatNoteSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub min_body_chars: usize,
    pub skip_pattern_raw: String,
    pub max_per_minute: usize,
}

impl ChatNoteSettings {
    fn safe_defaults() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            min_body_chars: 100,
            skip_pattern_raw: r"^(ok|si|sì|no|grazie|ciao|bene|perfetto|ottimo)[.!?\s]*$"
                .to_string(),
            max_per_minute: 50,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(ChatNoteSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> ChatNoteSettings {
    {
        let guard = SETTINGS_CACHE.read().await;
        if let Some((v, exp)) = guard.as_ref() {
            if Instant::now() < *exp {
                return v.clone();
            }
        }
    }
    let value = match load_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "wiki.chat_note: lettura settings fallita, uso safe_defaults"
            );
            ChatNoteSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value.clone(), Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<ChatNoteSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.chat_note_worker_enabled', \
            'agent.wiki.chat_note_worker_interval_secs', \
            'agent.wiki.chat_note_min_body_chars', \
            'agent.wiki.chat_note_skip_patterns', \
            'agent.wiki.chat_note_max_per_minute' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.chat_note_*")?;

    let mut out = ChatNoteSettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        match key.as_str() {
            "agent.wiki.chat_note_worker_enabled" => {
                out.enabled = matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
            "agent.wiki.chat_note_worker_interval_secs" => {
                if let Ok(v) = raw.trim().parse::<u64>() {
                    out.interval_secs = v.max(5);
                }
            }
            "agent.wiki.chat_note_min_body_chars" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.min_body_chars = v;
                }
            }
            "agent.wiki.chat_note_skip_patterns" => {
                if !raw.trim().is_empty() {
                    out.skip_pattern_raw = raw;
                }
            }
            "agent.wiki.chat_note_max_per_minute" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.max_per_minute = v.max(1);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Entry-point del worker
// ───────────────────────────────────────────────────────────────────────────

/// Avvia il loop in background. Delay iniziale 60s per non sovraccaricare boot.
pub fn start_chat_note_worker(state: Arc<WikiDeps>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            min_body_chars = init.min_body_chars,
            "wiki.chat_note: worker avviato"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }
            match scan_and_ingest(&state, &settings).await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(ingested = n, "wiki.chat_note: batch completato");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.chat_note: batch fallito");
                }
            }
            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

/// Singolo batch: legge i messaggi pending e ne ingesta fino al cap.
async fn scan_and_ingest(state: &WikiDeps, settings: &ChatNoteSettings) -> Result<usize> {
    // Compila la regex skip una volta per batch (case-insensitive). Errori
    // di compilazione -> WARN + skip filtraggio (zero false-negative).
    let skip_re: Option<Regex> = match Regex::new(&format!("(?i){}", settings.skip_pattern_raw)) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!(
                pattern = %settings.skip_pattern_raw,
                error = %e,
                "wiki.chat_note: regex skip non valida, filtro disabilitato"
            );
            None
        }
    };

    // Separazione DB per-progetto: i messaggi chat vivono nel DB del progetto (a
    // flag OFF ancora nel meta). Iteriamo i progetti e instradiamo scansione +
    // mark_processed sul pool di ciascuno; `wiki_docs`/Qdrant restano sul meta
    // (dominio KB non ancora migrato). `max_per_minute` resta un cap GLOBALE per
    // batch tramite il budget `remaining` decrementato a ogni messaggio trattato.
    let mut ingested = 0usize;
    let mut remaining = settings.max_per_minute;
    for project_id in state.list_project_ids().await {
        if remaining == 0 {
            break;
        }
        let run_pool = state.run_pool(project_id).await;
        // Messaggi user pending per QUESTO progetto (no JOIN projects: il
        // project_id viene dall'iterazione, e a flag ON il pool ha solo i suoi).
        let rows = sqlx::query(
            r#"
            SELECT cm.id, cm.session_id, cm.content, cm.created_at
            FROM chat_messages cm
            WHERE cm.project_id = $1
              AND cm.role = 'user'
              AND cm.kb_ingested IS NULL
              AND cm.deleted_at IS NULL
            ORDER BY cm.created_at ASC
            LIMIT $2
            "#,
        )
        .bind(project_id)
        .bind(remaining as i64)
        .fetch_all(&run_pool)
        .await
        .context("SELECT chat_messages pending kb_ingest")?;

        for row in rows {
            if remaining == 0 {
                break;
            }
            let message_id: Uuid = match row.try_get("id") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let session_id: Uuid = match row.try_get("session_id") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let content: String = row.try_get("content").unwrap_or_default();
            let created_at: chrono::DateTime<chrono::Utc> = row
                .try_get("created_at")
                .unwrap_or_else(|_| chrono::Utc::now());

            remaining = remaining.saturating_sub(1);

            // Filtro 1: lunghezza minima.
            if content.trim().chars().count() < settings.min_body_chars {
                mark_processed(&run_pool, message_id).await;
                continue;
            }
            // Filtro 2: pattern banale.
            if let Some(re) = &skip_re {
                if re.is_match(content.trim()) {
                    mark_processed(&run_pool, message_id).await;
                    continue;
                }
            }

            match ingest_message(
                state, message_id, project_id, session_id, &content, created_at,
            )
            .await
            {
                Ok(true) => {
                    ingested += 1;
                    mark_processed(&run_pool, message_id).await;
                }
                Ok(false) => {
                    // Skip esplicito (es. ingest_message ha deciso di non creare doc).
                    mark_processed(&run_pool, message_id).await;
                }
                Err(e) => {
                    // Non marcare: il prossimo giro riprovera'. Se l'errore e' persistente
                    // resta in coda — ammesso, e' un'eccezione rara.
                    tracing::warn!(
                        message_id = %message_id,
                        error = %e,
                        "wiki.chat_note: ingest_message fallito (retry al prossimo giro)"
                    );
                }
            }
        }
    }

    Ok(ingested)
}

/// Crea il wiki_doc + embedding per un singolo messaggio. Idempotente via
/// slug deterministico (`chat-{message_id}`) e ON CONFLICT.
async fn ingest_message(
    state: &WikiDeps,
    message_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    content: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    // Titolo: primi 80 char del contenuto, una linea sola.
    let title = build_title(content, 80);
    let slug = format!("chat-{}", message_id.simple());

    let body_md = format!(
        "# Messaggio chat\n\n\
         **Origine**: chat utente\n\
         **Data**: {created}\n\
         **Sessione**: `{session}`\n\
         **Messaggio**: `{message}`\n\n\
         ---\n\n{content}\n",
        created = created_at.to_rfc3339(),
        session = session_id,
        message = message_id,
        content = content
    );
    let body_hash = crate::vault::sha256_hex(&body_md);

    // content_hash: hash del CONTENUTO UTENTE normalizzato (trim), NON del
    // body_md arricchito. body_md include created_at/session_id/message_id e
    // sarebbe sempre diverso anche per testi identici (mig 0314). Questo hash
    // intercetta i duplicati reali (stesso testo, messaggi distinti).
    let content_hash = crate::vault::sha256_hex(content.trim());

    // Prevenzione duplicati di contenuto (mig 0314): se esiste gia' un chat_note
    // con lo stesso (scope, project_id, content_hash) lo saltiamo. L'indice
    // UNIQUE parziale uq_wiki_docs_chat_note_content e' la garanzia hard lato DB;
    // questo SELECT evita di sprecare un embedding per un doc che non verra'
    // creato. Il messaggio viene comunque marcato kb_ingested dal chiamante.
    let existing_content_dup: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM wiki_docs \
         WHERE kind = 'chat_note' AND scope = 'project' \
           AND project_id = $1 AND content_hash = $2 \
           AND slug <> $3 LIMIT 1",
    )
    .bind(project_id)
    .bind(&content_hash)
    .bind(&slug)
    .fetch_optional(&state.db)
    .await
    .context("SELECT content_hash duplicato chat_note")?;
    if existing_content_dup.is_some() {
        tracing::debug!(
            message_id = %message_id,
            "wiki.chat_note: contenuto duplicato (content_hash), skip ingest"
        );
        return Ok(false);
    }

    // Embed + upsert Qdrant (best-effort).
    let snippet = if content.len() > 2000 {
        &content[..2000]
    } else {
        content
    };
    let combined = format!("{title}\n\n{snippet}");
    // Id documento fissato qui per usarlo come id del punto Qdrant: il contratto
    // `wiki_docs.id == qdrant_point_id == payload.doc_id` e' richiesto dal link
    // semantico (process_semantic). Su re-run riusiamo l'id esistente.
    let doc_uuid: Uuid = sqlx::query_scalar(
        "SELECT id FROM wiki_docs WHERE scope = 'project' AND project_id = $1 AND slug = $2",
    )
    .bind(project_id)
    .bind(&slug)
    .fetch_optional(&state.db)
    .await
    .context("SELECT id wiki_docs chat_note esistente")?
    .unwrap_or_else(Uuid::new_v4);
    let qdrant_point_id: Option<String> =
        match state.ai.embed_text("", &combined).await {
            Ok(vector) => {
                let point_id = doc_uuid.to_string();
                let payload = json!({
                    "scope": "project",
                    "project_id": project_id.to_string(),
                    "doc_id": point_id,
                    "title": title.clone(),
                    "kind": "chat_note",
                    "session_id": session_id.to_string(),
                });
                match crate::content_points::upsert_wiki_content_point(
                    &state.db, &point_id, vector, payload,
                )
                .await
                {
                    Ok(_) => Some(point_id),
                    Err(e) => {
                        tracing::debug!(
                            message_id = %message_id,
                            error = %e,
                            "wiki.chat_note: upsert Qdrant fallito (proseguo)"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    message_id = %message_id,
                    error = %e,
                    "wiki.chat_note: embed_text fallito (proseguo senza vector)"
                );
                None
            }
        };

    let tags: Vec<String> = vec!["chat".to_string(), "auto".to_string()];
    sqlx::query(
        r#"
        INSERT INTO wiki_docs (
            id, scope, project_id, slug, title, body_md, body_hash,
            kind, tags, qdrant_point_id, edited_by,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read, content_hash
        ) VALUES (
            $8, 'project', $1, $2, $3, $4, $5,
            'chat_note', $6, $7, 'chat_message',
            'none', '{}', FALSE,
            $5, NULL,
            1, TRUE, FALSE, $9
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            -- Aggiorna solo metadati: il body chat non cambia (i messaggi sono immutabili).
            qdrant_point_id = COALESCE(wiki_docs.qdrant_point_id, EXCLUDED.qdrant_point_id),
            content_hash = COALESCE(wiki_docs.content_hash, EXCLUDED.content_hash),
            updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(&tags)
    .bind(qdrant_point_id.as_deref())
    .bind(doc_uuid)
    .bind(&content_hash)
    .execute(&state.db)
    .await
    .context("INSERT wiki_docs chat_note")?;

    Ok(true)
}

/// Imposta `kb_ingested = TRUE` per un message_id. Errori loggati a debug
/// (non bloccante: nel peggiore dei casi il messaggio verra' rivisto al prossimo giro).
async fn mark_processed(db: &PgPool, message_id: Uuid) {
    if let Err(e) = sqlx::query(
        "UPDATE chat_messages SET kb_ingested = TRUE WHERE id = $1 AND kb_ingested IS NULL",
    )
    .bind(message_id)
    .execute(db)
    .await
    {
        tracing::debug!(
            message_id = %message_id,
            error = %e,
            "wiki.chat_note: mark_processed fallito"
        );
    }
}

/// Costruisce un titolo da `content`: prima riga non vuota, taglio a `max_chars`.
fn build_title(content: &str, max_chars: usize) -> String {
    let first_line = content
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or(content.trim());
    if first_line.is_empty() {
        return "Messaggio chat".to_string();
    }
    let truncated: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        format!("{truncated}…")
    } else {
        truncated
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests (puri)
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_uses_first_non_empty_line() {
        let t = build_title("\n\nFammi vedere i log degli ultimi 30 minuti\n", 80);
        assert_eq!(t, "Fammi vedere i log degli ultimi 30 minuti");
    }

    #[test]
    fn title_truncates_long_lines() {
        let long = "a".repeat(120);
        let t = build_title(&long, 80);
        assert!(t.ends_with('…'));
        assert_eq!(t.chars().count(), 81); // 80 'a' + ellissi
    }

    #[test]
    fn skip_regex_catches_short_acks() {
        let re = Regex::new("(?i)^(ok|si|no|grazie)[.!?\\s]*$").unwrap();
        assert!(re.is_match("ok"));
        assert!(re.is_match("Grazie!"));
        assert!(re.is_match("no."));
        assert!(!re.is_match("ok procedi"));
    }
}
