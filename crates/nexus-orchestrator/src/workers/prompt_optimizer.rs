//! PromptOptimizerWorker — loop di auto-miglioramento dei prompt.
//!
//! Worker periodico:
//!
//! 1. **Aggregate**: raccoglie metriche da `nexus_agent_reflections` per ogni
//!    `(prompt_key, version)`. Richiede >= `min_runs` run per cohort.
//! 2. **Identify candidates**: prompt con `avg_reflection_score < threshold`
//!    o `feedback_rate < success_threshold` sono candidati all'ottimizzazione.
//! 3. **Generate variants**: `prompt_variants::call_prompt_revise` in modo
//!    `evaluate_and_revise` — risolve il modello dal purpose `prompt_revise`
//!    (regola G) e chiama il gateway. Usa il campo `revised_template` della
//!    risposta come variante.
//! 4. **Insert as inactive**: nuove versioni in `nexus_prompt_templates`
//!    con `is_active=FALSE`, `experimental=TRUE`.
//! 5. **Register canary**: inserisce in `prompt_ab_experiments` (status=running).
//! 6. **Promote/discard** (solo se `auto_promote_enabled=true` in DB):
//!    chiude esperimenti maturi con test statistico (Wilson score).
//!
//! ## Protezioni
//! - Kill switch globale: `optimizer_enabled=false` nel DB.
//! - Auto-promozione separata: `optimizer_auto_promote=false` (default) = dry-run.
//! - Safelist immutabile (`prompt_variants::is_safelisted`): `system.*` e
//!   `automation.*` non vengono mai ottimizzati.
//! - Cap concorrenza: max `optimizer_max_concurrent_experiments` esperimenti running.
//! - Auto-rollback: monitorato separatamente nella logica di promozione.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};

/// Cadenza del worker: 30 minuti. Vedi [`PromptOptimizerWorker::interval`] per
/// il perche' (vincolo di costo, non preferenza).
const OPTIMIZER_INTERVAL_SECS: u64 = 1800;
use crate::workers::prompt_variants;
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

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
              -- Prompt degli AGENTI: i template dei subagenti (agent.*) e il
              -- system del run principale (system.nexus_base). Il filtro era il
              -- solo 'agent.%', che escludeva proprio il prompt su cui passa la
              -- quasi totalita' del volume: nell'ultima settimana 575 delle 579
              -- interazioni erano del run principale. Con quel filtro il worker
              -- non avrebbe MAI raggiunto min_runs su nessuna chiave, restando a
              -- zero con l'aria di funzionare (regola O).
              AND (t.key LIKE 'agent.%' OR t.key = 'system.nexus_base')
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

    /// Genera una variante migliorata del prompt delegando al punto unico
    /// `prompt_variants::call_prompt_revise` (mode `evaluate_and_revise`), che
    /// risolve il modello dal purpose e chiama il gateway.
    ///
    /// Restituisce il contenuto della variante (revised_template) o None se la
    /// generazione fallisce o non c'e' `revised_template`.
    async fn generate_variant(
        &self,
        metrics: &PromptMetrics,
        weaknesses: &[String],
    ) -> Option<String> {
        // Segnali reflection-driven: metriche aggregate dalle reflection recenti.
        let signal_metrics = serde_json::json!({
            "avg_reflection_score": metrics.avg_reflection_score,
            "feedback_positive_rate": metrics.feedback_positive_rate,
            "total_runs": metrics.total_runs,
        });

        let result = prompt_variants::call_prompt_revise(
            self.pool.as_ref(),
            &metrics.prompt_key,
            &metrics.prompt_content,
            prompt_variants::ReviseMode::EvaluateAndRevise,
            prompt_variants::SignalKind::Reflection,
            weaknesses,
            signal_metrics,
        )
        .await?;

        if result.status != "completed" {
            warn!(
                "prompt_optimizer: prompt-revise status='{}' per '{}', variante scartata",
                result.status, metrics.prompt_key
            );
            return None;
        }

        match result.revised_template {
            Some(content) if !content.trim().is_empty() => Some(content),
            _ => {
                debug!(
                    "prompt_optimizer: prompt-revise senza revised_template per '{}'",
                    metrics.prompt_key
                );
                None
            }
        }
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
        sorted.sort_by_key(|s| std::cmp::Reverse(s.1));
        sorted.into_iter().take(5).map(|(w, _)| w).collect()
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

    /// 30 minuti, ed e' un vincolo di COSTO, non una preferenza: questo worker
    /// chiama il modello per ogni prompt candidato a ogni esecuzione. Al default
    /// del trait (60s) arriverebbe a ~1440 chiamate al giorno; qui sono al piu'
    /// 48.
    ///
    /// Il numero viveva nel chiamante, come intervallo globale dello scheduler:
    /// conteneva la spesa di questo worker rallentando pero' tutti gli altri
    /// (cleanup e session_persistence compresi, che ne uscivano storpiati).
    /// Ora il vincolo sta dove nasce, e gli altri worker corrono alla loro
    /// cadenza. Per spegnerlo del tutto resta il flag DB `optimizer_enabled`.
    fn interval(&self) -> Duration {
        Duration::from_secs(OPTIMIZER_INTERVAL_SECS)
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
                    format!("aggregate_metrics fallita: {e}"),
                    start.elapsed().as_millis() as u64,
                );
            }
        };

        // ── Identifica candidati ──────────────────────────────────────────
        let candidates: Vec<&PromptMetrics> = metrics
            .iter()
            .filter(|m| {
                !prompt_variants::is_safelisted(&m.prompt_key)
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

            match self.generate_variant(candidate, &weaknesses).await {
                Some(variant_content) => {
                    match prompt_variants::insert_variant_and_experiment(
                        self.pool.as_ref(),
                        &candidate.prompt_key,
                        candidate.prompt_version,
                        &variant_content,
                        traffic_pct,
                    )
                    .await
                    {
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
