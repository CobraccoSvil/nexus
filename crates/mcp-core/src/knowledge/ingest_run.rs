//! M12.1 — Ingestione automatica del resoconto di un run completato nella KB.
//!
//! Hook post-run: a fine `finalize_agent_run` (run completato con successo),
//! crea una nota `kind='agent_summary'` con il `final_answer`, la indicizza in
//! Qdrant e la collega nel grafo via `auto_link::build_links_for_new_note`
//! (M12.3). Best-effort: ogni errore e' loggato, mai propagato (non deve mai
//! rompere la chiusura del run).
//!
//! Filtri (regola G, settings DB):
//!   - `kb.ingest.enabled` (default true) — master switch
//!   - status = Completed AND iteration_count >= 1
//!   - len(final_answer) >= `kb.ingest.min_chars` (default 300)
//!   - CJK guard: skip se ratio caratteri CJK >= `kb.ingest.cjk_max_ratio_pct`
//!     (default 20) — evita di ingestare allucinazioni in cinese/giapponese.
//!
//! Idempotenza: pre-check su `source_run_id` + `kind='agent_summary'` (NON su
//! source_message_id, che e' il messaggio utente gia' usato dalla nota 'chat').

use serde_json::json;
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

use crate::knowledge::auto_link::{self, NewNoteLinkInput};
use crate::orchestrator::NeuralCoreClient;

async fn read_bool_setting(db: &PgPool, key: &str, default: bool) -> bool {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        )
    })
    .unwrap_or(default)
}

async fn read_int_setting(db: &PgPool, key: &str, default: i64) -> i64 {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// True se la frazione di caratteri CJK supera la soglia (allucinazione lingua).
fn cjk_ratio_exceeds(text: &str, max_ratio_pct: i64) -> bool {
    if max_ratio_pct >= 100 {
        return false;
    }
    let mut cjk = 0usize;
    let mut letters = 0usize;
    for c in text.chars() {
        let u = c as u32;
        // Hiragana, Katakana, Hangul, CJK Unified Ideographs (+ ext A), Fullwidth.
        let is_cjk = (0x3040..=0x30FF).contains(&u)
            || (0xAC00..=0xD7AF).contains(&u)
            || (0x4E00..=0x9FFF).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || (0xFF00..=0xFFEF).contains(&u);
        if is_cjk {
            cjk += 1;
            letters += 1;
        } else if c.is_alphabetic() {
            letters += 1;
        }
    }
    if letters == 0 {
        return false;
    }
    (cjk * 100 / letters) as i64 >= max_ratio_pct
}

/// Estrae i file modificati dal run leggendo gli agent_steps write_file/edit_file.
async fn extract_modified_files(db: &PgPool, run_id: Uuid) -> Vec<String> {
    let rows = sqlx::query(
        "SELECT tool_input FROM agent_steps \
         WHERE run_id = $1 AND tool_name IN ('write_file','edit_file') \
         AND tool_input IS NOT NULL",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut paths: Vec<String> = Vec::new();
    for row in rows {
        let ti: Option<serde_json::Value> = row.try_get("tool_input").ok();
        if let Some(v) = ti {
            for key in ["path", "file_path", "abs_path"] {
                if let Some(p) = v.get(key).and_then(|x| x.as_str()) {
                    if !p.is_empty() && !paths.iter().any(|e| e == p) {
                        paths.push(p.to_string());
                    }
                    break;
                }
            }
        }
    }
    paths
}

fn title_from_answer(task_type: &str, answer: &str) -> String {
    let first_line = answer.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let base = if task_type.is_empty() {
        "Run"
    } else {
        task_type
    };
    let mut t = format!("{}: {}", base, first_line.trim());
    if t.chars().count() > 120 {
        t = t.chars().take(117).collect::<String>() + "...";
    }
    t
}

/// Hook principale M12.1. Crea la nota agent_summary per il run completato.
/// `neural` serve per l'embedding (riusato dal link composer per il semantico).
/// `channels` serve per il promote M14.1 (eventi KnowledgeNoteUpdated).
pub async fn ingest_run_summary_to_kb(
    db: &PgPool,
    neural: &NeuralCoreClient,
    channels: &nexus_events::ProjectChannels,
    run_id: Uuid,
) {
    if !read_bool_setting(db, "kb.ingest.enabled", true).await {
        return;
    }

    // Snapshot del run.
    let row = match sqlx::query(
        "SELECT project_id, final_answer, iteration_count, status, \
                COALESCE(nexus_task_type,'') AS task_type, \
                COALESCE(nexus_agent_type,'') AS agent_type \
         FROM agent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => r,
        _ => return,
    };

    let project_id: Uuid = match row.try_get("project_id") {
        Ok(p) => p,
        Err(_) => return,
    };
    let final_answer: String = row
        .try_get("final_answer")
        .ok()
        .flatten()
        .unwrap_or_default();
    let iteration_count: i32 = row.try_get("iteration_count").unwrap_or(0);
    let task_type: String = row.try_get("task_type").unwrap_or_default();
    let agent_type: String = row.try_get("agent_type").unwrap_or_default();
    let status: String = row.try_get("status").unwrap_or_default();

    // File modificati dal run: serve sia al lifecycle KB (subito sotto) sia alla
    // nota agent_summary creata piu' avanti. Estratto una sola volta.
    let file_paths = extract_modified_files(db, run_id).await;

    // M14.1/M14.3 — Lifecycle KB: il promote delle note 'draft' -> 'active' e il
    // flag context-stale NON dipendono dalla "substance" del summary. Vanno
    // eseguiti a OGNI run completato, ANCHE quando l'ingest del summary viene
    // skippato (risposta corta < kb.ingest.min_chars o task banale). Prima erano
    // dopo i filtri substance: i task brevi lasciavano le note chat per sempre
    // in 'draft'. Spostati qui (regola H: fix della causa radice "le note
    // restano in bozza").
    if status == "completed" {
        // M15.4 — Backlog cross-run: i todo non completati di questo run vengono
        // marcati carry_over cosi' il run successivo li eredita come backlog.
        // origin_run_id preserva il run che li ha originati (idempotente).
        let _ = sqlx::query(
            "UPDATE nexus_agent_todos \
             SET carry_over = true, origin_run_id = COALESCE(origin_run_id, run_id) \
             WHERE run_id = $1 AND status NOT IN ('completed', 'cancelled')",
        )
        .bind(run_id)
        .execute(db)
        .await;

        if read_bool_setting(db, "kb.lifecycle.promote_enabled", true).await {
            crate::knowledge::promote_notes_on_run_completed(
                db,
                run_id,
                &file_paths,
                channels,
                project_id,
                &final_answer,
            )
            .await;
        }
        if !file_paths.is_empty()
            && read_bool_setting(db, "kb.lifecycle.context_stale_enabled", true).await
        {
            let res = sqlx::query(
                "UPDATE project_knowledge_notes \
                 SET context_stale_at = NOW() \
                 WHERE project_id = $1 AND status = 'active' \
                   AND file_paths && $2 \
                   AND (source_run_id IS NULL OR source_run_id <> $3) \
                   AND context_stale_at IS NULL",
            )
            .bind(project_id)
            .bind(&file_paths)
            .bind(run_id)
            .execute(db)
            .await;
            if let Ok(r) = res {
                if r.rows_affected() > 0 {
                    tracing::info!(run_id = %run_id, flagged = r.rows_affected(), "kb.lifecycle: note marcate context-stale");
                }
            }
        }
    }

    // Filtri substance.
    if iteration_count < 1 {
        return;
    }
    let min_chars = read_int_setting(db, "kb.ingest.min_chars", 300).await as usize;
    if final_answer.chars().count() < min_chars
        || final_answer.starts_with("[brain error")
        || final_answer.starts_with("[Error")
    {
        return;
    }
    let cjk_max = read_int_setting(db, "kb.ingest.cjk_max_ratio_pct", 20).await;
    if cjk_ratio_exceeds(&final_answer, cjk_max) {
        tracing::warn!(run_id = %run_id, "kb.ingest: skip nota (CJK ratio oltre soglia, probabile allucinazione lingua)");
        return;
    }

    // Idempotenza: nota agent_summary gia' presente per questo run.
    let already: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM project_knowledge_notes \
         WHERE source_run_id = $1 AND kind = 'agent_summary' LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if already.is_some() {
        return;
    }

    // NB: il lifecycle KB (promote + context-stale) e l'estrazione di file_paths
    // sono ora eseguiti in cima alla funzione (prima dei filtri substance), cosi'
    // le note vengono promosse anche per i run con summary non ingeribile.

    let note_id = Uuid::new_v4();
    let body_max = read_int_setting(db, "kb.ingest.body_max_chars", 20000).await as usize;
    let body_md: String = final_answer.chars().take(body_max).collect();
    let title = title_from_answer(&task_type, &body_md);
    let mut tags = vec!["kind:agent_summary".to_string()];
    if !agent_type.is_empty() {
        tags.push(format!("agent:{}", agent_type));
    }
    if !task_type.is_empty() {
        tags.push(format!("task:{}", task_type));
    }

    // Embedding + Qdrant (best-effort). Il vettore viene riusato dal link
    // composer (M12.3) per il passo semantico, evitando un secondo embedding.
    let embed_slice: String = body_md.chars().take(2000).collect();
    let mut embed_vector: Option<Vec<f32>> = None;
    let qdrant_point_id: Option<String> = match neural.embed_text("", &embed_slice).await {
        Ok(vector) => {
            embed_vector = Some(vector.clone());
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "project_id": project_id.to_string(),
                "note_id": note_id.to_string(),
                "intent": "agent_summary",
                "status": "active",
                "kind": "agent_summary",
            });
            match crate::vector_memory::upsert_knowledge_point(db, &point_id, vector, payload).await
            {
                Ok(_) => Some(point_id),
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = %e, "kb.ingest: Qdrant upsert fallito (procedo senza embedding)");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(run_id = %run_id, error = %e, "kb.ingest: embed_text fallito (procedo senza embedding)");
            None
        }
    };

    // INSERT nota. source_message_id = NULL (l'unicita' e' garantita dal
    // pre-check su source_run_id). status='active' (M14.1: i resoconti nascono
    // gia' attivi, non draft).
    let res = sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes
            (id, project_id, source_run_id, source_message_id, intent, title, body_md,
             status, qdrant_point_id, tags, file_paths, access_count, created_at, updated_at,
             kind, off_topic, source_kind)
        VALUES ($1, $2, $3, NULL, 'agent_summary', $4, $5,
                'active', $6, $7, $8, 0, NOW(), NOW(),
                'agent_summary', false, 'agent')
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(run_id)
    .bind(&title)
    .bind(&body_md)
    .bind(&qdrant_point_id)
    .bind(&tags)
    .bind(&file_paths)
    .execute(db)
    .await;

    if let Err(e) = res {
        tracing::warn!(run_id = %run_id, error = %e, "kb.ingest: INSERT nota fallito");
        return;
    }

    tracing::info!(run_id = %run_id, note_id = %note_id, files = file_paths.len(), "kb.ingest: nota agent_summary creata");

    // M14.2 — Deprecazione su correzione: questo nuovo summary supera le note
    // active precedenti che riferiscono gli stessi file (il codice e' cambiato).
    // Gated da kb.lifecycle.auto_deprecate_on_correction.
    if read_bool_setting(db, "kb.lifecycle.auto_deprecate_on_correction", true).await {
        crate::knowledge::deprecate_notes_on_correction(
            db,
            project_id,
            note_id,
            &file_paths,
            channels,
        )
        .await;
    }

    // Link composer M12.3 (best-effort).
    let links = auto_link::build_links_for_new_note(
        db,
        neural,
        NewNoteLinkInput {
            project_id,
            note_id,
            body_md: body_md.clone(),
            file_paths,
            source_run_id: Some(run_id),
            parent_run_id: None,
            embed_vector,
            embed_fallback_text: format!("{}\n{}", title, embed_slice),
        },
    )
    .await;
    if links > 0 {
        tracing::info!(run_id = %run_id, note_id = %note_id, links, "kb.ingest: link auto creati");
    }
}
