//! Tool agente di analisi qualita codice: scan singolo/progetto e batch.
//!
//! Estratto da mod.rs (refactor god-file).

use serde_json::Value;
use sqlx::Row;

use super::gateway_client::{
    gateway_batch_status, gateway_batch_submit, GwBatchRequest,
};
use super::ToolContextCore;
use nexus_types::routing_client::resolve_purpose_via_http;

/// Purpose del routing per il batch-analyze (regola G: modello dal DB, non
/// hardcoded). Tier-only in `nexus_purpose_model` (mig 0102/0136).
const BATCH_PURPOSE: &str = "anthropic_batch";

/// `max_tokens` di generazione per ogni richiesta del batch. Non e' un nome di
/// modello (regola G): e' il tetto di output, allineato al default del gateway.
const BATCH_MAX_TOKENS: u32 = 4096;

pub async fn tool_scan_code_quality(ctx: &ToolContextCore, input: &Value) -> String {
    let file_path = input.get("file_path").and_then(Value::as_str);
    let severity_filter = input
        .get("severity_filter")
        .and_then(Value::as_str)
        .unwrap_or("all");

    if let Some(rel_path) = file_path {
        // Single file analysis. Punto unico (regola L): de-duplica la root e blocca "..".
        let full_path = match nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, rel_path) {
            Ok(clean) => ctx.root_path.join(&clean),
            Err(e) => return format!("Errore risoluzione path: {}", e.message()),
        };
        let content = match tokio::fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return format!("Errore lettura file: {}", e),
        };

        if rel_path.ends_with(".sql") {
            let db_report = mcp_db::analyze_query(&content);
            let findings: Vec<String> = db_report
                .findings
                .iter()
                .map(|f| {
                    format!(
                        "[{}][{}] {} -- {}",
                        f.severity.to_uppercase(),
                        f.category,
                        f.title,
                        f.detail
                    )
                })
                .collect();
            if findings.is_empty() {
                return format!("Nessun problema trovato in `{}`", rel_path);
            }
            return format!("Analisi SQL `{}`:\n{}", rel_path, findings.join("\n"));
        }

        let report = mcp_quality::analyze_source(rel_path, &content);

        let filtered: Vec<_> = report
            .findings
            .iter()
            .filter(|f| match severity_filter {
                "high" => f.severity == "high",
                "medium" => f.severity == "high" || f.severity == "medium",
                _ => true,
            })
            .collect();

        if filtered.is_empty() {
            return format!(
                "Nessun problema trovato in `{}` (filtro: {})",
                rel_path, severity_filter
            );
        }

        let lines: Vec<String> = filtered
            .iter()
            .map(|f| {
                let loc = f.line.map(|l| format!(":{}", l)).unwrap_or_default();
                format!(
                    "[{}][{}] {}{} -- {}",
                    f.severity.to_uppercase(),
                    f.category,
                    rel_path,
                    loc,
                    f.title
                )
            })
            .collect();

        format!("Analisi `{}`:\n{}\n\nMetriche: {} righe totali, complessità max: {}, lunghezza media funzioni: {:.0}",
            rel_path, lines.join("\n"),
            report.metrics.total_lines, report.metrics.max_complexity, report.metrics.avg_function_length)
    } else {
        // Full project scan: read from DB if available
        let rows = sqlx::query(
            "SELECT file_path, category, severity, title, line_number \
             FROM project_quality_findings WHERE project_id = $1 AND fixed_at IS NULL \
             ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END \
             LIMIT 30",
        )
        .bind(ctx.project_id)
        .fetch_all(&*ctx.db)
        .await;

        match rows {
            Ok(rows) if !rows.is_empty() => {
                let lines: Vec<String> = rows.iter().map(|r| {
                    let fp: String = r.try_get("file_path").unwrap_or_default();
                    let cat: String = r.try_get("category").unwrap_or_default();
                    let sev: String = r.try_get("severity").unwrap_or_default();
                    let title: String = r.try_get("title").unwrap_or_default();
                    let line: Option<i32> = r.try_get("line_number").ok().flatten();
                    let loc = line.map(|l| format!(":{}", l)).unwrap_or_default();
                    format!("[{}][{}] {}{} -- {}", sev.to_uppercase(), cat, fp, loc, title)
                }).collect();
                format!("Top findings del progetto (da ultimo scan):\n{}\n\nUsa scan_code_quality(file_path) per analizzare un file specifico.", lines.join("\n"))
            }
            _ => {
                "Nessun dato di qualità disponibile. Esegui prima una scansione completa dal pannello Ottimizzazione, oppure specifica un file_path per analizzare un file singolo.".to_string()
            }
        }
    }
}

/// Ruolo di sistema per batch_analyze_code dal DB (mig 0445) con fallback
/// hardcoded. Query diretta: questo crate e' a monte di mcp-core e non puo'
/// usare get_template_or_default.
async fn batch_role_prompt(db: &sqlx::PgPool, task: &str) -> String {
    let (key, fallback) = match task {
        "document" => (
            "system.batch_document_role",
            "Sei un esperto di documentazione tecnica. Analizza il codice e genera commenti/docstring chiari e concisi in italiano. Concentrati sul WHY, non sul WHAT.",
        ),
        "optimize" => (
            "system.batch_optimize_role",
            "Sei un esperto di ottimizzazione del codice. Identifica problemi di performance, complessità eccessiva, codice duplicato e suggerisci refactoring concreti.",
        ),
        _ => (
            "system.batch_review_role",
            "Sei un esperto di revisione del codice. Identifica bug potenziali, problemi di sicurezza, violazioni di pattern architetturali e punti di miglioramento.",
        ),
    };
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
    )
    .bind(key)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .unwrap_or_else(|| fallback.to_string())
}

pub async fn tool_batch_analyze_code(ctx: &ToolContextCore, input: &Value) -> String {
    let task = input
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("analyze");
    let files_arr = match input.get("files").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => return nexus_types::tool_outcome::tool_failure("[batch_analyze_code] Campo 'files' mancante o non è un array"),
    };
    if files_arr.is_empty() {
        return nexus_types::tool_outcome::tool_failure("[batch_analyze_code] Nessun file specificato");
    }
    if files_arr.len() > 20 {
        return nexus_types::tool_outcome::tool_failure("[batch_analyze_code] Massimo 20 file per batch");
    }

    let system_prompt = batch_role_prompt(&ctx.db, task).await;

    // Leggi il contenuto dei file non forniti
    let mut requests: Vec<GwBatchRequest> = Vec::new();
    for (i, file_obj) in files_arr.iter().enumerate() {
        let path_str = match file_obj.get("path").and_then(Value::as_str) {
            Some(p) => p.to_string(),
            None => continue,
        };
        let content = if let Some(c) = file_obj
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            c.to_string()
        } else {
            // Leggi il file dalla root del progetto. Punto unico (regola L):
            // de-duplica la root e blocca "..".
            let abs_path = match nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, &path_str) {
                Ok(clean) => ctx.root_path.join(&clean),
                Err(e) => {
                    tracing::warn!("batch_analyze_code: path '{}' non valido: {}", path_str, e.message());
                    continue;
                }
            };
            match tokio::fs::read_to_string(&abs_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        "batch_analyze_code: impossibile leggere {}: {}",
                        path_str,
                        e
                    );
                    format!("\u{274C} [Errore lettura file: {e}]")
                }
            }
        };
        requests.push(GwBatchRequest {
            custom_id: format!("file-{i}"),
            system: Some(system_prompt.clone()),
            prompt: format!(
                "File: {}\n\n```\n{}\n```\n\nEsegui il task '{}' su questo file.",
                path_str,
                &content[..content.len().min(32000)],
                task
            ),
        });
    }

    if requests.is_empty() {
        return nexus_types::tool_outcome::tool_failure("[batch_analyze_code] Nessun file valido trovato");
    }

    // Provider/modello dal purpose (regola G: niente modello hardcoded). Il batch
    // del gateway oggi supporta solo Anthropic: se il purpose risolve un altro
    // provider, il gateway risponde 400/501 e l'errore risale onestamente al
    // modello (niente fallback inventato, regola H).
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, BATCH_PURPOSE).await {
        Ok(pm) => pm,
        Err(e) => {
            return nexus_types::tool_outcome::tool_failure(format!(
                "[batch_analyze_code] modello batch non risolvibile (purpose '{BATCH_PURPOSE}'): {e}. \
                 Verifica nexus_purpose_model.{BATCH_PURPOSE} (mig 0102/0136)."
            ));
        }
    };

    // Sottomette il batch al gateway Rust (POST /v1/batch).
    let batch_id =
        match gateway_batch_submit(&ctx.db, &provider, &model, &requests, BATCH_MAX_TOKENS).await {
            Ok(id) => id,
            Err(e) => {
                return nexus_types::tool_outcome::tool_failure(format!(
                    "[batch_analyze_code] Errore sottomissione batch: {e}"
                ))
            }
        };

    // Poll con backoff esponenziale (max 10 minuti) su GET /v1/batch/{provider}/{id}.
    let mut wait_secs = 2u64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    let results = loop {
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        wait_secs = (wait_secs * 2).min(60);

        let snapshot = match gateway_batch_status(&ctx.db, &provider, &batch_id).await {
            Ok(s) => s,
            Err(e) => {
                return nexus_types::tool_outcome::tool_failure(format!(
                    "[batch_analyze_code] Errore polling status: {e}"
                ))
            }
        };
        if snapshot.is_ended() {
            break snapshot.results;
        }
        if tokio::time::Instant::now() >= deadline {
            return nexus_types::tool_outcome::tool_failure(format!(
                "[batch_analyze_code] Timeout: il batch {batch_id} non ha terminato in 10 minuti"
            ));
        }
    };

    // Formatta output (custom_id -> file, preservando la forma storica).
    let mut output_parts: Vec<String> = Vec::new();
    for (i, file_obj) in files_arr.iter().enumerate() {
        let path_str = file_obj.get("path").and_then(Value::as_str).unwrap_or("?");
        let custom_id = format!("file-{i}");
        if let Some(result) = results.iter().find(|r| r.custom_id == custom_id) {
            if let Some(err) = &result.error {
                output_parts.push(format!("### {path_str}\n\n[Errore: {err}]"));
            } else if !result.content.is_empty() {
                output_parts.push(format!("### {path_str}\n\n{}", result.content));
            }
        }
    }

    if output_parts.is_empty() {
        nexus_types::tool_outcome::tool_failure(format!(
            "[batch_analyze_code] Nessun risultato per il batch {batch_id}"
        ))
    } else {
        format!(
            "## Analisi batch ({task}) — {} file\n\n{}",
            output_parts.len(),
            output_parts.join("\n\n---\n\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::is_tool_failure;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    /// Contesto reale (la struct di produzione), pool lazy mai contattato: i
    /// rami qui esercitati rifiutano l'input PRIMA di toccare il DB. Stessa
    /// forma di `attachments::tests::ctx_di_prova` (il crate non ha un helper
    /// condiviso per i test; quando nascera', questi due convergono li').
    fn ctx_di_prova() -> ToolContextCore {
        use crate::context_core::{NoopEmbedder, NoopMutationHooks};
        let db =
            sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
        ToolContextCore {
            root_path: std::env::temp_dir(),
            user_id: Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks: Arc::new(NoopMutationHooks),
            embedder: Arc::new(NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        }
    }

    /// Un input rifiutato e' un FALLIMENTO, e deve dichiararlo nel canale che
    /// il dispatch legge (il marker del ponte legacy, finche' la firma non
    /// migra a RispostaTool). Fino al 02/08/2026 questi rami tornavano prosa
    /// nuda: il modello riceveva "Campo 'files' mancante" come un successo, e
    /// l'anti-loop contava la ripetizione come produttiva.
    ///
    /// MUTAZIONE: rimettere `String::from(...)` nudo su uno dei rami fa
    /// rosseggiare l'asserzione corrispondente — il valore del difetto reale.
    #[tokio::test]
    async fn l_input_rifiutato_dichiara_il_fallimento() {
        let ctx = ctx_di_prova();
        let casi = [
            json!({}),                                        // files mancante
            json!({"files": []}),                             // vuoto
            json!({"files": (0..21).map(|i| json!({"path": format!("f{i}.rs")})).collect::<Vec<_>>()}), // oltre il cap
        ];
        for input in casi {
            let out = tool_batch_analyze_code(&ctx, &input).await;
            assert!(
                is_tool_failure(&out),
                "il rifiuto deve dichiararsi fallimento nel canale del dispatch: {out}"
            );
        }
    }
}
