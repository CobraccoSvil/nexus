// ═══════════════════════════════════════════════════════════════════════════
// wiki/triple_extractor.rs — Fase 5 ADR 0017 v2: LLM-assisted triple extractor.
//
// Estrae triple semantiche (subj_doc, predicate, object) da documenti wiki
// invocando un modello LLM con output JSON strict. Le triple validate vengono
// salvate in `wiki_concept_triples` con `source='llm'`.
//
// Configurazione DB-driven (mig 0297 settings + mig 0298 prompt + 0297 purpose):
//   - settings.agent.wiki.triple_extract_enabled                (default true)
//   - settings.agent.wiki.triple_extract_interval_secs          (default 1800)
//   - settings.agent.wiki.triple_extract_cap_per_day_meta       (default 50)
//   - settings.agent.wiki.triple_extract_cap_per_day_project    (default 200)
//   - settings.agent.wiki.triple_extract_min_confidence         (default 0.55)
//   - settings.agent.wiki.triple_extract_max_triples_per_doc    (default 20)
//   - nexus_purpose_model['wiki_triple_extract'] -> (provider, model_id)
//   - nexus_prompt_templates['agent.wiki_triple_extract'] -> contenuto XML
//
// Niente fallback hardcoded sui modelli (regola G): se il purpose o la
// routing_matrix non sono disponibili l'estrazione fallisce in modo visibile
// (Result::Err propagato al chiamante).
// ═══════════════════════════════════════════════════════════════════════════

use nexus_types::get_template_or_default;
use crate::deps::WikiDeps;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Whitelist canonica dei predicate (deve coincidere con CHECK constraint
// `triple_predicate_check` di mig 0295).
// ───────────────────────────────────────────────────────────────────────────

const PREDICATE_WHITELIST: &[&str] = &[
    "relates",
    "supersedes",
    "depends_on",
    "illustrates",
    "contradicts",
    "followup",
    "correction_of",
    "refines",
    "duplicate_of",
    "blocks",
    "blocked_by",
    "mentions",
    "implements",
    "tests",
];

fn is_valid_predicate(p: &str) -> bool {
    PREDICATE_WHITELIST.contains(&p)
}

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern allineato a links_worker.rs)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct TripleExtractSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub cap_per_day_meta: u32,
    pub cap_per_day_project: u32,
    pub min_confidence: f32,
    pub max_triples_per_doc: u32,
}

impl TripleExtractSettings {
    const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            interval_secs: 1800,
            cap_per_day_meta: 50,
            cap_per_day_project: 200,
            min_confidence: 0.55,
            max_triples_per_doc: 20,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(TripleExtractSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> TripleExtractSettings {
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
                "wiki.triple_extract: lettura settings fallita, uso safe_defaults"
            );
            TripleExtractSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value, Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<TripleExtractSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.triple_extract_enabled', \
            'agent.wiki.triple_extract_interval_secs', \
            'agent.wiki.triple_extract_cap_per_day_meta', \
            'agent.wiki.triple_extract_cap_per_day_project', \
            'agent.wiki.triple_extract_min_confidence', \
            'agent.wiki.triple_extract_max_triples_per_doc' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.triple_extract_*")?;

    let mut out = TripleExtractSettings::safe_defaults();
    let mut seen: HashSet<&'static str> = HashSet::new();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        let trimmed = raw.trim();
        match key.as_str() {
            "agent.wiki.triple_extract_enabled" => {
                out.enabled = !matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                );
                seen.insert("enabled");
            }
            "agent.wiki.triple_extract_interval_secs" => {
                if let Ok(v) = trimmed.parse::<u64>() {
                    out.interval_secs = v.max(60);
                    seen.insert("interval");
                }
            }
            "agent.wiki.triple_extract_cap_per_day_meta" => {
                if let Ok(v) = trimmed.parse::<u32>() {
                    out.cap_per_day_meta = v;
                    seen.insert("cap_meta");
                }
            }
            "agent.wiki.triple_extract_cap_per_day_project" => {
                if let Ok(v) = trimmed.parse::<u32>() {
                    out.cap_per_day_project = v;
                    seen.insert("cap_project");
                }
            }
            "agent.wiki.triple_extract_min_confidence" => {
                if let Ok(v) = trimmed.parse::<f32>() {
                    out.min_confidence = v.clamp(0.0, 1.0);
                    seen.insert("min_conf");
                }
            }
            "agent.wiki.triple_extract_max_triples_per_doc" => {
                if let Ok(v) = trimmed.parse::<u32>() {
                    out.max_triples_per_doc = v.clamp(1, 100);
                    seen.insert("max_triples");
                }
            }
            _ => {}
        }
    }
    if seen.len() < 6 {
        tracing::info!(
            present = seen.len(),
            "wiki.triple_extract: alcuni settings assenti, applico safe_defaults"
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

#[derive(Debug, Default, Serialize, Clone)]
pub struct ExtractReport {
    pub doc_id: Option<Uuid>,
    pub triples_extracted: usize,
    pub triples_skipped_low_conf: usize,
    pub triples_skipped_invalid_predicate: usize,
    pub triples_unresolved_doc: usize,
    pub llm_tokens_input: i64,
    pub llm_tokens_output: i64,
    pub llm_cost_usd: f64,
    pub elapsed_ms: u128,
    pub errors: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct BatchReport {
    pub processed_count: usize,
    pub batch_results: Vec<ExtractReport>,
    pub daily_cap_remaining_after: i64,
}

// ───────────────────────────────────────────────────────────────────────────
// Tipi locali
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DocRow {
    id: Uuid,
    scope: String,
    project_id: Option<Uuid>,
    title: String,
    body_md: String,
}

async fn fetch_doc(db: &PgPool, doc_id: Uuid) -> Result<Option<DocRow>> {
    let row =
        sqlx::query("SELECT id, scope, project_id, title, body_md FROM wiki_docs WHERE id = $1")
            .bind(doc_id)
            .fetch_optional(db)
            .await
            .context("SELECT wiki_docs per triple_extractor")?;
    Ok(row.map(|r| DocRow {
        id: r.get("id"),
        scope: r.get("scope"),
        project_id: r.try_get("project_id").ok(),
        title: r.get("title"),
        body_md: r.get("body_md"),
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// Render prompt
// ───────────────────────────────────────────────────────────────────────────

async fn render_prompt(state: &WikiDeps, doc: &DocRow, max_triples: u32) -> Result<String> {
    // Template via cache 60s (`prompt_templates::get_template_or_default`).
    let tmpl = get_template_or_default(
        &state.db,
        &state.template_cache,
        "agent.wiki_triple_extract",
    )
    .await;
    if tmpl.trim().is_empty() {
        anyhow::bail!(
            "prompt template 'agent.wiki_triple_extract' mancante in nexus_prompt_templates \
             (applicare migrazione 0298)"
        );
    }
    // Sostituzione {{max_triples}} + append documento (titolo/body) come blocco utente.
    // I prompt template Nexus usano {{var}} per sostituzioni runtime.
    let system_part = tmpl.replace("{{max_triples}}", &max_triples.to_string());
    let user_block = format!(
        "\n\n=== DOCUMENTO DA ANALIZZARE ===\n# Titolo\n{title}\n\n# Body Markdown\n{body}\n=== FINE DOCUMENTO ===\n\nEmetti ora il JSON.",
        title = doc.title,
        body = doc.body_md,
    );
    Ok(format!("{system_part}{user_block}"))
}

// ───────────────────────────────────────────────────────────────────────────
// Parsing + validazione output LLM
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ParsedTriple {
    predicate: String,
    object_kind: String,
    object_doc_ref: Option<String>,
    object_concept: Option<String>,
    object_external: Option<String>,
    evidence: Option<String>,
    confidence: f32,
}

// Estrazione blocco JSON: punto unico in `crate::llm_json` (regola L / ADR 0026).
use nexus_types::llm_json::extract_json_object;

fn parse_triples_from_llm(content: &str) -> Result<Vec<ParsedTriple>> {
    let json_slice = extract_json_object(content)
        .ok_or_else(|| anyhow!("output LLM non contiene un oggetto JSON"))?;
    let parsed: Value = serde_json::from_str(json_slice).with_context(|| {
        format!(
            "JSON parse error sul payload LLM (len={})",
            json_slice.len()
        )
    })?;
    let arr = parsed
        .get("triples")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("manca campo 'triples' (array) nel JSON"))?;

    let mut out: Vec<ParsedTriple> = Vec::with_capacity(arr.len());
    for item in arr {
        let predicate = item
            .get("predicate")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let obj = item.get("object").cloned().unwrap_or(Value::Null);
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let doc_ref = obj
            .get("doc_slug_or_title")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let concept = obj
            .get("concept_label")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let external = obj
            .get("external_ref")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let evidence = item
            .get("evidence")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(200).collect::<String>())
            .filter(|s| !s.is_empty());
        let confidence = item
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;

        out.push(ParsedTriple {
            predicate,
            object_kind: kind,
            object_doc_ref: doc_ref,
            object_concept: concept,
            object_external: external,
            evidence,
            confidence: confidence.clamp(0.0, 1.0),
        });
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Risoluzione oggetto -> doc id (kind=doc)
// ───────────────────────────────────────────────────────────────────────────

/// Cerca un documento per slug/title in scope adattivo:
///   1. Stesso scope + stesso project_id
///   2. Se sorgente=project: meta con public_read=true
///   3. Se sorgente=meta: qualunque meta (gia' coperto dal punto 1) — niente altro
async fn resolve_target_doc(
    db: &PgPool,
    from_scope: &str,
    from_project_id: Option<Uuid>,
    needle: &str,
) -> Result<Option<Uuid>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let local: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM wiki_docs \
         WHERE scope = $1 \
           AND project_id IS NOT DISTINCT FROM $2 \
           AND (slug = $3 OR title ILIKE $3) \
         LIMIT 1",
    )
    .bind(from_scope)
    .bind(from_project_id)
    .bind(needle)
    .fetch_optional(db)
    .await
    .context("SELECT wiki_docs lookup target tripla locale")?;
    if local.is_some() {
        return Ok(local);
    }

    if from_scope == "project" {
        let cross: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM wiki_docs \
             WHERE scope = 'meta' AND public_read = TRUE \
               AND (slug = $1 OR title ILIKE $1) \
             LIMIT 1",
        )
        .bind(needle)
        .fetch_optional(db)
        .await
        .context("SELECT wiki_docs lookup target tripla cross-scope meta")?;
        return Ok(cross);
    }
    Ok(None)
}

// ───────────────────────────────────────────────────────────────────────────
// UPSERT idempotente in wiki_concept_triples
// ───────────────────────────────────────────────────────────────────────────

/// Inserisce una tripla. Se gia' esiste una riga con stessi (subj, predicate,
/// object) e source='llm', aggiorna confidence/evidence solo se nuovo confidence
/// > vecchio. Non esiste un UNIQUE constraint a DB (la PK e' uuid), quindi
/// > l'idempotenza e' implementata application-side con una SELECT preliminare.
async fn upsert_triple(
    db: &PgPool,
    subj_doc_id: Uuid,
    predicate: &str,
    obj_doc_id: Option<Uuid>,
    obj_text: Option<&str>,
    obj_external: Option<&str>,
    confidence: f32,
    evidence: Option<&str>,
) -> Result<bool> {
    // Match: stessa (subj, predicate, oggetto) e source='llm'.
    let existing: Option<(Uuid, f32)> = sqlx::query_as(
        "SELECT id, confidence FROM wiki_concept_triples \
         WHERE subj_doc_id = $1 \
           AND predicate = $2 \
           AND source = 'llm' \
           AND obj_doc_id IS NOT DISTINCT FROM $3 \
           AND obj_text IS NOT DISTINCT FROM $4 \
           AND obj_external IS NOT DISTINCT FROM $5 \
         LIMIT 1",
    )
    .bind(subj_doc_id)
    .bind(predicate)
    .bind(obj_doc_id)
    .bind(obj_text)
    .bind(obj_external)
    .fetch_optional(db)
    .await
    .context("SELECT wiki_concept_triples per dedup")?;

    if let Some((existing_id, existing_conf)) = existing {
        if confidence > existing_conf {
            sqlx::query(
                "UPDATE wiki_concept_triples \
                 SET confidence = $1, evidence = COALESCE($2, evidence) \
                 WHERE id = $3",
            )
            .bind(confidence)
            .bind(evidence)
            .bind(existing_id)
            .execute(db)
            .await
            .context("UPDATE wiki_concept_triples")?;
        }
        return Ok(false);
    }

    sqlx::query(
        "INSERT INTO wiki_concept_triples \
           (subj_doc_id, predicate, obj_doc_id, obj_text, obj_external, source, confidence, evidence) \
         VALUES ($1, $2, $3, $4, $5, 'llm', $6, $7)",
    )
    .bind(subj_doc_id)
    .bind(predicate)
    .bind(obj_doc_id)
    .bind(obj_text)
    .bind(obj_external)
    .bind(confidence)
    .bind(evidence)
    .execute(db)
    .await
    .context("INSERT wiki_concept_triples")?;
    Ok(true)
}

// ───────────────────────────────────────────────────────────────────────────
// API pubblica — estrazione singolo doc
// ───────────────────────────────────────────────────────────────────────────

pub async fn extract_triples_for_doc(state: &WikiDeps, doc_id: Uuid) -> Result<ExtractReport> {
    let started = Instant::now();
    let mut report = ExtractReport {
        doc_id: Some(doc_id),
        ..Default::default()
    };

    let settings = current_settings(&state.db).await;
    if !settings.enabled {
        report
            .errors
            .push("worker disabilitato via settings".to_string());
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    let Some(doc) = fetch_doc(&state.db, doc_id).await? else {
        anyhow::bail!("documento {doc_id} non trovato");
    };

    // Risolvi modello dal PUNTO UNICO tier-only (regola L/G).
    let (provider, model) = state
        .ai
        .resolve_purpose_model("wiki_triple_extract")
        .await
        .map_err(|m| anyhow!(m))?;

    let prompt = render_prompt(state, &doc, settings.max_triples_per_doc).await?;

    // NB: non logghiamo il body del documento ne' il prompt; solo metadati.
    tracing::info!(
        doc_id = %doc.id,
        scope = %doc.scope,
        provider = %provider,
        model = %model,
        body_len = doc.body_md.len(),
        "wiki.triple_extract: invio LLM"
    );

    let resp = match state
        .ai
        .generate_completion(&provider, &model, &prompt)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            report.errors.push(format!("llm error: {e}"));
            report.elapsed_ms = started.elapsed().as_millis();
            return Ok(report);
        }
    };

    // Estrai token usage / cost se presenti nel payload del provider.
    report.llm_tokens_input = resp
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .or_else(|| resp.get("input_tokens").and_then(|v| v.as_i64()))
        .or_else(|| {
            resp.get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    report.llm_tokens_output = resp
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .or_else(|| resp.get("output_tokens").and_then(|v| v.as_i64()))
        .or_else(|| {
            resp.get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    report.llm_cost_usd = resp
        .get("cost_usd")
        .and_then(|v| v.as_f64())
        .or_else(|| resp.get("total_cost").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);

    let content = resp
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() {
        report.errors.push("output LLM vuoto".to_string());
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    let parsed = match parse_triples_from_llm(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                doc_id = %doc.id,
                error = %e,
                content_len = content.len(),
                "wiki.triple_extract: parse JSON fallito"
            );
            report.errors.push(format!("parse: {e}"));
            report.elapsed_ms = started.elapsed().as_millis();
            return Ok(report);
        }
    };

    // Cap difensivo lato server: il prompt impone max_triples, ma se il modello
    // ne emette di piu' tronchiamo qui.
    let max_n = settings.max_triples_per_doc as usize;

    for pt in parsed.into_iter().take(max_n) {
        if !is_valid_predicate(&pt.predicate) {
            report.triples_skipped_invalid_predicate += 1;
            continue;
        }
        if pt.confidence < settings.min_confidence {
            report.triples_skipped_low_conf += 1;
            continue;
        }

        // Risoluzione oggetto in base al kind.
        let (obj_doc_id, obj_text, obj_external) = match pt.object_kind.as_str() {
            "doc" => {
                let Some(needle) = pt.object_doc_ref.as_deref() else {
                    report.triples_unresolved_doc += 1;
                    continue;
                };
                match resolve_target_doc(&state.db, &doc.scope, doc.project_id, needle).await {
                    Ok(Some(target_id)) => {
                        if target_id == doc.id {
                            // Self-reference scartato (vincolo logico, non DB).
                            continue;
                        }
                        (Some(target_id), None, None)
                    }
                    Ok(None) => {
                        report.triples_unresolved_doc += 1;
                        continue;
                    }
                    Err(e) => {
                        report.errors.push(format!("resolve doc '{needle}': {e}"));
                        continue;
                    }
                }
            }
            "concept" => {
                let Some(label) = pt.object_concept.as_deref() else {
                    continue;
                };
                (None, Some(label.to_string()), None)
            }
            "external" => {
                let Some(href) = pt.object_external.as_deref() else {
                    continue;
                };
                (None, None, Some(href.to_string()))
            }
            _ => {
                report
                    .errors
                    .push(format!("object.kind invalido: '{}'", pt.object_kind));
                continue;
            }
        };

        let inserted = upsert_triple(
            &state.db,
            doc.id,
            &pt.predicate,
            obj_doc_id,
            obj_text.as_deref(),
            obj_external.as_deref(),
            pt.confidence,
            pt.evidence.as_deref(),
        )
        .await;
        match inserted {
            Ok(true) => {
                report.triples_extracted += 1;
            }
            Ok(false) => {
                // Update di una tripla esistente: conteggia comunque come "estratta".
                report.triples_extracted += 1;
            }
            Err(e) => {
                tracing::warn!(
                    doc_id = %doc.id,
                    predicate = %pt.predicate,
                    error = %e,
                    "wiki.triple_extract: upsert fallito"
                );
                report.errors.push(format!("upsert: {e}"));
            }
        }
    }

    report.elapsed_ms = started.elapsed().as_millis();
    tracing::info!(
        doc_id = %doc.id,
        extracted = report.triples_extracted,
        skipped_low_conf = report.triples_skipped_low_conf,
        skipped_invalid_pred = report.triples_skipped_invalid_predicate,
        unresolved_doc = report.triples_unresolved_doc,
        tokens_in = report.llm_tokens_input,
        tokens_out = report.llm_tokens_output,
        elapsed_ms = report.elapsed_ms,
        "wiki.triple_extract: doc completato"
    );
    Ok(report)
}

// ───────────────────────────────────────────────────────────────────────────
// API pubblica — batch per scope
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum ExtractScope {
    Meta,
    Project(Uuid),
}

impl ExtractScope {
    fn cap_for(&self, s: TripleExtractSettings) -> u32 {
        match self {
            ExtractScope::Meta => s.cap_per_day_meta,
            ExtractScope::Project(_) => s.cap_per_day_project,
        }
    }
}

/// Quanti doc con almeno una tripla LLM sono stati creati nelle ultime 24h
/// dentro lo scope. Usato per il cap diurno.
async fn docs_processed_last_24h(db: &PgPool, scope: ExtractScope) -> Result<i64> {
    let count: i64 = match scope {
        ExtractScope::Meta => sqlx::query_scalar(
            "SELECT COUNT(DISTINCT t.subj_doc_id) \
             FROM wiki_concept_triples t \
             JOIN wiki_docs d ON d.id = t.subj_doc_id \
             WHERE t.source = 'llm' \
               AND t.created_at >= NOW() - INTERVAL '24 hours' \
               AND d.scope = 'meta'",
        )
        .fetch_one(db)
        .await
        .context("COUNT doc meta processati 24h")?,
        ExtractScope::Project(pid) => sqlx::query_scalar(
            "SELECT COUNT(DISTINCT t.subj_doc_id) \
             FROM wiki_concept_triples t \
             JOIN wiki_docs d ON d.id = t.subj_doc_id \
             WHERE t.source = 'llm' \
               AND t.created_at >= NOW() - INTERVAL '24 hours' \
               AND d.scope = 'project' \
               AND d.project_id = $1",
        )
        .bind(pid)
        .fetch_one(db)
        .await
        .context("COUNT doc project processati 24h")?,
    };
    Ok(count)
}

/// Seleziona i doc candidati: priorita' a quelli senza alcuna tripla LLM,
/// poi quelli con triple LLM piu' vecchie del `updated_at` del doc.
async fn fetch_candidates(db: &PgPool, scope: ExtractScope, limit: i64) -> Result<Vec<Uuid>> {
    let rows = match scope {
        ExtractScope::Meta => sqlx::query(
            "SELECT d.id FROM wiki_docs d \
             LEFT JOIN LATERAL ( \
                 SELECT MAX(created_at) AS last_llm FROM wiki_concept_triples \
                 WHERE subj_doc_id = d.id AND source = 'llm' \
             ) lt ON TRUE \
             WHERE d.scope = 'meta' \
               AND (lt.last_llm IS NULL OR lt.last_llm < d.updated_at) \
             ORDER BY (lt.last_llm IS NOT NULL), d.updated_at DESC \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(db)
        .await
        .context("SELECT candidati meta per triple_extract")?,
        ExtractScope::Project(pid) => sqlx::query(
            "SELECT d.id FROM wiki_docs d \
             LEFT JOIN LATERAL ( \
                 SELECT MAX(created_at) AS last_llm FROM wiki_concept_triples \
                 WHERE subj_doc_id = d.id AND source = 'llm' \
             ) lt ON TRUE \
             WHERE d.scope = 'project' AND d.project_id = $1 \
               AND (lt.last_llm IS NULL OR lt.last_llm < d.updated_at) \
             ORDER BY (lt.last_llm IS NOT NULL), d.updated_at DESC \
             LIMIT $2",
        )
        .bind(pid)
        .bind(limit)
        .fetch_all(db)
        .await
        .context("SELECT candidati project per triple_extract")?,
    };
    Ok(rows.into_iter().map(|r| r.get::<Uuid, _>("id")).collect())
}

/// Estrazione batch su uno scope, rispettando cap diurno. Sequenziale (no
/// parallelismo) per non sforare rate limit del provider.
pub async fn extract_triples_for_scope(
    state: &WikiDeps,
    scope: ExtractScope,
) -> Result<BatchReport> {
    let mut batch = BatchReport::default();
    let settings = current_settings(&state.db).await;
    if !settings.enabled {
        tracing::info!("wiki.triple_extract: disabilitato via settings, no-op");
        return Ok(batch);
    }

    let cap = scope.cap_for(settings) as i64;
    let processed = docs_processed_last_24h(&state.db, scope).await?;
    let remaining = (cap - processed).max(0);
    batch.daily_cap_remaining_after = remaining;
    if remaining == 0 {
        tracing::info!(
            scope = ?match scope {
                ExtractScope::Meta => "meta".to_string(),
                ExtractScope::Project(p) => format!("project:{p}"),
            },
            cap = cap,
            processed = processed,
            "wiki.triple_extract: cap diurno raggiunto, skip batch"
        );
        return Ok(batch);
    }

    let candidates = fetch_candidates(&state.db, scope, remaining).await?;
    if candidates.is_empty() {
        tracing::debug!("wiki.triple_extract: nessun candidato da processare");
        return Ok(batch);
    }

    tracing::info!(
        candidates = candidates.len(),
        remaining_before = remaining,
        cap = cap,
        "wiki.triple_extract: avvio batch"
    );

    for doc_id in candidates {
        match extract_triples_for_doc(state, doc_id).await {
            Ok(rep) => {
                batch.processed_count += 1;
                // Aggiorna il restante in modo conservativo: il doc e' stato
                // "consumato" indipendentemente da quante triple ha prodotto.
                batch.daily_cap_remaining_after = (batch.daily_cap_remaining_after - 1).max(0);
                batch.batch_results.push(rep);
            }
            Err(e) => {
                tracing::warn!(
                    doc_id = %doc_id,
                    error = %e,
                    "wiki.triple_extract: doc fallito, continuo batch"
                );
                let mut rep = ExtractReport {
                    doc_id: Some(doc_id),
                    ..Default::default()
                };
                rep.errors.push(format!("{e}"));
                batch.batch_results.push(rep);
            }
        }
        // Backoff fra una chiamata e l'altra per essere gentili col provider.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(batch)
}

// ───────────────────────────────────────────────────────────────────────────
// Worker periodico
// ───────────────────────────────────────────────────────────────────────────

pub fn start_triple_extractor_worker(state: WikiDeps) {
    tokio::spawn(async move {
        // Delay iniziale per non sovraccaricare il boot.
        tokio::time::sleep(Duration::from_secs(90)).await;

        // Log di avvio (interval letto al primo giro).
        let initial_settings = current_settings(&state.db).await;
        tracing::info!(
            interval_secs = initial_settings.interval_secs,
            enabled = initial_settings.enabled,
            "wiki.triple_extract: worker avviato"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }

            // 1) scope meta.
            if let Err(e) = extract_triples_for_scope(&state, ExtractScope::Meta).await {
                tracing::warn!(error = %e, "wiki.triple_extract: batch meta fallito");
            }

            // 2) ogni progetto registrato.
            let project_ids: Vec<Uuid> =
                match sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects")
                    .fetch_all(&state.db)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "wiki.triple_extract: SELECT projects fallita, skip batch project"
                        );
                        Vec::new()
                    }
                };
            for pid in project_ids {
                if let Err(e) = extract_triples_for_scope(&state, ExtractScope::Project(pid)).await
                {
                    tracing::warn!(
                        project_id = %pid,
                        error = %e,
                        "wiki.triple_extract: batch project fallito"
                    );
                }
            }

            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_defaults_match_migration_0297() {
        let d = TripleExtractSettings::safe_defaults();
        assert!(d.enabled);
        assert_eq!(d.interval_secs, 1800);
        assert_eq!(d.cap_per_day_meta, 50);
        assert_eq!(d.cap_per_day_project, 200);
        assert!((d.min_confidence - 0.55).abs() < 1e-6);
        assert_eq!(d.max_triples_per_doc, 20);
    }

    #[test]
    fn whitelist_predicate_e_quella_del_check_constraint() {
        assert!(is_valid_predicate("relates"));
        assert!(is_valid_predicate("supersedes"));
        assert!(is_valid_predicate("tests"));
        assert!(!is_valid_predicate("invented_predicate"));
        assert!(!is_valid_predicate(""));
        assert_eq!(PREDICATE_WHITELIST.len(), 14);
    }

    #[test]
    fn extract_json_object_tollera_preamboli() {
        let raw = "preambolo testo\n```json\n{\"triples\": []}\n```";
        let slice = extract_json_object(raw).expect("trova {");
        assert!(slice.starts_with('{'));
        assert!(slice.ends_with('}'));
    }

    #[test]
    fn parse_triples_filtra_campi_mancanti_senza_panicare() {
        let raw = r#"{
          "triples": [
            { "predicate": "relates", "object": {"kind": "concept", "concept_label": "RAG"}, "evidence": "...", "confidence": 0.8 },
            { "predicate": "depends_on", "object": {"kind": "doc", "doc_slug_or_title": "0015-rag"}, "confidence": 0.92 },
            { "predicate": "mentions", "object": {"kind": "external", "external_ref": "https://x"} }
          ]
        }"#;
        let parsed = parse_triples_from_llm(raw).expect("parse ok");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].predicate, "relates");
        assert_eq!(parsed[0].object_concept.as_deref(), Some("RAG"));
        assert_eq!(parsed[1].object_doc_ref.as_deref(), Some("0015-rag"));
        assert_eq!(parsed[2].object_external.as_deref(), Some("https://x"));
        assert!((parsed[2].confidence - 0.0).abs() < 1e-6);
    }
}
