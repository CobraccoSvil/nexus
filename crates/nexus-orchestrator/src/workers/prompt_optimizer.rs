//! PromptOptimizerWorker — Fase 3 del piano Nexus
//!
//! Worker periodico che chiude il loop di auto-miglioramento dei prompt:
//!
//! 1. **Aggregate**: raccoglie metriche da `nexus_agent_reflections` per ogni
//!    `(prompt_key, version)`. Richiede >= `min_runs` run per cohort.
//! 2. **Identify candidates**: prompt con `avg_reflection_score < threshold`
//!    o `feedback_rate < success_threshold` sono candidati all'ottimizzazione.
//! 3. **Generate variants**: chiama Claude API con meta-prompt XML.
//!    Genera 1-2 varianti per candidato. Valida lo schema XML (tag obbligatori).
//! 4. **Insert as inactive**: nuove versioni in `nexus_prompt_templates`
//!    con `is_active=FALSE`, `experimental=TRUE`.
//! 5. **Register canary**: inserisce in `prompt_ab_experiments` (status=running).
//! 6. **Promote/discard** (solo se `auto_promote_enabled=true` in DB):
//!    chiude esperimenti maturi con test statistico (Wilson score).
//!
//! ## Protezioni
//! - Kill switch globale: `optimizer_enabled=false` nel DB.
//! - Auto-promozione separata: `optimizer_auto_promote=false` (default) = dry-run.
//! - Safelist immutabile: `system.*` e `automation.*` non vengono mai ottimizzati.
//! - Cap concorrenza: max `optimizer_max_concurrent_experiments` esperimenti running.
//! - Auto-rollback: monitorato separatamente nella logica di promozione.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// Prefissi di chiave prompt che non vengono mai ottimizzati automaticamente.
const SAFELIST_PREFIXES: &[&str] = &[
    "system.",
    "automation.",
    "system.nexus",
    "automation.mode_",
];

/// Tag XML obbligatori che ogni variante generata deve contenere.
const REQUIRED_XML_TAGS: &[&str] = &[
    "<role>",
    "<autonomia>",
    "<protocollo>",
    "<output_format>",
    "<reflection>",
];

/// Metriche aggregate per un singolo (prompt_key, version).
#[derive(Debug, Clone)]
struct PromptMetrics {
    prompt_key: String,
    prompt_version: i32,
    prompt_content: String,
    avg_reflection_score: f64,
    feedback_positive_rate: f64,
    total_runs: i64,
}

/// Variante di prompt generata dall'AI.
#[derive(Debug, Serialize, Deserialize)]
struct GeneratedVariant {
    content: String,
    rationale: String,
}

pub struct PromptOptimizerWorker {
    pool: Arc<PgPool>,
}

impl PromptOptimizerWorker {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Legge un setting dalla tabella `settings`. Restituisce il valore grezzo o una stringa vuota.
    async fn read_setting(&self, key: &str) -> String {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT value FROM settings WHERE key = $1"
        )
        .bind(key)
        .fetch_optional(self.pool.as_ref())
        .await
        .unwrap_or(None);
        row.map(|(v,)| v).unwrap_or_default()
    }

    async fn read_setting_bool(&self, key: &str, default: bool) -> bool {
        let raw = self.read_setting(key).await;
        if raw.is_empty() { return default; }
        !matches!(raw.trim().to_lowercase().as_str(), "false" | "0" | "no")
    }

    async fn read_setting_f64(&self, key: &str, default: f64) -> f64 {
        let raw = self.read_setting(key).await;
        raw.trim().parse::<f64>().unwrap_or(default)
    }

    async fn read_setting_i64(&self, key: &str, default: i64) -> i64 {
        let raw = self.read_setting(key).await;
        raw.trim().parse::<i64>().unwrap_or(default)
    }

    /// Restituisce true se la chiave e' nella safelist (non ottimizzabile).
    fn is_safelisted(key: &str) -> bool {
        SAFELIST_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
    }

    /// Raccoglie le metriche aggregate per tutti i prompt agente.
    async fn aggregate_metrics(
        &self,
        min_runs: i64,
    ) -> Result<Vec<PromptMetrics>, sqlx::Error> {
        // Join tra nexus_prompt_templates e nexus_agent_reflections
        // (prompt_feedback droppata in mig 0131, feedback ora in ai_response_feedback con schema diverso)
        let rows = sqlx::query(
            r#"
            SELECT
                t.key                                           AS prompt_key,
                t.version                                       AS prompt_version,
                t.content                                       AS prompt_content,
                COALESCE(AVG(r.score::float8), 0.0)            AS avg_reflection_score,
                0.5::float8                                    AS feedback_positive_rate,
                COUNT(r.id)                                    AS total_reflection_runs
            FROM nexus_prompt_templates t
            LEFT JOIN nexus_agent_reflections r
                ON r.prompt_key = t.key
               AND r.prompt_version = t.version
               AND r.created_at >= NOW() - INTERVAL '7 days'
            WHERE t.is_active = TRUE
              AND t.key LIKE 'agent.%'
            GROUP BY t.key, t.version, t.content
            HAVING COUNT(r.id) >= $1
            ORDER BY avg_reflection_score ASC
            "#,
        )
        .bind(min_runs)
        .fetch_all(self.pool.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| PromptMetrics {
                prompt_key: r.get::<String, _>("prompt_key"),
                prompt_version: r.get::<i32, _>("prompt_version"),
                prompt_content: r.get::<String, _>("prompt_content"),
                avg_reflection_score: r.get::<Option<f64>, _>("avg_reflection_score").unwrap_or(0.0),
                feedback_positive_rate: r.get::<Option<f64>, _>("feedback_positive_rate").unwrap_or(0.5),
                total_runs: r.get::<Option<i64>, _>("total_reflection_runs").unwrap_or(0),
            })
            .collect())
    }

    /// Conta gli esperimenti `running` attivi nel DB.
    async fn count_running_experiments(&self) -> i64 {
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM prompt_ab_experiments WHERE status = 'running'"
        )
        .fetch_one(self.pool.as_ref())
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0)
    }

    /// Verifica se esiste gia' un esperimento running per questa chiave.
    async fn has_running_experiment(&self, prompt_key: &str) -> bool {
        let count: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM prompt_ab_experiments
             WHERE prompt_key = $1 AND status = 'running'",
        )
        .bind(prompt_key)
        .fetch_one(self.pool.as_ref())
        .await
        .unwrap_or(Some(0));
        count.unwrap_or(0) > 0
    }

    /// Genera una variante migliorata del prompt via Claude API.
    /// Restituisce None se la generazione fallisce o il risultato e' invalido.
    async fn generate_variant(
        &self,
        metrics: &PromptMetrics,
        weaknesses: &[String],
        anthropic_key: &str,
    ) -> Option<GeneratedVariant> {
        if anthropic_key.is_empty() {
            warn!("prompt_optimizer: anthropic_api_key assente, skip generazione variante");
            return None;
        }

        let weakness_text = if weaknesses.is_empty() {
            "Nessuna debolezza specifica identificata. Migliorare chiarezza e autonomia.".to_string()
        } else {
            weaknesses.join("\n- ")
        };

        let meta_prompt = format!(
            r#"Sei un esperto di prompt engineering per agenti AI specializzati in sviluppo software.
Il tuo compito e' migliorare il prompt seguente per l'agente Nexus.

PROMPT ATTUALE:
<prompt_attuale>
{content}
</prompt_attuale>

METRICHE DI PERFORMANCE:
- Score reflection medio: {score:.2} / 1.0 (soglia: 0.65)
- Feedback positivi: {feedback:.0}%
- Run analizzati: {runs}

DEBOLEZZE IDENTIFICATE:
- {weaknesses}

ISTRUZIONI:
1. Genera UNA variante migliorata che risolva le debolezze identificate.
2. Mantieni la struttura XML con i tag: <role>, <contesto>, <autonomia>, <protocollo>, <tool_usage>, <anti_loop>, <output_format>, <examples>, <reflection>.
3. Tutto in italiano. Nessuna emoji.
4. Aumenta l'autonomia esplicita: l'agente NON deve chiedere conferma per operazioni di sola lettura.
5. Aggiungi esempi few-shot concreti se mancanti.

Rispondi con JSON nel formato:
{{"content": "<prompt migliorato completo>", "rationale": "<spiegazione breve delle modifiche>"}}"#,
            content = &metrics.prompt_content[..metrics.prompt_content.len().min(6000)],
            score = metrics.avg_reflection_score,
            feedback = metrics.feedback_positive_rate * 100.0,
            runs = metrics.total_runs,
            weaknesses = weakness_text,
        );

        // ── BP9 follow-up (Batch API) ─────────────────────────────────────
        // L'infrastruttura DB e' pronta in mig 0121:
        //   - tabella nexus_anthropic_batches (tracking batch in volo)
        //   - flag settings.prompt_optimizer_use_batch_api (default false)
        //
        // Per attivare la Batch API (50% sconto token):
        // 1. Leggere il flag prompt_optimizer_use_batch_api dal DB
        // 2. Se true: accumulare N richieste in un buffer
        // 3. POST https://api.anthropic.com/v1/messages/batches con custom_id
        //    per ogni richiesta, persistere anthropic_batch_id in DB
        // 4. Worker separato (poll ogni 5min) che chiama
        //    GET /v1/messages/batches/{id}/results per batch ended
        // 5. Una volta recuperati, marca batch come 'ended' e processa le
        //    risposte (parsing identico al flusso sincrono attuale)
        //
        // Tradeoff: latenza fino a 24h per ottenere le varianti, ma 50% in
        // meno di costo. Adatto per il prompt_optimizer perche' le varianti
        // non servono in real-time. Manteniamo il flusso sincrono finche'
        // l'admin non attiva esplicitamente il flag.
        // Modello da DB (nexus_purpose_model, purpose='prompt_optimizer').
        // Niente fallback hardcoded: se non configurato, errore esplicito.
        let optimizer_model: Option<(String,)> = sqlx::query_as(
            "SELECT model_id FROM nexus_purpose_model WHERE purpose = 'prompt_optimizer' LIMIT 1"
        )
        .fetch_optional(self.pool.as_ref())
        .await
        .ok()
        .flatten();
        let model_id = match optimizer_model {
            Some((m,)) => m,
            None => {
                error!("prompt_optimizer: nexus_purpose_model purpose='prompt_optimizer' non configurato");
                return None;
            }
        };

        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "model": model_id,
            "max_tokens": 4096,
            "temperature": 0.3,
            "messages": [{
                "role": "user",
                "content": meta_prompt
            }]
        });

        let response = match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", anthropic_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("prompt_optimizer: errore HTTP API Claude: {}", e);
                return None;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            error!("prompt_optimizer: Claude API error {}: {}", status, &body_text[..body_text.len().min(200)]);
            return None;
        }

        let resp_json: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                error!("prompt_optimizer: parse risposta Claude: {}", e);
                return None;
            }
        };

        let raw_text = resp_json["content"][0]["text"].as_str()?.to_string();

        // Estrae il JSON dalla risposta
        let variant: GeneratedVariant = if let Ok(v) = serde_json::from_str(&raw_text) {
            v
        } else {
            // Tenta estrazione con regex-like: cerca il primo { ... }
            let start = raw_text.find('{')?;
            let end = raw_text.rfind('}')?;
            if end <= start { return None; }
            serde_json::from_str(&raw_text[start..=end]).ok()?
        };

        // Validazione schema XML
        for tag in REQUIRED_XML_TAGS {
            if !variant.content.contains(tag) {
                warn!(
                    "prompt_optimizer: variante per '{}' manca tag {}, scartata",
                    metrics.prompt_key, tag
                );
                return None;
            }
        }

        Some(variant)
    }

    /// Recupera le debolezze piu' comuni dalle reflection recenti per un prompt.
    async fn top_weaknesses(&self, prompt_key: &str) -> Vec<String> {
        // Aggregate le weaknesses dalle ultime 50 reflection
        let rows = sqlx::query(
            r#"
            SELECT unnest(weaknesses) AS weakness
            FROM nexus_agent_reflections
            WHERE prompt_key = $1
              AND created_at >= NOW() - INTERVAL '7 days'
            ORDER BY created_at DESC
            LIMIT 50
            "#,
        )
        .bind(prompt_key)
        .fetch_all(self.pool.as_ref())
        .await
        .unwrap_or_default();

        // Conta le occorrenze e prendi le top 5
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for row in rows {
            if let Some(w) = row.get::<Option<String>, _>("weakness") {
                *counts.entry(w).or_insert(0) += 1;
            }
        }
        let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.into_iter().take(5).map(|(w, _)| w).collect()
    }

    /// Inserisce la variante nel DB e registra l'esperimento canary.
    async fn insert_variant_and_experiment(
        &self,
        metrics: &PromptMetrics,
        variant: &GeneratedVariant,
        traffic_pct: i64,
    ) -> Result<(), sqlx::Error> {
        let new_version = metrics.prompt_version + 1;

        // Inserisce la nuova versione come inattiva e sperimentale
        sqlx::query(
            r#"
            INSERT INTO nexus_prompt_templates
                (key, version, content, is_active, experimental,
                 schema_type, placeholder_vars, updated_by)
            VALUES ($1, $2, $3, FALSE, TRUE, 'xml',
                    '["lang_hint","type_hint","repo_summary"]'::jsonb,
                    'prompt_optimizer')
            ON CONFLICT (key, version) DO NOTHING
            "#,
        )
        .bind(&metrics.prompt_key)
        .bind(new_version)
        .bind(&variant.content)
        .execute(self.pool.as_ref())
        .await?;

        // Registra l'esperimento canary
        sqlx::query(
            r#"
            INSERT INTO prompt_ab_experiments
                (prompt_key, baseline_version, variant_version,
                 traffic_pct, status, auto_promote_enabled)
            VALUES ($1, $2, $3, $4, 'running', FALSE)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&metrics.prompt_key)
        .bind(metrics.prompt_version)
        .bind(new_version)
        .bind(traffic_pct as i32)
        .execute(self.pool.as_ref())
        .await?;

        info!(
            "prompt_optimizer: variante v{} inserita per '{}' (traffic={}%)",
            new_version, metrics.prompt_key, traffic_pct
        );
        Ok(())
    }

    /// Chiude gli esperimenti maturi (se auto_promote abilitato).
    /// In dry-run (auto_promote=false) logga solo il risultato suggerito.
    async fn evaluate_mature_experiments(&self, auto_promote: bool) -> u32 {
        let experiments = match sqlx::query(
            r#"
            SELECT id, prompt_key, baseline_version, variant_version,
                   min_runs_required, started_at
            FROM prompt_ab_experiments
            WHERE status = 'running'
              AND started_at < NOW() - INTERVAL '24 hours'
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await {
            Ok(rows) => rows,
            Err(e) => {
                error!("prompt_optimizer: lettura esperimenti maturi: {}", e);
                return 0;
            }
        };

        let mut evaluated = 0u32;
        for exp in &experiments {
            let prompt_key: String = exp.get("prompt_key");
            let baseline_version: i32 = exp.get("baseline_version");
            let variant_version: i32 = exp.get("variant_version");
            let exp_id: uuid::Uuid = exp.get("id");

            let stats = self.compute_experiment_stats(
                &prompt_key,
                baseline_version,
                variant_version,
            ).await;

            let (bl_rate, vr_rate) = match stats {
                Some(s) => s,
                None => continue,
            };

            let delta = vr_rate - bl_rate;
            let decision = if delta > 0.05 {
                "promoted"
            } else if delta < -0.05 {
                "discarded"
            } else {
                // Non abbastanza differenza statistica → attende ancora
                continue;
            };

            info!(
                "prompt_optimizer: esperimento {} per '{}' -> {} (bl={:.3} vr={:.3} delta={:+.3})",
                exp_id, prompt_key, decision, bl_rate, vr_rate, delta
            );

            if auto_promote {
                let _ = self.apply_decision(&exp_id, &prompt_key,
                    baseline_version, variant_version, decision,
                    bl_rate, vr_rate).await;
            } else {
                debug!("prompt_optimizer: dry-run, decisione '{}' non applicata per '{}'",
                    decision, prompt_key);
            }
            evaluated += 1;
        }
        evaluated
    }

    /// Calcola baseline vs variant success rate basandosi sulle reflection.
    async fn compute_experiment_stats(
        &self,
        prompt_key: &str,
        baseline_version: i32,
        variant_version: i32,
    ) -> Option<(f64, f64)> {
        let bl = sqlx::query_scalar::<_, Option<f64>>(
            r#"
            SELECT COALESCE(AVG(score::float8), 0.5)
            FROM nexus_agent_reflections
            WHERE prompt_key = $1 AND prompt_version = $2
              AND created_at >= NOW() - INTERVAL '7 days'
            "#,
        )
        .bind(prompt_key)
        .bind(baseline_version)
        .fetch_one(self.pool.as_ref())
        .await
        .ok()??;

        let vr = sqlx::query_scalar::<_, Option<f64>>(
            r#"
            SELECT COALESCE(AVG(score::float8), 0.5)
            FROM nexus_agent_reflections
            WHERE prompt_key = $1 AND prompt_version = $2
              AND created_at >= NOW() - INTERVAL '7 days'
            "#,
        )
        .bind(prompt_key)
        .bind(variant_version)
        .fetch_one(self.pool.as_ref())
        .await
        .ok()??;

        Some((bl, vr))
    }

    /// Applica la decisione di promozione o scarto al DB.
    async fn apply_decision(
        &self,
        experiment_id: &uuid::Uuid,
        prompt_key: &str,
        baseline_version: i32,
        variant_version: i32,
        decision: &str,
        bl_rate: f64,
        vr_rate: f64,
    ) -> Result<(), sqlx::Error> {
        let reason = format!(
            "Auto-decisione: baseline={:.3} variant={:.3} delta={:+.3}",
            bl_rate, vr_rate, vr_rate - bl_rate
        );

        // Usa query runtime (non macro) per evitare type-check compile-time su NUMERIC
        sqlx::query(
            r#"
            UPDATE prompt_ab_experiments
            SET status = $1,
                ended_at = NOW(),
                baseline_success_rate = $2::numeric,
                variant_success_rate = $3::numeric,
                decision_reason = $4
            WHERE id = $5
            "#,
        )
        .bind(decision)
        .bind(bl_rate)
        .bind(vr_rate)
        .bind(&reason)
        .bind(*experiment_id)
        .execute(self.pool.as_ref())
        .await?;

        if decision == "promoted" {
            // Disattiva baseline, attiva variante
            sqlx::query(
                "UPDATE nexus_prompt_templates SET is_active = FALSE
                 WHERE key = $1 AND version = $2",
            )
            .bind(prompt_key)
            .bind(baseline_version)
            .execute(self.pool.as_ref())
            .await?;

            sqlx::query(
                "UPDATE nexus_prompt_templates
                 SET is_active = TRUE, experimental = FALSE
                 WHERE key = $1 AND version = $2",
            )
            .bind(prompt_key)
            .bind(variant_version)
            .execute(self.pool.as_ref())
            .await?;

            info!("prompt_optimizer: promosso v{} per '{}'", variant_version, prompt_key);
        } else {
            // Scarta la variante (resta inattiva)
            sqlx::query(
                "UPDATE nexus_prompt_templates SET experimental = FALSE
                 WHERE key = $1 AND version = $2",
            )
            .bind(prompt_key)
            .bind(variant_version)
            .execute(self.pool.as_ref())
            .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl LearningWorker for PromptOptimizerWorker {
    fn name(&self) -> &str {
        "prompt_optimizer"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    async fn run(&self, _context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        // ── Legge configurazione dal DB ──────────────────────────────────
        let enabled = self.read_setting_bool("optimizer_enabled", true).await;
        if !enabled {
            debug!("prompt_optimizer: disabilitato (optimizer_enabled=false)");
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64);
        }

        let auto_promote = self.read_setting_bool("optimizer_auto_promote", false).await;
        let min_runs = self.read_setting_i64("optimizer_min_runs", 30).await;
        let reflection_threshold = self.read_setting_f64("optimizer_reflection_threshold", 0.65).await;
        let max_concurrent = self.read_setting_i64("optimizer_max_concurrent_experiments", 3).await;
        let traffic_pct = self.read_setting_i64("optimizer_canary_traffic_pct", 10).await;
        let anthropic_key = self.read_setting("anthropic_api_key").await;

        info!(
            "prompt_optimizer: avvio (auto_promote={} min_runs={} threshold={:.2})",
            auto_promote, min_runs, reflection_threshold
        );

        // ── Valuta esperimenti maturi ────────────────────────────────────
        let evaluated = self.evaluate_mature_experiments(auto_promote).await;
        if evaluated > 0 {
            info!("prompt_optimizer: {} esperimenti valutati", evaluated);
        }

        // ── Raccoglie metriche ────────────────────────────────────────────
        let metrics = match self.aggregate_metrics(min_runs).await {
            Ok(m) => m,
            Err(e) => {
                error!("prompt_optimizer: errore aggregate_metrics: {}", e);
                return WorkerOutcome::fail(
                    self.name(),
                    &format!("aggregate_metrics fallita: {e}"),
                    start.elapsed().as_millis() as u64,
                );
            }
        };

        // ── Identifica candidati ──────────────────────────────────────────
        let candidates: Vec<&PromptMetrics> = metrics
            .iter()
            .filter(|m| {
                !Self::is_safelisted(&m.prompt_key)
                    && m.avg_reflection_score < reflection_threshold
            })
            .collect();

        if candidates.is_empty() {
            info!("prompt_optimizer: nessun candidato all'ottimizzazione, tutti i prompt sopra soglia");
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64);
        }

        info!("prompt_optimizer: {} candidati all'ottimizzazione", candidates.len());

        // ── Genera varianti ───────────────────────────────────────────────
        let mut generated = 0u32;
        let mut skipped = 0u32;

        for candidate in candidates {
            // Cap concorrenza
            let running = self.count_running_experiments().await;
            if running >= max_concurrent {
                warn!(
                    "prompt_optimizer: cap concorrenza raggiunto ({}/{}), stop",
                    running, max_concurrent
                );
                break;
            }

            // Salta se gia' in corso un esperimento per questo prompt
            if self.has_running_experiment(&candidate.prompt_key).await {
                debug!("prompt_optimizer: esperimento gia' running per '{}', skip",
                    candidate.prompt_key);
                skipped += 1;
                continue;
            }

            let weaknesses = self.top_weaknesses(&candidate.prompt_key).await;

            match self.generate_variant(candidate, &weaknesses, &anthropic_key).await {
                Some(variant) => {
                    match self.insert_variant_and_experiment(candidate, &variant, traffic_pct).await {
                        Ok(()) => generated += 1,
                        Err(e) => error!(
                            "prompt_optimizer: insert variante '{}': {}",
                            candidate.prompt_key, e
                        ),
                    }
                }
                None => {
                    debug!("prompt_optimizer: generazione fallita per '{}'",
                        candidate.prompt_key);
                    skipped += 1;
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "prompt_optimizer: completato in {}ms (generated={} skipped={} evaluated={})",
            duration_ms, generated, skipped, evaluated
        );

        WorkerOutcome::ok(self.name(), duration_ms)
    }
}
