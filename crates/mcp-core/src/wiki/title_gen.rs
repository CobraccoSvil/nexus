// ═══════════════════════════════════════════════════════════════════════════
// wiki/title_gen.rs — ADR 0017 v2: generazione titoli descrittivi LLM.
//
// I doc con titolo-artefatto (kind chat_note / run_summary / other) hanno un
// `wiki_docs.title` che e' un frammento del primo messaggio o un placeholder
// ("Run agent del ...", "chrome-error://...", "(indice):1 Unsafe attempt...").
// Nella KB risultano illeggibili. Questo modulo li sostituisce con un titolo
// conciso e descrittivo generato via LLM.
//
// Pattern allineato a `wiki::triple_extractor` (regola H, no duplicazione):
//   - settings DB-driven cache 60s
//   - purpose model via routing_matrix.purpose_model("wiki_title_gen")
//   - prompt via prompt_templates::get_template_or_default
//   - chiamata LLM via orchestrator.neural.generate_completion
//   - cap diurno per scope
//
// Configurazione DB-driven (mig 0306 settings/purpose + 0307 prompt):
//   - settings.agent.wiki.title_gen_enabled        (default true)
//   - settings.agent.wiki.title_gen_daily_cap      (default 100)
//   - settings.agent.wiki.title_gen_max_words       (default 10)
//   - nexus_purpose_model['wiki_title_gen'] -> (provider, model_id)
//   - nexus_prompt_templates['agent.wiki_title_gen'] -> contenuto XML
//
// Niente fallback hardcoded sui modelli (regola G): se purpose/routing_matrix
// non sono disponibili la generazione fallisce in modo visibile.
//
// Robustezza errori (regola F): se l'LLM fallisce o ritorna vuoto il titolo
// resta invariato e si logga WARN senza payload (no body/prompt nei log).
// ═══════════════════════════════════════════════════════════════════════════

use crate::prompt_templates::get_template_or_default;
use crate::AppState;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Kind considerati "artefatto": titolo non parlante, candidato a rigenerazione.
// I veri documenti redatti (adr, api, schema, changelog, architecture, runbook,
// concept, decision, note) NON vengono mai toccati.
// ───────────────────────────────────────────────────────────────────────────

const ARTIFACT_KINDS: &[&str] = &["chat_note", "run_summary", "other"];

fn is_artifact_kind(kind: &str) -> bool {
    ARTIFACT_KINDS.contains(&kind)
}

// Quanto body inviare al modello: sufficiente per capire l'argomento, basso
// costo. Allineato al troncamento usato altrove (reingest snippet 2000).
const BODY_SNIPPET_MAX: usize = 2000;

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern triple_extractor.rs)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct TitleGenSettings {
    pub enabled: bool,
    pub daily_cap: u32,
    pub max_words: u32,
    pub interval_secs: u64,
}

impl TitleGenSettings {
    const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            daily_cap: 100,
            max_words: 10,
            interval_secs: 1800,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(TitleGenSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> TitleGenSettings {
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
                "wiki.title_gen: lettura settings fallita, uso safe_defaults"
            );
            TitleGenSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value, Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<TitleGenSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.title_gen_enabled', \
            'agent.wiki.title_gen_daily_cap', \
            'agent.wiki.title_gen_max_words', \
            'agent.wiki.title_gen_interval_secs' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.title_gen_*")?;

    let mut out = TitleGenSettings::safe_defaults();
    let mut seen: HashSet<&'static str> = HashSet::new();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        let trimmed = raw.trim();
        match key.as_str() {
            "agent.wiki.title_gen_enabled" => {
                out.enabled = !matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                );
                seen.insert("enabled");
            }
            "agent.wiki.title_gen_daily_cap" => {
                if let Ok(v) = trimmed.parse::<u32>() {
                    out.daily_cap = v;
                    seen.insert("cap");
                }
            }
            "agent.wiki.title_gen_max_words" => {
                if let Ok(v) = trimmed.parse::<u32>() {
                    out.max_words = v.clamp(1, 30);
                    seen.insert("max_words");
                }
            }
            "agent.wiki.title_gen_interval_secs" => {
                if let Ok(v) = trimmed.parse::<u64>() {
                    out.interval_secs = v.max(60);
                    seen.insert("interval");
                }
            }
            _ => {}
        }
    }
    if seen.len() < 4 {
        tracing::info!(
            present = seen.len(),
            "wiki.title_gen: alcuni settings assenti, applico safe_defaults"
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
pub struct TitleGenReport {
    pub doc_id: Option<Uuid>,
    pub old_title: Option<String>,
    pub new_title: Option<String>,
    pub updated: bool,
    pub llm_tokens_input: i64,
    pub llm_tokens_output: i64,
    pub elapsed_ms: u128,
    pub errors: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct TitleGenBatchReport {
    pub processed_count: usize,
    pub updated_count: usize,
    pub batch_results: Vec<TitleGenReport>,
    pub daily_cap_remaining_after: i64,
}

// ───────────────────────────────────────────────────────────────────────────
// Tipi locali
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DocRow {
    id: Uuid,
    kind: String,
    title: String,
    body_md: String,
    manually_edited: bool,
}

async fn fetch_doc(db: &PgPool, doc_id: Uuid) -> Result<Option<DocRow>> {
    let row = sqlx::query(
        "SELECT id, kind, title, body_md, manually_edited \
         FROM wiki_docs WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(db)
    .await
    .context("SELECT wiki_docs per title_gen")?;
    Ok(row.map(|r| DocRow {
        id: r.get("id"),
        kind: r.get("kind"),
        title: r.get("title"),
        body_md: r.get("body_md"),
        manually_edited: r.try_get("manually_edited").unwrap_or(false),
    }))
}

// ───────────────────────────────────────────────────────────────────────────
// Render prompt
// ───────────────────────────────────────────────────────────────────────────

async fn render_prompt(state: &AppState, doc: &DocRow, max_words: u32) -> Result<String> {
    let tmpl =
        get_template_or_default(&state.db, &state.template_cache, "agent.wiki_title_gen").await;
    if tmpl.trim().is_empty() {
        anyhow::bail!(
            "prompt template 'agent.wiki_title_gen' mancante in nexus_prompt_templates \
             (applicare migrazione 0307)"
        );
    }
    let snippet = if doc.body_md.len() > BODY_SNIPPET_MAX {
        // Taglio su confine di char per evitare slice non-UTF8.
        doc.body_md
            .chars()
            .take(BODY_SNIPPET_MAX)
            .collect::<String>()
    } else {
        doc.body_md.clone()
    };
    let prompt = tmpl
        .replace("{{max_words}}", &max_words.to_string())
        .replace("{{current_title}}", &doc.title)
        .replace("{{body}}", &snippet);
    Ok(prompt)
}

// ───────────────────────────────────────────────────────────────────────────
// Sanitizzazione output LLM -> titolo pulito
// ───────────────────────────────────────────────────────────────────────────

/// Estrae un titolo pulito dall'output LLM: prima riga non vuota, rimozione di
/// virgolette/prefissi/punto finale, cap a `max_words` parole.
fn sanitize_title(raw: &str, max_words: u32) -> Option<String> {
    let line = raw
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())?
        .trim();

    // Rimuovi prefissi tipo "Titolo:", "Title:".
    let line = line
        .strip_prefix("Titolo:")
        .or_else(|| line.strip_prefix("Title:"))
        .or_else(|| line.strip_prefix("titolo:"))
        .unwrap_or(line)
        .trim();

    // Rimuovi virgolette e punteggiatura finale ridondante, iterando finche'
    // stabile: l'ordine puo' variare (es. `"...".` -> punto dopo virgoletta).
    let mut line = line.to_string();
    loop {
        let trimmed = line
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == '«' || c == '»' || c == '`')
            .trim_end_matches(['.', ';', ','])
            .trim()
            .to_string();
        if trimmed == line {
            break;
        }
        line = trimmed;
    }

    if line.is_empty() {
        return None;
    }

    // Cap a max_words parole.
    let words: Vec<&str> = line.split_whitespace().collect();
    let capped = if words.len() > max_words as usize {
        words[..max_words as usize].join(" ")
    } else {
        words.join(" ")
    };

    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// API pubblica — generazione singolo doc
// ───────────────────────────────────────────────────────────────────────────

/// Genera (e applica) un titolo descrittivo per un singolo doc.
///
/// Non rigenera se:
///   - settings disabilitati;
///   - il doc e' `manually_edited` (titolo curato a mano);
///   - il `kind` non e' un artefatto (doc redatto vero).
///
/// In caso di errore LLM o output vuoto il titolo resta invariato (regola F);
/// l'errore e' riportato nel `TitleGenReport.errors` senza payload nei log.
pub async fn generate_title_for_doc(state: &AppState, doc_id: Uuid) -> Result<TitleGenReport> {
    let started = Instant::now();
    let mut report = TitleGenReport {
        doc_id: Some(doc_id),
        ..Default::default()
    };

    let settings = current_settings(&state.db).await;
    if !settings.enabled {
        report
            .errors
            .push("title_gen disabilitato via settings".to_string());
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    let Some(doc) = fetch_doc(&state.db, doc_id).await? else {
        anyhow::bail!("documento {doc_id} non trovato");
    };
    report.old_title = Some(doc.title.clone());

    if doc.manually_edited {
        report
            .errors
            .push("doc manually_edited, titolo non rigenerato".to_string());
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }
    if !is_artifact_kind(&doc.kind) {
        report
            .errors
            .push(format!("kind '{}' non e' artefatto, skip", doc.kind));
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    // Risolvi modello dal PUNTO UNICO tier-only (regola L/G).
    let (provider, model) = crate::internal_routing::resolve_purpose_model(state, "wiki_title_gen")
        .await
        .into_model("wiki_title_gen")
        .map_err(|m| anyhow!(m))?;

    let prompt = render_prompt(state, &doc, settings.max_words).await?;

    // Niente body/prompt/title nei log (regola F): solo metadati.
    tracing::info!(
        doc_id = %doc.id,
        kind = %doc.kind,
        provider = %provider,
        model = %model,
        body_len = doc.body_md.len(),
        "wiki.title_gen: invio LLM"
    );

    let resp = match state
        .orchestrator
        .neural
        .generate_completion(&provider, &model, &prompt)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            // Errore LLM: titolo invariato (regola F).
            tracing::warn!(doc_id = %doc.id, error = %e, "wiki.title_gen: chiamata LLM fallita");
            report.errors.push(format!("llm error: {e}"));
            report.elapsed_ms = started.elapsed().as_millis();
            return Ok(report);
        }
    };

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

    let content = resp
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let Some(new_title) = sanitize_title(&content, settings.max_words) else {
        tracing::warn!(
            doc_id = %doc.id,
            content_len = content.len(),
            "wiki.title_gen: output LLM vuoto o non sanificabile, titolo invariato"
        );
        report
            .errors
            .push("output LLM vuoto/non valido".to_string());
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    };

    // No-op se il titolo coincide: aggiorna comunque title_generated_at per
    // far avanzare il cap e non riconsiderare il doc al giro successivo.
    if new_title == doc.title {
        let _ = sqlx::query("UPDATE wiki_docs SET title_generated_at = NOW() WHERE id = $1")
            .bind(doc.id)
            .execute(&state.db)
            .await;
        report.new_title = Some(new_title);
        report.updated = false;
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }

    // Applica il nuovo titolo. NON marchiamo manually_edited (e' una
    // rigenerazione automatica). Aggiorniamo updated_at + title_generated_at.
    sqlx::query(
        "UPDATE wiki_docs \
         SET title = $1, title_generated_at = NOW(), updated_at = NOW() \
         WHERE id = $2",
    )
    .bind(&new_title)
    .bind(doc.id)
    .execute(&state.db)
    .await
    .context("UPDATE wiki_docs title (title_gen)")?;

    report.new_title = Some(new_title.clone());
    report.updated = true;
    report.elapsed_ms = started.elapsed().as_millis();
    tracing::info!(
        doc_id = %doc.id,
        kind = %doc.kind,
        tokens_in = report.llm_tokens_input,
        tokens_out = report.llm_tokens_output,
        elapsed_ms = report.elapsed_ms,
        "wiki.title_gen: titolo rigenerato"
    );
    Ok(report)
}

// ───────────────────────────────────────────────────────────────────────────
// API pubblica — batch per scope
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum TitleScope {
    Meta,
    Project(Uuid),
}

/// Quanti doc hanno avuto il titolo (ri)generato nelle ultime 24h dentro lo
/// scope. Usato per il cap diurno.
async fn docs_processed_last_24h(db: &PgPool, scope: TitleScope) -> Result<i64> {
    let count: i64 = match scope {
        TitleScope::Meta => sqlx::query_scalar(
            "SELECT COUNT(*) FROM wiki_docs \
             WHERE scope = 'meta' \
               AND title_generated_at >= NOW() - INTERVAL '24 hours'",
        )
        .fetch_one(db)
        .await
        .context("COUNT title_gen meta 24h")?,
        TitleScope::Project(pid) => sqlx::query_scalar(
            "SELECT COUNT(*) FROM wiki_docs \
             WHERE scope = 'project' AND project_id = $1 \
               AND title_generated_at >= NOW() - INTERVAL '24 hours'",
        )
        .bind(pid)
        .fetch_one(db)
        .await
        .context("COUNT title_gen project 24h")?,
    };
    Ok(count)
}

/// Seleziona i doc candidati: artefatti, non editati a mano, mai rigenerati
/// (o rigenerati prima dell'ultimo `updated_at`). Priorita' ai mai rigenerati.
async fn fetch_candidates(db: &PgPool, scope: TitleScope, limit: i64) -> Result<Vec<Uuid>> {
    let kinds: Vec<String> = ARTIFACT_KINDS.iter().map(|s| s.to_string()).collect();
    let rows = match scope {
        TitleScope::Meta => sqlx::query(
            "SELECT id FROM wiki_docs \
             WHERE scope = 'meta' \
               AND kind = ANY($1) \
               AND manually_edited = FALSE \
               AND (title_generated_at IS NULL OR title_generated_at < updated_at) \
             ORDER BY (title_generated_at IS NOT NULL), updated_at DESC \
             LIMIT $2",
        )
        .bind(&kinds)
        .bind(limit)
        .fetch_all(db)
        .await
        .context("SELECT candidati meta per title_gen")?,
        TitleScope::Project(pid) => sqlx::query(
            "SELECT id FROM wiki_docs \
             WHERE scope = 'project' AND project_id = $1 \
               AND kind = ANY($2) \
               AND manually_edited = FALSE \
               AND (title_generated_at IS NULL OR title_generated_at < updated_at) \
             ORDER BY (title_generated_at IS NOT NULL), updated_at DESC \
             LIMIT $3",
        )
        .bind(pid)
        .bind(&kinds)
        .bind(limit)
        .fetch_all(db)
        .await
        .context("SELECT candidati project per title_gen")?,
    };
    Ok(rows.into_iter().map(|r| r.get::<Uuid, _>("id")).collect())
}

/// Generazione batch su uno scope, rispettando cap diurno. Sequenziale (no
/// parallelismo) per non sforare rate limit del provider.
pub async fn generate_titles_for_scope(
    state: &AppState,
    scope: TitleScope,
) -> Result<TitleGenBatchReport> {
    let mut batch = TitleGenBatchReport::default();
    let settings = current_settings(&state.db).await;
    if !settings.enabled {
        tracing::info!("wiki.title_gen: disabilitato via settings, no-op");
        return Ok(batch);
    }

    let cap = settings.daily_cap as i64;
    let processed = docs_processed_last_24h(&state.db, scope).await?;
    let remaining = (cap - processed).max(0);
    batch.daily_cap_remaining_after = remaining;
    if remaining == 0 {
        tracing::info!(
            scope = ?match scope {
                TitleScope::Meta => "meta".to_string(),
                TitleScope::Project(p) => format!("project:{p}"),
            },
            cap = cap,
            processed = processed,
            "wiki.title_gen: cap diurno raggiunto, skip batch"
        );
        return Ok(batch);
    }

    let candidates = fetch_candidates(&state.db, scope, remaining).await?;
    if candidates.is_empty() {
        tracing::debug!("wiki.title_gen: nessun candidato da processare");
        return Ok(batch);
    }

    tracing::info!(
        candidates = candidates.len(),
        remaining_before = remaining,
        cap = cap,
        "wiki.title_gen: avvio batch"
    );

    for doc_id in candidates {
        match generate_title_for_doc(state, doc_id).await {
            Ok(rep) => {
                batch.processed_count += 1;
                if rep.updated {
                    batch.updated_count += 1;
                }
                batch.daily_cap_remaining_after = (batch.daily_cap_remaining_after - 1).max(0);
                batch.batch_results.push(rep);
            }
            Err(e) => {
                tracing::warn!(
                    doc_id = %doc_id,
                    error = %e,
                    "wiki.title_gen: doc fallito, continuo batch"
                );
                let mut rep = TitleGenReport {
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
// Worker periodico (scope Meta + tutti i progetti)
// ───────────────────────────────────────────────────────────────────────────

/// Avvia il loop periodico di generazione titoli. Ogni ciclo processa lo
/// scope `Meta` e poi lo scope `Project` di ogni progetto registrato. Il cap
/// diurno e' applicato per-scope dentro `generate_titles_for_scope`, quindi il
/// loop non puo' spammare l'LLM oltre i cap configurati. Interval e enabled
/// sono DB-driven (`agent.wiki.title_gen_*`, cache 60s). Senza questo loop i
/// titoli dei progetti restavano da rigenerare finche' non triggerati a mano.
pub fn start_title_gen_worker(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        // Delay iniziale (150s): dopo links/run_summary per non concentrare il
        // carico LLM al boot.
        tokio::time::sleep(Duration::from_secs(150)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            daily_cap = init.daily_cap,
            "wiki.title_gen: worker periodico avviato (meta + progetti)"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }

            // Scope Meta
            if let Err(e) = generate_titles_for_scope(&state, TitleScope::Meta).await {
                tracing::warn!(error = %e, "wiki.title_gen: batch periodico meta fallito");
            }

            // Scope Project: un batch per ogni progetto registrato (cap diurno
            // applicato per-scope dentro generate_titles_for_scope).
            match fetch_project_ids(&state.db).await {
                Ok(ids) => {
                    for pid in ids {
                        if let Err(e) =
                            generate_titles_for_scope(&state, TitleScope::Project(pid)).await
                        {
                            tracing::warn!(
                                project_id = %pid,
                                error = %e,
                                "wiki.title_gen: batch periodico progetto fallito"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.title_gen: SELECT projects fallita, skip giro progetti");
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
        .context("SELECT id FROM projects (title_gen worker)")?;
    Ok(ids)
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_defaults_match_migration_0306() {
        let d = TitleGenSettings::safe_defaults();
        assert!(d.enabled);
        assert_eq!(d.daily_cap, 100);
        assert_eq!(d.max_words, 10);
    }

    #[test]
    fn artifact_kinds_solo_artefatti() {
        assert!(is_artifact_kind("chat_note"));
        assert!(is_artifact_kind("run_summary"));
        assert!(is_artifact_kind("other"));
        assert!(!is_artifact_kind("adr"));
        assert!(!is_artifact_kind("changelog"));
        assert!(!is_artifact_kind("architecture"));
        assert!(!is_artifact_kind(""));
    }

    #[test]
    fn sanitize_rimuove_virgolette_prefissi_e_punto() {
        let out = sanitize_title("Titolo: \"Configurazione database applicazione\".", 10);
        assert_eq!(out.as_deref(), Some("Configurazione database applicazione"));
    }

    #[test]
    fn sanitize_prende_prima_riga_non_vuota() {
        let out = sanitize_title("\n\nRiepilogo run di analisi progetto\nseconda riga", 10);
        assert_eq!(out.as_deref(), Some("Riepilogo run di analisi progetto"));
    }

    #[test]
    fn sanitize_cap_max_words() {
        let out = sanitize_title("uno due tre quattro cinque sei sette", 3);
        assert_eq!(out.as_deref(), Some("uno due tre"));
    }

    #[test]
    fn sanitize_vuoto_ritorna_none() {
        assert!(sanitize_title("", 10).is_none());
        assert!(sanitize_title("\n  \n", 10).is_none());
        assert!(sanitize_title("\"\"", 10).is_none());
    }
}
