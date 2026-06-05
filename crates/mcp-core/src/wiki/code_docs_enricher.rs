// ═══════════════════════════════════════════════════════════════════════════
// wiki/code_docs_enricher.rs — arricchimento LLM dei wiki_docs kind='code'.
//
// Causa radice risolta (mig 0331): `wiki::code_graph::ensure_code_doc` crea i
// wiki_docs kind='code' come PLACEHOLDER inerti (body fisso) al solo scopo di
// ancorare le triple `imports` del code-graph. Nessuno stadio li trasformava in
// schede descrittive ne' calcolava l'embedding -> la knowledge base non
// conteneva conoscenza utilizzabile sui file e l'agente rianalizzava i sorgenti.
//
// Questo worker e' lo stadio mancante: genera via LLM una scheda descrittiva per
// file (scopo, simboli esportati, dipendenze, note d'uso), la salva in body_md e
// ne calcola l'embedding in wiki_content. E' l'UNICO punto che chiama l'LLM per
// questo scopo (rate-limited via daily_cap), cosi' il costo e' governato.
//
// Modello come CATEGORIA configurabile da admin (regola G + dashboard routing):
// il purpose 'wiki_code_docs_enricher' usa il `tier` (mig 0203) come selezione
// primaria, risolto dal PUNTO UNICO `internal_routing::resolve_purpose_model`
// (regola L). provider/model_id statici sono solo fallback.
//
// Idempotenza: `wiki_docs.code_source_hash` registra l'hash del sorgente al
// momento dell'arricchimento. Il worker processa solo i doc con
// code_source_hash IS NULL (placeholder mai arricchiti + doc marcati stale dal
// reindex quando il file cambia, vedi `mark_code_doc_stale_if_changed`). Su
// hash invariato non viene sprecata alcuna chiamata LLM.
//
// Settings DB-driven (mig 0331 -> agent.wiki.code_docs_enricher_*), cache 60s.
// ═══════════════════════════════════════════════════════════════════════════

use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
use crate::AppState;
use anyhow::{Context, Result};
use serde_json::json;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

const PURPOSE: &str = "wiki_code_docs_enricher";

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern allineato a chat_note_worker)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CodeDocsEnricherSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub batch_max: usize,
    pub daily_cap: i64,
    pub max_source_chars: usize,
    pub min_source_chars: usize,
}

impl CodeDocsEnricherSettings {
    fn safe_defaults() -> Self {
        Self {
            enabled: true,
            interval_secs: 45,
            batch_max: 20,
            daily_cap: 500,
            max_source_chars: 12000,
            min_source_chars: 40,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(CodeDocsEnricherSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> CodeDocsEnricherSettings {
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
                "wiki.code_docs: lettura settings fallita, uso safe_defaults"
            );
            CodeDocsEnricherSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value.clone(), Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<CodeDocsEnricherSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.wiki.code_docs_enricher_enabled', \
            'agent.wiki.code_docs_enricher_interval_secs', \
            'agent.wiki.code_docs_enricher_batch_max', \
            'agent.wiki.code_docs_enricher_daily_cap', \
            'agent.wiki.code_docs_enricher_max_source_chars', \
            'agent.wiki.code_docs_enricher_min_source_chars' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.wiki.code_docs_enricher_*")?;

    let mut out = CodeDocsEnricherSettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        match key.as_str() {
            "agent.wiki.code_docs_enricher_enabled" => {
                out.enabled = matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
            "agent.wiki.code_docs_enricher_interval_secs" => {
                if let Ok(v) = raw.trim().parse::<u64>() {
                    out.interval_secs = v.max(5);
                }
            }
            "agent.wiki.code_docs_enricher_batch_max" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.batch_max = v.max(1);
                }
            }
            "agent.wiki.code_docs_enricher_daily_cap" => {
                if let Ok(v) = raw.trim().parse::<i64>() {
                    out.daily_cap = v.max(0);
                }
            }
            "agent.wiki.code_docs_enricher_max_source_chars" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.max_source_chars = v.max(200);
                }
            }
            "agent.wiki.code_docs_enricher_min_source_chars" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.min_source_chars = v;
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

/// Avvia il loop in background. Delay iniziale 75s: lascia completare boot e
/// prima indicizzazione code (che crea i placeholder) prima di arricchirli.
pub fn start_code_docs_enricher_worker(state: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(75)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            batch_max = init.batch_max,
            daily_cap = init.daily_cap,
            "wiki.code_docs: worker avviato"
        );

        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }
            match scan_and_enrich(&state, &settings).await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(enriched = n, "wiki.code_docs: batch completato");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.code_docs: batch fallito");
                }
            }
            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

/// Singolo batch: arricchisce fino a `batch_max` doc code non ancora arricchiti,
/// rispettando il cap diurno.
async fn scan_and_enrich(state: &AppState, settings: &CodeDocsEnricherSettings) -> Result<usize> {
    // Cap diurno: quanti arricchimenti nelle ultime 24h. Protegge il costo.
    let enriched_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE kind = 'code' AND code_docs_enriched_at > NOW() - INTERVAL '24 hours'",
    )
    .fetch_one(&state.db)
    .await
    .context("COUNT cap diurno code_docs")?;
    if settings.daily_cap > 0 && enriched_24h >= settings.daily_cap {
        tracing::debug!(
            enriched_24h,
            cap = settings.daily_cap,
            "wiki.code_docs: cap diurno raggiunto, skip batch"
        );
        return Ok(0);
    }

    let remaining_cap = if settings.daily_cap > 0 {
        (settings.daily_cap - enriched_24h).max(0) as usize
    } else {
        usize::MAX
    };
    let limit = settings.batch_max.min(remaining_cap).max(1) as i64;

    // Candidati: placeholder mai arricchiti o marcati stale (code_source_hash NULL).
    let rows = sqlx::query(
        "SELECT id, project_id, vault_file_path FROM wiki_docs \
         WHERE scope = 'project' AND kind = 'code' \
           AND code_source_hash IS NULL \
           AND manually_edited = FALSE \
           AND project_id IS NOT NULL \
           AND vault_file_path IS NOT NULL \
         ORDER BY updated_at ASC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .context("SELECT candidati code_docs da arricchire")?;

    if rows.is_empty() {
        return Ok(0);
    }

    // Cache root per progetto: evita una query per ogni doc dello stesso progetto.
    let mut root_cache: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut done = 0usize;

    for row in rows {
        let doc_id: Uuid = row.try_get("id").unwrap_or_default();
        let project_id: Uuid = row.try_get("project_id").unwrap_or_default();
        let relative_path: String = row.try_get("vault_file_path").unwrap_or_default();
        if relative_path.is_empty() {
            continue;
        }

        let root = match root_cache.entry(project_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let r = resolve_project_root(&state.db, project_id).await;
                e.insert(r.clone());
                r
            }
        };
        let Some(root) = root else {
            tracing::debug!(project_id = %project_id, "wiki.code_docs: root non risolto, skip doc");
            continue;
        };

        let abs = std::path::Path::new(&root).join(&relative_path);
        let content = match tokio::fs::read_to_string(&abs).await {
            Ok(c) => c,
            Err(e) => {
                // File rimosso/illeggibile: marca arricchito-con-hash-vuoto per non
                // riprovare in loop (verra' riportato stale se ricompare e cambia).
                tracing::debug!(
                    doc_id = %doc_id,
                    error = %e,
                    "wiki.code_docs: sorgente non leggibile, skip"
                );
                mark_unreadable(&state.db, doc_id).await;
                continue;
            }
        };

        match enrich_code_doc(state, settings, doc_id, project_id, &relative_path, &content).await {
            Ok(true) => done += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(doc_id = %doc_id, error = %e, "wiki.code_docs: arricchimento fallito");
                // Purpose non configurato -> inutile insistere su tutto il batch.
                if e.to_string().contains("purpose non configurato") {
                    break;
                }
            }
        }
    }

    Ok(done)
}

/// Risolve il root assoluto del progetto. Stessa fonte di verita' di
/// `projects::indexing` (repositories.root_path -> analysis_json -> repository_root_path).
async fn resolve_project_root(db: &PgPool, project_id: Uuid) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', p.repository_root_path, '') \
         FROM projects p \
         LEFT JOIN repositories r ON r.project_id = p.id \
         WHERE p.id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match row {
        Some((r,)) if !r.is_empty() => Some(r),
        _ => None,
    }
}

/// Genera la scheda descrittiva via LLM, calcola l'embedding e aggiorna il doc.
/// Ritorna Ok(true) se il doc e' stato arricchito, Ok(false) se saltato
/// (sorgente troppo breve o output LLM vuoto).
async fn enrich_code_doc(
    state: &AppState,
    settings: &CodeDocsEnricherSettings,
    doc_id: Uuid,
    project_id: Uuid,
    relative_path: &str,
    content: &str,
) -> Result<bool> {
    let source_hash = crate::wiki::vault::sha256_hex(content);

    // File banale: marca con l'hash corrente per non riprovare, senza LLM.
    if content.trim().chars().count() < settings.min_source_chars {
        finalize_skip(&state.db, doc_id, &source_hash).await;
        return Ok(false);
    }

    // Risolve provider+model dal PUNTO UNICO (regola L): tier dinamico -> statico.
    let (provider, model) = match resolve_purpose_model(state, PURPOSE).await {
        PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        PurposeResolution::InCooldown { .. } => {
            anyhow::bail!("provider in cooldown per purpose {PURPOSE}");
        }
        PurposeResolution::NotFound => {
            anyhow::bail!(
                "purpose non configurato: {PURPOSE} (applicare migrazione 0331)"
            );
        }
        PurposeResolution::MatrixUnavailable(e) => {
            anyhow::bail!("routing_matrix non disponibile: {e}");
        }
    };

    let snippet: String = content.chars().take(settings.max_source_chars).collect();
    let prompt = build_prompt(relative_path, &snippet);

    // Niente sorgente/prompt nei log (regola F): solo metadati.
    tracing::info!(
        doc_id = %doc_id,
        provider = %provider,
        model = %model,
        source_len = content.len(),
        "wiki.code_docs: invio LLM"
    );

    let resp = state
        .orchestrator
        .neural
        .generate_completion(&provider, &model, &prompt)
        .await
        .context("generate_completion code_docs")?;

    let raw_description = resp
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // Strip difensivo: alcuni modelli incapsulano l'intera risposta in un fence
    // ```markdown ... ```. Non ci affidiamo al solo prompt (regola H).
    let description = strip_outer_code_fence(&raw_description);

    if description.is_empty() {
        // Output vuoto: non marchiamo come arricchito, riproveremo al prossimo giro.
        anyhow::bail!("output LLM vuoto");
    }

    let title = relative_path.to_string();
    let body_md = format!(
        "# {relative_path}\n\n{description}\n\n---\n\n_Scheda generata automaticamente \
         dalla knowledge base Nexus (wiki code-docs enricher). Modifica manuale = \
         blocca la rigenerazione._\n"
    );
    let body_hash = crate::wiki::vault::sha256_hex(&body_md);

    // Embedding + upsert Qdrant (best-effort). Contratto: wiki_docs.id == point_id.
    let combined = {
        let snip: String = description.chars().take(2000).collect();
        format!("{title}\n\n{snip}")
    };
    let qdrant_point_id: Option<String> =
        match state.orchestrator.neural.embed_text("", &combined).await {
            Ok(vector) => {
                let point_id = doc_id.to_string();
                let payload = json!({
                    "scope": "project",
                    "project_id": project_id.to_string(),
                    "doc_id": point_id,
                    "title": title,
                    "kind": "code",
                    "updated_at": chrono::Utc::now().to_rfc3339(),
                });
                match crate::vector_memory::upsert_wiki_content_point(
                    &state.db, &point_id, vector, payload,
                )
                .await
                {
                    Ok(_) => Some(point_id),
                    Err(e) => {
                        tracing::debug!(doc_id = %doc_id, error = %e, "wiki.code_docs: upsert Qdrant fallito (proseguo)");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::debug!(doc_id = %doc_id, error = %e, "wiki.code_docs: embed_text fallito (proseguo)");
                None
            }
        };

    // Aggiorna il doc. Rispetta manually_edited (non sovrascrive doc editati a mano).
    sqlx::query(
        "UPDATE wiki_docs SET \
            body_md = $2, body_hash = $3, generated_hash = $3, \
            code_source_hash = $4, code_docs_enriched_at = NOW(), \
            qdrant_point_id = COALESCE($5, qdrant_point_id), \
            updated_at = NOW() \
         WHERE id = $1 AND manually_edited = FALSE",
    )
    .bind(doc_id)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(&source_hash)
    .bind(qdrant_point_id.as_deref())
    .execute(&state.db)
    .await
    .context("UPDATE wiki_docs arricchito")?;

    Ok(true)
}

/// Costruisce il prompt di descrizione del file. Output atteso: scheda Markdown
/// concisa. Il prompt e' un call site FUORI chat (worker), quindi e' autoritativo
/// e self-contained (CLAUDE.md sez. D).
fn build_prompt(relative_path: &str, source: &str) -> String {
    format!(
        "Sei un assistente che documenta codice sorgente per una knowledge base. \
Analizza il file seguente e produci una scheda descrittiva CONCISA in italiano, \
in Markdown, con queste sezioni (ometti quelle non pertinenti):\n\n\
- **Scopo**: una o due frasi su cosa fa il file e il suo ruolo nel progetto.\n\
- **Esporta**: principali simboli pubblici (funzioni, classi, componenti, tipi) con una riga ciascuno.\n\
- **Dipendenze chiave**: moduli/pacchetti importanti da cui dipende.\n\
- **Note**: dettagli utili a chi deve modificarlo (side effect, invarianti, gotcha).\n\n\
Regole: massimo ~200 parole, niente codice ripetuto integralmente, niente preamboli \
tipo 'Ecco la scheda'. NON racchiudere la risposta in un blocco di codice (niente \
```markdown o ``` attorno alla scheda). Rispondi SOLO con la scheda in Markdown.\n\n\
File: `{relative_path}`\n\n\
```\n{source}\n```\n"
    )
}

/// Rimuove un eventuale blocco di codice che racchiude l'INTERA risposta
/// (```...``` o ```markdown...```). Se il testo non e' interamente avvolto in un
/// singolo fence, lo restituisce invariato (non tocca fence interni legittimi).
fn strip_outer_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") || !trimmed.ends_with("```") {
        return trimmed.to_string();
    }
    // Deve esserci esattamente la coppia di fence esterni (apertura + chiusura):
    // se ci sono piu' di 2 occorrenze di ``` il testo ha fence interni e lo
    // lasciamo intatto per non corromperlo.
    if trimmed.matches("```").count() != 2 {
        return trimmed.to_string();
    }
    let after_open = match trimmed.find('\n') {
        Some(idx) => &trimmed[idx + 1..],
        None => return trimmed.to_string(),
    };
    let body = after_open.strip_suffix("```").unwrap_or(after_open);
    body.trim().to_string()
}

/// Marca un doc come 'processato senza arricchimento' fissando l'hash corrente,
/// cosi' il worker non lo ripesca finche' il sorgente non cambia.
async fn finalize_skip(db: &PgPool, doc_id: Uuid, source_hash: &str) {
    let _ = sqlx::query(
        "UPDATE wiki_docs SET code_source_hash = $2, code_docs_enriched_at = NOW(), updated_at = NOW() \
         WHERE id = $1 AND manually_edited = FALSE",
    )
    .bind(doc_id)
    .bind(source_hash)
    .execute(db)
    .await;
}

/// File sorgente non leggibile: fissa un hash sentinella per evitare retry in loop.
async fn mark_unreadable(db: &PgPool, doc_id: Uuid) {
    let _ = sqlx::query(
        "UPDATE wiki_docs SET code_source_hash = '__unreadable__', updated_at = NOW() \
         WHERE id = $1 AND manually_edited = FALSE",
    )
    .bind(doc_id)
    .execute(db)
    .await;
}

/// Marca stale il doc code di un file quando il suo sorgente cambia: azzera
/// `code_source_hash` SOLO se il nuovo hash differisce da quello registrato,
/// cosi' il worker lo ripesca e rigenera la scheda. Chiamato dal reindex
/// (best-effort, non blocca). PUNTO UNICO del segnale di staleness.
pub async fn mark_code_doc_stale_if_changed(
    db: &PgPool,
    project_id: Uuid,
    relative_path: &str,
    content: &str,
) {
    let new_hash = crate::wiki::vault::sha256_hex(content);
    let _ = sqlx::query(
        "UPDATE wiki_docs SET code_source_hash = NULL, updated_at = NOW() \
         WHERE scope = 'project' AND kind = 'code' AND project_id = $1 \
           AND vault_file_path = $2 AND manually_edited = FALSE \
           AND code_source_hash IS DISTINCT FROM $3",
    )
    .bind(project_id)
    .bind(relative_path)
    .bind(&new_hash)
    .execute(db)
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_include_path_e_sorgente() {
        let p = build_prompt("src/foo/bar.rs", "fn main() {}");
        assert!(p.contains("src/foo/bar.rs"), "il path deve comparire nel prompt");
        assert!(p.contains("fn main() {}"), "il sorgente deve comparire nel prompt");
        // Deve chiedere output in italiano e solo la scheda (call site fuori chat).
        assert!(p.contains("italiano"));
        assert!(p.contains("**Scopo**"));
    }

    #[test]
    fn strip_fence_rimuove_wrapper_markdown() {
        let s = "```markdown\n**Scopo**: x\n\n**Note**: y\n```";
        assert_eq!(strip_outer_code_fence(s), "**Scopo**: x\n\n**Note**: y");
        let s2 = "```\nciao\n```";
        assert_eq!(strip_outer_code_fence(s2), "ciao");
    }

    #[test]
    fn strip_fence_preserva_testo_normale_e_fence_interni() {
        let plain = "**Scopo**: descrizione senza fence";
        assert_eq!(strip_outer_code_fence(plain), plain);
        // Fence interno legittimo (snippet): non deve essere rimosso.
        let inner = "**Esporta**:\n```rust\nfn x() {}\n```\nfine";
        assert_eq!(strip_outer_code_fence(inner), inner);
    }

    #[test]
    fn safe_defaults_sono_coerenti() {
        let d = CodeDocsEnricherSettings::safe_defaults();
        assert!(d.enabled);
        assert!(d.interval_secs >= 5);
        assert!(d.batch_max >= 1);
        assert!(d.max_source_chars > d.min_source_chars);
    }
}
