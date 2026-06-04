//! M12.3 — Link composer triplo per le note KB create automaticamente.
//!
//! Quando `ingest_run` crea una nota `agent_summary`/`subagent_summary`, questa
//! funzione popola il grafo `project_knowledge_links` con tre strategie
//! indipendenti e complementari, tutte `created_by='auto'`:
//!
//!   1. **Strutturale** (deterministico, no embedding):
//!      - `parent_run_id` → nota del parent (lookup `source_run_id`) → `followup` conf 1.0
//!      - file_paths in comune con note esistenti → `relates` conf 0.85
//!   2. **Semantico** (riuso `search_knowledge_points`):
//!      - top-K simili in Qdrant filtrati per project_id, escluso self → `relates` conf=score
//!        (solo score >= `kb.autolink.semantic_threshold`)
//!   3. **Wikilink** espliciti nel body (`[[Titolo]]`):
//!      - risolti per title case-insensitive → `refinement` (cap `kb.autolink.wikilink_max_per_note`)
//!
//! Tutti i link sono idempotenti: `ON CONFLICT (from_note_id, to_note_id, rel_type)`.
//! I link strutturali hanno precedenza: una coppia gia' linkata al passo 1 non
//! viene ri-linkata con `relates` al passo 2 (si evita rumore di rel_type misti).
//!
//! Best-effort: ogni passo degrada da solo con log warn; un fallimento di
//! embedding non blocca i link strutturali e viceversa. La feature resta ON.
//!
//! Settings (mig 0240): `kb.autolink.enabled`, `kb.autolink.semantic_threshold`,
//! `kb.autolink.semantic_top_k`, `kb.autolink.wikilink_max_per_note`.

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::orchestrator::NeuralCoreClient;

/// Regex Obsidian-style `[[Titolo della nota]]`. Cattura il contenuto interno.
static WIKILINK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[\[([^\]]{1,200})\]\]").unwrap());

/// Input per il link composer. `embed_vector` opzionale: se fornito (gia'
/// calcolato in ingest_run) evita un secondo embedding per il passo semantico.
pub struct NewNoteLinkInput {
    pub project_id: Uuid,
    pub note_id: Uuid,
    pub body_md: String,
    pub file_paths: Vec<String>,
    /// Run sorgente della nota (per escludere note dello stesso run dai semantici).
    pub source_run_id: Option<Uuid>,
    /// Parent run (sub-agent): se presente, link `followup` verso la nota del parent.
    pub parent_run_id: Option<Uuid>,
    /// Vettore embedding gia' calcolato (riuso). Se None, il passo semantico ri-embedda.
    pub embed_vector: Option<Vec<f32>>,
    /// Testo da embeddare se `embed_vector` e' None (title + body troncato).
    pub embed_fallback_text: String,
}

async fn autolink_enabled(db: &PgPool) -> bool {
    read_bool_setting(db, "kb.autolink.enabled", true).await
}

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

async fn read_int_setting(db: &PgPool, key: &str, default: i32) -> i32 {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(default)
}

async fn read_float_setting(db: &PgPool, key: &str, default: f64) -> f64 {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

/// Inserisce un link idempotente. Ritorna true se ha creato/aggiornato una riga.
/// `created_by='auto'`, gestisce il vincolo from != to silenziosamente.
async fn insert_link(
    db: &PgPool,
    from_note_id: Uuid,
    to_note_id: Uuid,
    rel_type: &str,
    confidence: f32,
) -> bool {
    if from_note_id == to_note_id {
        return false;
    }
    let res = sqlx::query(
        r#"
        INSERT INTO project_knowledge_links (from_note_id, to_note_id, rel_type, created_by, confidence)
        VALUES ($1, $2, $3, 'auto', $4)
        ON CONFLICT (from_note_id, to_note_id, rel_type)
        DO UPDATE SET confidence = GREATEST(project_knowledge_links.confidence, EXCLUDED.confidence)
        "#,
    )
    .bind(from_note_id)
    .bind(to_note_id)
    .bind(rel_type)
    .bind(confidence.clamp(0.0, 1.0))
    .execute(db)
    .await;
    matches!(res, Ok(r) if r.rows_affected() > 0)
}

/// Punto di ingresso M12.3. Esegue i tre passi e ritorna il numero di link creati.
/// Best-effort: non propaga errori.
pub async fn build_links_for_new_note(
    db: &PgPool,
    neural: &NeuralCoreClient,
    input: NewNoteLinkInput,
) -> usize {
    if !autolink_enabled(db).await {
        tracing::debug!(note_id = %input.note_id, "kb.autolink disabled, skip");
        return 0;
    }

    // `linked` traccia le note gia' collegate (a qualunque rel_type) per evitare
    // che il passo semantico re-linki con `relates` cio' che e' gia' `followup`/`relates`.
    let mut linked: HashSet<Uuid> = HashSet::new();
    let mut created = 0usize;

    // ── Passo 1a: link followup verso la nota del parent run ────────────────
    if let Some(parent) = input.parent_run_id {
        if let Some(parent_note) =
            find_note_by_run(db, input.project_id, parent, input.note_id).await
        {
            if insert_link(db, input.note_id, parent_note, "followup", 1.0).await {
                created += 1;
            }
            linked.insert(parent_note);
        }
    }

    // ── Passo 1b: link relates verso note con file_paths in comune ──────────
    if !input.file_paths.is_empty() {
        let related =
            find_notes_by_file_paths(db, input.project_id, &input.file_paths, input.note_id, 5)
                .await;
        for nid in related {
            if linked.contains(&nid) {
                continue;
            }
            if insert_link(db, input.note_id, nid, "relates", 0.85).await {
                created += 1;
            }
            linked.insert(nid);
        }
    }

    // ── Passo 2: link semantici via Qdrant top-K ────────────────────────────
    let threshold = read_float_setting(db, "kb.autolink.semantic_threshold", 0.65).await;
    let top_k = read_int_setting(db, "kb.autolink.semantic_top_k", 3)
        .await
        .max(0) as usize;
    if top_k > 0 {
        let vector = match input.embed_vector.clone() {
            Some(v) => Some(v),
            None => {
                let slice = if input.embed_fallback_text.len() > 2000 {
                    &input.embed_fallback_text[..2000]
                } else {
                    &input.embed_fallback_text
                };
                match neural.embed_text("", slice).await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(
                            note_id = %input.note_id, error = %e,
                            "kb.autolink: embed per ricerca semantica fallito, salto passo semantico"
                        );
                        None
                    }
                }
            }
        };
        if let Some(vector) = vector {
            // top_k + 1 perche' il self appare quasi sempre con score ~1.0.
            match crate::vector_memory::search_knowledge_points(
                db,
                vector,
                input.project_id,
                top_k + 1,
            )
            .await
            {
                Ok(hits) => {
                    for hit in hits {
                        if (hit.score as f64) < threshold {
                            continue;
                        }
                        let other_id = match hit
                            .payload
                            .get("note_id")
                            .and_then(|v| v.as_str())
                            .and_then(|s| Uuid::parse_str(s).ok())
                        {
                            Some(id) => id,
                            None => continue,
                        };
                        if other_id == input.note_id || linked.contains(&other_id) {
                            continue;
                        }
                        if insert_link(db, input.note_id, other_id, "relates", hit.score as f32)
                            .await
                        {
                            created += 1;
                        }
                        linked.insert(other_id);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        note_id = %input.note_id, error = %e,
                        "kb.autolink: search_knowledge_points fallita, salto passo semantico"
                    );
                }
            }
        }
    }

    // ── Passo 3: wikilink espliciti [[Titolo]] nel body ─────────────────────
    let wikilink_cap = read_int_setting(db, "kb.autolink.wikilink_max_per_note", 10)
        .await
        .max(0) as usize;
    if wikilink_cap > 0 {
        let mut titles_seen: HashSet<String> = HashSet::new();
        let mut wikilink_count = 0usize;
        for cap in WIKILINK_RE.captures_iter(&input.body_md) {
            if wikilink_count >= wikilink_cap {
                break;
            }
            let raw_title = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if raw_title.is_empty() {
                continue;
            }
            // Supporta `[[Titolo|alias]]`: usa solo la parte prima di `|`.
            let title = raw_title
                .split('|')
                .next()
                .unwrap_or(raw_title)
                .trim()
                .to_lowercase();
            if title.is_empty() || !titles_seen.insert(title.clone()) {
                continue;
            }
            if let Some(target) =
                resolve_note_by_title(db, input.project_id, &title, input.note_id).await
            {
                if linked.contains(&target) {
                    continue;
                }
                if insert_link(db, input.note_id, target, "refinement", 0.9).await {
                    created += 1;
                }
                linked.insert(target);
                wikilink_count += 1;
            }
        }
    }

    if created > 0 {
        tracing::info!(
            note_id = %input.note_id,
            links = created,
            "kb.autolink: link composer ha creato {} link",
            created
        );
    }
    created
}

/// Trova la nota associata a un run (lookup `source_run_id`), escludendo `exclude`.
async fn find_note_by_run(
    db: &PgPool,
    project_id: Uuid,
    run_id: Uuid,
    exclude: Uuid,
) -> Option<Uuid> {
    sqlx::query(
        r#"
        SELECT id FROM project_knowledge_notes
        WHERE project_id = $1 AND source_run_id = $2 AND id != $3
        ORDER BY created_at ASC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(run_id)
    .bind(exclude)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get::<Uuid, _>("id").ok())
}

/// Trova note (esclusa `exclude`) che condividono almeno un file_path con quelli dati.
/// Usa l'indice GIN `idx_pkn_file_paths_gin` via overlap `&&`.
async fn find_notes_by_file_paths(
    db: &PgPool,
    project_id: Uuid,
    file_paths: &[String],
    exclude: Uuid,
    limit: i64,
) -> Vec<Uuid> {
    let rows = sqlx::query(
        r#"
        SELECT id FROM project_knowledge_notes
        WHERE project_id = $1
          AND status IN ('active', 'draft')
          AND file_paths && $2
          AND id != $3
        ORDER BY updated_at DESC
        LIMIT $4
        "#,
    )
    .bind(project_id)
    .bind(file_paths)
    .bind(exclude)
    .bind(limit)
    .fetch_all(db)
    .await
    .ok()
    .unwrap_or_default();
    rows.into_iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect()
}

/// Risolve una nota per titolo (case-insensitive, match esatto o prefisso),
/// escludendo `exclude`. Preferisce note piu' vecchie (la nota "canonica" a cui
/// la nuova fa riferimento dovrebbe precederla).
async fn resolve_note_by_title(
    db: &PgPool,
    project_id: Uuid,
    title_lower: &str,
    exclude: Uuid,
) -> Option<Uuid> {
    sqlx::query(
        r#"
        SELECT id FROM project_knowledge_notes
        WHERE project_id = $1
          AND status IN ('active', 'draft')
          AND id != $3
          AND lower(title) = $2
        ORDER BY created_at ASC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(title_lower)
    .bind(exclude)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get::<Uuid, _>("id").ok())
}
