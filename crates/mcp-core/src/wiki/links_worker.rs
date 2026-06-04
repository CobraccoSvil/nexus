// ═══════════════════════════════════════════════════════════════════════════
// wiki/links_worker.rs — Fase 4 ADR 0017 v2.
//
// Popola `wiki_links` con due strategie complementari:
//
//   1. **Wikilink testuali**: parse `[[Target]]` dal body_md tramite
//      `vault::extract_wikilinks`, risoluzione del target nello scope locale
//      (e cross-scope automatica verso meta `public_read=true`), INSERT con
//      rel_type=`mentions`, confidence 0.95, evidence `wikilink [[<target>]]`.
//
//   2. **Semantica via Qdrant**: per ogni doc con `qdrant_point_id`, GET vector
//      dalla collection `wiki_content`, search top-K con score_threshold, filtro
//      ACL (scope locale + meta public), INSERT con rel_type=`relates`,
//      confidence=score, evidence `semantic cosine=<score>`.
//
// Tuning DB-driven (mig 0296):
//   - `agent.wiki.semantic_link_threshold`  (default safe 0.60, NO fallback HC)
//   - `agent.wiki.semantic_link_top_k`      (default safe 10)
//   - `agent.wiki.link_worker_enabled`      (default safe true)
//
// Niente fallback hardcoded: se un setting manca il worker logga WARN e usa
// le safe_defaults (analogo pattern a `agent_tools::attachment_settings`),
// cosi' un DB up con tabella settings vuota non blocca la pipeline ma rimane
// visibile all'amministratore.
// ═══════════════════════════════════════════════════════════════════════════

use crate::vector_memory::{get_wiki_content_point_vector, search_wiki_content_points};
use crate::wiki::model::WikiScope;
use crate::wiki::vault::extract_wikilinks;
use crate::AppState;
use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::PgPool;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct LinkWorkerSettings {
    pub semantic_threshold: f32,
    pub semantic_top_k: usize,
    pub enabled: bool,
    pub interval_secs: u64,
}

impl LinkWorkerSettings {
    /// Ultima rete di sicurezza se il DB ha settings vuoto/corrotto. Allineati
    /// ai valori della migrazione 0296 cosi' che il comportamento sia identico.
    const fn safe_defaults() -> Self {
        Self {
            semantic_threshold: 0.60,
            semantic_top_k: 10,
            enabled: true,
            interval_secs: 1800,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<
    RwLock<Option<(LinkWorkerSettings, Instant)>>,
> = once_cell::sync::Lazy::new(|| RwLock::new(None));

/// Carica i settings con cache 60s. Mancanze loggano WARN + safe_defaults.
pub async fn current_settings(db: &PgPool) -> LinkWorkerSettings {
    {
        let guard = SETTINGS_CACHE.read().await;
        if let Some((v, exp)) = *guard {
            if Instant::now() < exp {
                return v;
            }
        }
    }

    let value = match load_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "wiki.links: lettura settings agent.wiki.* fallita, uso safe_defaults"
            );
            LinkWorkerSettings::safe_defaults()
        }
    };

    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value, Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<LinkWorkerSettings> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.semantic_link_threshold', \
            'agent.wiki.semantic_link_top_k', \
            'agent.wiki.link_worker_enabled', \
            'agent.wiki.link_worker_interval_secs' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.*")?;

    let mut out = LinkWorkerSettings::safe_defaults();
    let mut seen: HashSet<&'static str> = HashSet::new();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        match key.as_str() {
            "agent.wiki.semantic_link_threshold" => {
                if let Ok(v) = raw.trim().parse::<f32>() {
                    out.semantic_threshold = v.clamp(0.0, 1.0);
                    seen.insert("threshold");
                }
            }
            "agent.wiki.semantic_link_top_k" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.semantic_top_k = v.clamp(1, 200);
                    seen.insert("top_k");
                }
            }
            "agent.wiki.link_worker_enabled" => {
                out.enabled = !matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                );
                seen.insert("enabled");
            }
            "agent.wiki.link_worker_interval_secs" => {
                if let Ok(v) = raw.trim().parse::<u64>() {
                    out.interval_secs = v.max(60);
                    seen.insert("interval");
                }
            }
            _ => {}
        }
    }
    // Se DB up ma chiavi mancanti (mig 0296 non applicata), log INFO una volta.
    if seen.len() < 4 {
        tracing::info!(
            present = seen.len(),
            "wiki.links: alcuni settings agent.wiki.* assenti, applico safe_defaults"
        );
    }
    Ok(out)
}

#[cfg(test)]
pub async fn _reset_settings_cache_for_tests() {
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = None;
}

// ───────────────────────────────────────────────────────────────────────────
// Report
// ───────────────────────────────────────────────────────────────────────────

/// Report dell'esecuzione del worker, ritornato sia dall'endpoint sincrono che
/// dai log del task background.
#[derive(Debug, Default, Serialize, Clone)]
pub struct RecomputeReport {
    pub docs_scanned: usize,
    pub wikilinks_resolved: usize,
    pub wikilinks_unresolved: usize,
    pub semantic_links_created: usize,
    pub semantic_links_updated: usize,
    pub errors: Vec<String>,
    pub elapsed_ms: u128,
}

// ───────────────────────────────────────────────────────────────────────────
// Helpers DB
// ───────────────────────────────────────────────────────────────────────────

/// Riga minimale per il worker: id, scope, project_id, title, body_md,
/// qdrant_point_id, public_read. Tutto cio' che serve per risolvere wikilink
/// e fare semantic search.
#[derive(Debug, Clone, sqlx::FromRow)]
struct DocRow {
    id: Uuid,
    scope: String,
    project_id: Option<Uuid>,
    title: String,
    body_md: String,
    qdrant_point_id: Option<String>,
    #[allow(dead_code)]
    public_read: bool,
}

async fn fetch_doc(db: &PgPool, doc_id: Uuid) -> Result<Option<DocRow>> {
    let row = sqlx::query_as::<_, DocRow>(
        "SELECT id, scope, project_id, title, body_md, qdrant_point_id, public_read \
         FROM wiki_docs WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(db)
    .await
    .context("SELECT wiki_docs per links worker")?;
    Ok(row)
}

async fn fetch_docs_for_scope(
    db: &PgPool,
    scope: Option<WikiScope>,
    project_id: Option<Uuid>,
) -> Result<Vec<DocRow>> {
    let rows = match (scope, project_id) {
        (None, _) => sqlx::query_as::<_, DocRow>(
            "SELECT id, scope, project_id, title, body_md, qdrant_point_id, public_read \
             FROM wiki_docs ORDER BY updated_at DESC",
        )
        .fetch_all(db)
        .await
        .context("SELECT wiki_docs (all)")?,
        (Some(s), None) => sqlx::query_as::<_, DocRow>(
            "SELECT id, scope, project_id, title, body_md, qdrant_point_id, public_read \
             FROM wiki_docs WHERE scope = $1 ORDER BY updated_at DESC",
        )
        .bind(s.as_str())
        .fetch_all(db)
        .await
        .context("SELECT wiki_docs (scope)")?,
        (Some(s), Some(pid)) => sqlx::query_as::<_, DocRow>(
            "SELECT id, scope, project_id, title, body_md, qdrant_point_id, public_read \
             FROM wiki_docs WHERE scope = $1 AND project_id = $2 ORDER BY updated_at DESC",
        )
        .bind(s.as_str())
        .bind(pid)
        .fetch_all(db)
        .await
        .context("SELECT wiki_docs (scope+project)")?,
    };
    Ok(rows)
}

/// Risolve un target wikilink applicando le regole ACL del worker:
///   - prima nello scope locale (stesso scope + stesso project_id, slug o title)
///   - se non trovato e doc sorgente == project: cerca in scope=meta SE
///     `public_read=true`
///   - se non trovato e doc sorgente == meta: cerca progetto (qualunque) — il
///     worker e' sistema, ACL utente arriva poi sull'endpoint graph
fn normalize_target(raw: &str) -> String {
    raw.trim().trim_start_matches('!').to_string()
}

async fn resolve_wikilink_target(
    db: &PgPool,
    from_scope: &str,
    from_project_id: Option<Uuid>,
    target: &str,
) -> Result<Option<Uuid>> {
    let needle = normalize_target(target);
    if needle.is_empty() {
        return Ok(None);
    }

    // 1) Stesso scope + stesso project_id.
    let local: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM wiki_docs \
         WHERE scope = $1 \
           AND project_id IS NOT DISTINCT FROM $2 \
           AND (slug = $3 OR title ILIKE $3) \
         LIMIT 1",
    )
    .bind(from_scope)
    .bind(from_project_id)
    .bind(&needle)
    .fetch_optional(db)
    .await
    .context("SELECT wiki_docs lookup wikilink locale")?;

    if local.is_some() {
        return Ok(local);
    }

    // 2) Cross-scope automatico.
    let cross: Option<Uuid> = match from_scope {
        "project" => sqlx::query_scalar(
            "SELECT id FROM wiki_docs \
             WHERE scope = 'meta' AND public_read = TRUE \
               AND (slug = $1 OR title ILIKE $1) \
             LIMIT 1",
        )
        .bind(&needle)
        .fetch_optional(db)
        .await
        .context("SELECT wiki_docs lookup wikilink cross-scope meta")?,
        "meta" => sqlx::query_scalar(
            "SELECT id FROM wiki_docs \
             WHERE scope = 'project' \
               AND (slug = $1 OR title ILIKE $1) \
             ORDER BY updated_at DESC \
             LIMIT 1",
        )
        .bind(&needle)
        .fetch_optional(db)
        .await
        .context("SELECT wiki_docs lookup wikilink cross-scope project")?,
        _ => None,
    };
    Ok(cross)
}

/// INSERT idempotente di un link con strategia di update sul max(confidence).
async fn upsert_link(
    db: &PgPool,
    from_doc_id: Uuid,
    to_doc_id: Uuid,
    rel_type: &str,
    confidence: f32,
    evidence: &str,
) -> Result<bool> {
    if from_doc_id == to_doc_id {
        return Ok(false);
    }
    let row: Option<(bool,)> = sqlx::query_as(
        r#"
        INSERT INTO wiki_links (from_doc_id, to_doc_id, rel_type, confidence, created_by, evidence)
        VALUES ($1, $2, $3, $4, 'auto', $5)
        ON CONFLICT (from_doc_id, to_doc_id, rel_type)
        DO UPDATE SET
            confidence = GREATEST(wiki_links.confidence, EXCLUDED.confidence),
            evidence   = COALESCE(EXCLUDED.evidence, wiki_links.evidence)
        RETURNING (xmax = 0) AS inserted
        "#,
    )
    .bind(from_doc_id)
    .bind(to_doc_id)
    .bind(rel_type)
    .bind(confidence.clamp(0.0, 1.0))
    .bind(evidence)
    .fetch_optional(db)
    .await
    .context("UPSERT wiki_links")?;
    Ok(row.map(|(inserted,)| inserted).unwrap_or(false))
}

// ───────────────────────────────────────────────────────────────────────────
// Strategie
// ───────────────────────────────────────────────────────────────────────────

async fn process_wikilinks(
    db: &PgPool,
    doc: &DocRow,
    report: &mut RecomputeReport,
) -> Result<()> {
    let links = extract_wikilinks(&doc.body_md);
    if links.is_empty() {
        return Ok(());
    }
    let mut seen: HashSet<String> = HashSet::new();
    for raw in links {
        let key = raw.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        match resolve_wikilink_target(db, &doc.scope, doc.project_id, &raw).await {
            Ok(Some(target_id)) => {
                let evidence = format!("wikilink [[{}]]", normalize_target(&raw));
                match upsert_link(db, doc.id, target_id, "mentions", 0.95, &evidence).await {
                    Ok(_) => {
                        report.wikilinks_resolved += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            doc_id = %doc.id,
                            target = %raw,
                            error = %e,
                            "wiki.links: upsert wikilink fallito"
                        );
                        report
                            .errors
                            .push(format!("wikilink {}: {e}", normalize_target(&raw)));
                    }
                }
            }
            Ok(None) => {
                report.wikilinks_unresolved += 1;
                tracing::info!(
                    doc_id = %doc.id,
                    target = %raw,
                    scope = %doc.scope,
                    "wiki.links: wikilink unresolved"
                );
            }
            Err(e) => {
                report.errors.push(format!("resolve {}: {e}", raw));
            }
        }
    }
    Ok(())
}

async fn process_semantic(
    db: &PgPool,
    doc: &DocRow,
    settings: LinkWorkerSettings,
    report: &mut RecomputeReport,
) -> Result<()> {
    let Some(point_id) = doc.qdrant_point_id.as_deref() else {
        return Ok(());
    };
    let vector = match get_wiki_content_point_vector(db, point_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                doc_id = %doc.id,
                error = %e,
                "wiki.links: GET vector fallito, skip semantic per questo doc"
            );
            return Ok(());
        }
    };

    let hits = match search_wiki_content_points(
        db,
        vector,
        settings.semantic_top_k,
        settings.semantic_threshold as f64,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(
                doc_id = %doc.id,
                error = %e,
                "wiki.links: semantic search fallita, skip"
            );
            return Ok(());
        }
    };

    for hit in hits {
        // `hit.point_id` e' l'id del punto Qdrant, che NON coincide sempre con
        // `wiki_docs.id`: i worker run_summary/chat_note generano un UUID
        // dedicato per il punto e lo salvano in `wiki_docs.qdrant_point_id`
        // (solo il re-ingest del vault meta usa `point_id == doc.id`). Per
        // mappare il punto al documento autorevole risolviamo sempre via
        // `qdrant_point_id`; in fallback (dati storici allineati) accettiamo
        // anche il match diretto su `id`.
        // Self-skip: confronta con il point_id del doc sorgente, non con il suo id.
        if doc
            .qdrant_point_id
            .as_deref()
            .is_some_and(|p| p == hit.point_id)
        {
            continue;
        }
        // ACL semantica: il target deve essere visibile nello stesso scope del
        // sorgente OR essere meta public_read. Il payload Qdrant ha `scope` e
        // `project_id` ma per coerenza forte ricontrolliamo il DB (l'autorevole).
        let target_meta: Option<(Uuid, String, Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT id, scope, project_id, public_read FROM wiki_docs \
             WHERE qdrant_point_id = $1 OR id::text = $1 \
             ORDER BY (qdrant_point_id = $1) DESC LIMIT 1",
        )
        .bind(&hit.point_id)
        .fetch_optional(db)
        .await
        .context("SELECT wiki_docs target semantic")?;
        let Some((target_id, target_scope, target_pid, target_public)) = target_meta else {
            continue;
        };
        // Self-skip anche per id (caso point_id == doc.id sul re-ingest meta).
        if target_id == doc.id {
            continue;
        }

        let allowed = match (doc.scope.as_str(), target_scope.as_str()) {
            // Stesso scope + stesso project (o entrambi meta).
            (a, b) if a == b => doc.project_id == target_pid,
            // Project -> Meta solo se public_read.
            ("project", "meta") => target_public,
            // Meta -> Project consentito (admin-only nelle viste UI).
            ("meta", "project") => true,
            _ => false,
        };
        if !allowed {
            continue;
        }

        let evidence = format!("semantic cosine={:.4}", hit.score);
        match upsert_link(
            db,
            doc.id,
            target_id,
            "relates",
            hit.score as f32,
            &evidence,
        )
        .await
        {
            Ok(true) => report.semantic_links_created += 1,
            Ok(false) => report.semantic_links_updated += 1,
            Err(e) => {
                tracing::warn!(
                    doc_id = %doc.id,
                    target = %target_id,
                    error = %e,
                    "wiki.links: upsert semantic fallito"
                );
                report
                    .errors
                    .push(format!("semantic {target_id}: {e}"));
            }
        }
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// API pubbliche
// ───────────────────────────────────────────────────────────────────────────

/// Ricompila i link per UN solo documento. Esegue entrambe le strategie.
pub async fn recompute_links_for_doc(
    state: &AppState,
    doc_id: Uuid,
) -> Result<RecomputeReport> {
    let started = Instant::now();
    let mut report = RecomputeReport::default();
    let settings = current_settings(&state.db).await;

    if !settings.enabled {
        tracing::info!("wiki.links: worker disabilitato via settings, no-op");
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    let Some(doc) = fetch_doc(&state.db, doc_id).await? else {
        anyhow::bail!("documento {doc_id} non trovato");
    };
    report.docs_scanned = 1;
    if let Err(e) = process_wikilinks(&state.db, &doc, &mut report).await {
        report.errors.push(format!("wikilinks: {e}"));
    }
    if let Err(e) = process_semantic(&state.db, &doc, settings, &mut report).await {
        report.errors.push(format!("semantic: {e}"));
    }
    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

/// Ricompila i link per tutti i documenti in uno scope (+ filtro project_id).
pub async fn recompute_links_for_scope(
    state: &AppState,
    scope: Option<WikiScope>,
    project_id: Option<Uuid>,
) -> Result<RecomputeReport> {
    let started = Instant::now();
    let mut report = RecomputeReport::default();
    let settings = current_settings(&state.db).await;

    if !settings.enabled {
        tracing::info!("wiki.links: worker disabilitato via settings, no-op");
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    let docs = fetch_docs_for_scope(&state.db, scope, project_id).await?;
    report.docs_scanned = docs.len();
    tracing::info!(
        scope = ?scope.map(|s| s.as_str()),
        project_id = ?project_id,
        candidates = docs.len(),
        threshold = settings.semantic_threshold,
        top_k = settings.semantic_top_k,
        "wiki.links: recompute avvio"
    );

    for doc in docs {
        if let Err(e) = process_wikilinks(&state.db, &doc, &mut report).await {
            report
                .errors
                .push(format!("doc {} wikilinks: {e}", doc.id));
        }
        if let Err(e) = process_semantic(&state.db, &doc, settings, &mut report).await {
            report.errors.push(format!("doc {} semantic: {e}", doc.id));
        }
    }

    report.elapsed_ms = started.elapsed().as_millis();
    tracing::info!(
        scanned = report.docs_scanned,
        wikilinks_resolved = report.wikilinks_resolved,
        wikilinks_unresolved = report.wikilinks_unresolved,
        semantic_created = report.semantic_links_created,
        semantic_updated = report.semantic_links_updated,
        errors = report.errors.len(),
        elapsed_ms = report.elapsed_ms,
        "wiki.links: recompute completato"
    );
    Ok(report)
}

// ───────────────────────────────────────────────────────────────────────────
// Worker periodico (scope Meta + tutti i progetti)
// ───────────────────────────────────────────────────────────────────────────

/// Avvia il loop periodico di recompute link. Ogni ciclo processa lo scope
/// `Meta` e poi, uno alla volta, lo scope `Project` di ogni progetto
/// registrato (`SELECT id FROM projects`). Interval e enabled sono DB-driven
/// (settings `agent.wiki.link_worker_*`, cache 60s). Senza questo loop i
/// progetti restavano senza link finche' non triggerati a mano via REST.
pub fn start_links_worker(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        // Delay iniziale (120s): lascia tempo al bootstrap F4 (scope=Meta) e al
        // re-ingest F3 di completare prima del primo giro periodico.
        tokio::time::sleep(Duration::from_secs(120)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            "wiki.links: worker periodico avviato (meta + progetti)"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }

            // Scope Meta
            if let Err(e) =
                recompute_links_for_scope(&state, Some(WikiScope::Meta), None).await
            {
                tracing::warn!(error = %e, "wiki.links: recompute periodico meta fallito");
            }

            // Scope Project: un giro per ogni progetto registrato.
            match fetch_project_ids(&state.db).await {
                Ok(ids) => {
                    for pid in ids {
                        if let Err(e) = recompute_links_for_scope(
                            &state,
                            Some(WikiScope::Project),
                            Some(pid),
                        )
                        .await
                        {
                            tracing::warn!(
                                project_id = %pid,
                                error = %e,
                                "wiki.links: recompute periodico progetto fallito"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.links: SELECT projects fallita, skip giro progetti");
                }
            }

            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

/// Elenco id dei progetti registrati, per il giro periodico per-progetto.
async fn fetch_project_ids(db: &PgPool) -> Result<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM projects")
        .fetch_all(db)
        .await
        .context("SELECT id FROM projects (links worker)")?;
    Ok(ids)
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_defaults_match_migration() {
        let d = LinkWorkerSettings::safe_defaults();
        assert!((d.semantic_threshold - 0.60).abs() < 1e-6);
        assert_eq!(d.semantic_top_k, 10);
        assert!(d.enabled);
        assert_eq!(d.interval_secs, 1800);
    }

    #[test]
    fn normalize_target_strips_bang_and_spaces() {
        assert_eq!(normalize_target("  Foo  "), "Foo");
        assert_eq!(normalize_target("!embed-me"), "embed-me");
    }
}
