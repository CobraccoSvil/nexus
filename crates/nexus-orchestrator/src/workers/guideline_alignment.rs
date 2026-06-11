//! GuidelineAlignmentWorker — agente di allineamento direttive di prompt engineering.
//!
//! Worker periodico che valuta la conformita' dei template prompt attivi alle
//! direttive (guideline) approvate dall'admin e, quando abilitato, propone o
//! genera revisioni:
//!
//! 1. **Dirty-check**: salta i template gia' valutati di recente (stesso
//!    `content_hash` + `guideline_set_hash`, `checked_at` entro l'intervallo
//!    configurato). Lo scheduler dei periodic worker tickka ogni 1800s e ignora
//!    `interval()`, quindi il throttling 24h e' interno qui.
//! 2. **Conformance check** (brain `POST /agent/prompt-revise`, mode `evaluate`):
//!    salva l'esito in `nexus_prompt_conformance` (append-only, ON CONFLICT
//!    DO NOTHING).
//! 3. **Revisione** (solo se `alignment_autovariant_enabled` e score sotto soglia):
//!    - `system.*`/`automation.*` (safelist): genera una PROPOSTA in
//!      `nexus_alignment_proposal` (status pending) da approvare a mano.
//!      Mai auto-applicata.
//!    - `agent.*`: genera variante + esperimento A/B (riusa
//!      `prompt_variants::insert_variant_and_experiment`).
//!
//! ## Protezioni
//! - Kill switch globale: `alignment_enabled=false` (default) = no-op.
//! - Revisione automatica separata: `alignment_autovariant_enabled=false`
//!   (default) = solo valutazione.
//! - Cap costo: `alignment_max_checks_per_tick` conformance check per esecuzione.
//! - Selezione modello tier-only lato brain (purpose `prompt_conformance_check`).

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use crate::workers::prompt_variants;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// Template attivo candidato alla valutazione di conformita'.
#[derive(Debug, Clone)]
struct TemplateRow {
    key: String,
    version: i32,
    content: String,
}

pub struct GuidelineAlignmentWorker {
    pool: Arc<PgPool>,
}

impl GuidelineAlignmentWorker {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// Legge un setting dalla tabella `settings`. Valore grezzo o stringa vuota.
    async fn read_setting(&self, key: &str) -> String {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(self.pool.as_ref())
            .await
            .ok()
            .flatten();
        row.map(|(v,)| v).unwrap_or_default()
    }

    async fn read_setting_bool(&self, key: &str, default: bool) -> bool {
        let raw = self.read_setting(key).await;
        if raw.is_empty() {
            return default;
        }
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

    /// SHA-256 esadecimale di una stringa.
    fn sha256_hex(input: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Hash dell'insieme di guideline attive: concatenazione ordinata di
    /// `practice_key|version`. Cambia (e forza una rivalutazione) ogni volta che
    /// una guideline viene attivata/disattivata o versionata.
    async fn guideline_set_hash(&self) -> Result<String, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT practice_key, version
            FROM nexus_prompt_guideline
            WHERE is_active = TRUE
            ORDER BY practice_key
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        let concat: String = rows
            .iter()
            .map(|r| {
                let key: String = r.get("practice_key");
                let version: i32 = r.get("version");
                format!("{key}|{version}")
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(Self::sha256_hex(&concat))
    }

    /// Seleziona i template attivi candidati (agent./system./automation.).
    async fn select_templates(&self, cap: i64) -> Result<Vec<TemplateRow>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT key, version, content
            FROM nexus_prompt_templates
            WHERE is_active = TRUE
              AND (key LIKE 'agent.%' OR key LIKE 'system.%' OR key LIKE 'automation.%')
            ORDER BY key
            LIMIT $1
            "#,
        )
        .bind(cap)
        .fetch_all(self.pool.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| TemplateRow {
                key: r.get("key"),
                version: r.get("version"),
                content: r.get("content"),
            })
            .collect())
    }

    /// Dirty-check: true se per (key, version, content_hash, guideline_set_hash)
    /// esiste gia' una riga conformance verificata entro `interval_hours`.
    async fn already_checked_recently(
        &self,
        key: &str,
        version: i32,
        content_hash: &str,
        guideline_set_hash: &str,
        interval_hours: i64,
    ) -> bool {
        let count: Option<i64> = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM nexus_prompt_conformance
            WHERE prompt_key = $1
              AND prompt_version = $2
              AND content_hash = $3
              AND guideline_set_hash = $4
              AND checked_at >= NOW() - make_interval(hours => $5)
            "#,
        )
        .bind(key)
        .bind(version)
        .bind(content_hash)
        .bind(guideline_set_hash)
        .bind(interval_hours as i32)
        .fetch_one(self.pool.as_ref())
        .await
        .ok()
        .flatten();
        count.unwrap_or(0) > 0
    }

    /// Inserisce l'esito conformance (ON CONFLICT DO NOTHING) e ritorna l'id
    /// della riga (sia appena inserita sia gia' esistente per la stessa chiave).
    async fn insert_conformance(
        &self,
        key: &str,
        version: i32,
        content_hash: &str,
        guideline_set_hash: &str,
        result: &prompt_variants::PromptReviseResult,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO nexus_prompt_conformance
                (prompt_key, prompt_version, content_hash, guideline_set_hash,
                 overall_score, dimensions, issues)
            VALUES ($1, $2, $3, $4, $5::numeric, $6, $7)
            ON CONFLICT (prompt_key, prompt_version, content_hash, guideline_set_hash)
            DO NOTHING
            "#,
        )
        .bind(key)
        .bind(version)
        .bind(content_hash)
        .bind(guideline_set_hash)
        .bind(result.overall_score)
        .bind(result.dimensions_json())
        .bind(result.issues_json())
        .execute(self.pool.as_ref())
        .await?;

        // Recupera l'id (la riga esiste sempre dopo l'INSERT idempotente).
        let id: Option<i64> = sqlx::query_scalar(
            r#"
            SELECT id
            FROM nexus_prompt_conformance
            WHERE prompt_key = $1
              AND prompt_version = $2
              AND content_hash = $3
              AND guideline_set_hash = $4
            "#,
        )
        .bind(key)
        .bind(version)
        .bind(content_hash)
        .bind(guideline_set_hash)
        .fetch_optional(self.pool.as_ref())
        .await?;

        Ok(id)
    }

    /// Verifica se esiste gia' un esperimento running per questa chiave
    /// (stessa logica dell'optimizer: niente A/B concorrenti sullo stesso prompt).
    async fn has_running_experiment(&self, prompt_key: &str) -> bool {
        let count: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prompt_ab_experiments
             WHERE prompt_key = $1 AND status = 'running'",
        )
        .bind(prompt_key)
        .fetch_one(self.pool.as_ref())
        .await
        .ok()
        .flatten();
        count.unwrap_or(0) > 0
    }

    /// Inserisce una proposta di revisione per un prompt safelist
    /// (system.*/automation.*). Mai auto-applicata: status pending, ON CONFLICT
    /// DO NOTHING per evitare proposte pending duplicate sulla stessa baseline.
    async fn insert_proposal(
        &self,
        prompt_key: &str,
        baseline_version: i32,
        proposed_content: &str,
        rationale: Option<&str>,
        conformance_id: Option<i64>,
    ) -> Result<bool, sqlx::Error> {
        let affected = sqlx::query(
            r#"
            INSERT INTO nexus_alignment_proposal
                (prompt_key, baseline_version, proposed_content, rationale,
                 trigger_source, conformance_id, status)
            VALUES ($1, $2, $3, $4, 'guideline', $5, 'pending')
            ON CONFLICT (prompt_key, baseline_version, status) DO NOTHING
            "#,
        )
        .bind(prompt_key)
        .bind(baseline_version)
        .bind(proposed_content)
        .bind(rationale)
        .bind(conformance_id)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();
        Ok(affected > 0)
    }
}

#[async_trait]
impl LearningWorker for GuidelineAlignmentWorker {
    fn name(&self) -> &str {
        "guideline_alignment"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    async fn run(&self, _context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        // ── Configurazione dal DB ────────────────────────────────────────────
        let enabled = self.read_setting_bool("alignment_enabled", false).await;
        if !enabled {
            debug!("guideline_alignment: disabilitato (alignment_enabled=false)");
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64);
        }

        let threshold = self
            .read_setting_f64("alignment_conformance_threshold", 0.75)
            .await;
        let interval_hours = self.read_setting_i64("alignment_check_interval_hours", 24).await;
        let max_checks = self.read_setting_i64("alignment_max_checks_per_tick", 20).await;
        let autovariant = self
            .read_setting_bool("alignment_autovariant_enabled", false)
            .await;

        info!(
            "guideline_alignment: avvio (threshold={:.2} interval_h={} max_checks={} autovariant={})",
            threshold, interval_hours, max_checks, autovariant
        );

        // ── Hash dell'insieme di guideline attive (dirty-check) ──────────────
        let guideline_set_hash = match self.guideline_set_hash().await {
            Ok(h) => h,
            Err(e) => {
                error!("guideline_alignment: lettura guideline attive fallita: {e}");
                return WorkerOutcome::fail(
                    self.name(),
                    format!("guideline_set_hash fallita: {e}"),
                    start.elapsed().as_millis() as u64,
                );
            }
        };

        // ── Template candidati ───────────────────────────────────────────────
        let templates = match self.select_templates(max_checks).await {
            Ok(t) => t,
            Err(e) => {
                error!("guideline_alignment: select_templates fallita: {e}");
                return WorkerOutcome::fail(
                    self.name(),
                    format!("select_templates fallita: {e}"),
                    start.elapsed().as_millis() as u64,
                );
            }
        };

        let mut checked = 0u32;
        let mut skipped = 0u32;
        let mut proposals = 0u32;
        let mut variants = 0u32;

        for tpl in &templates {
            let content_hash = Self::sha256_hex(&tpl.content);

            // Dirty-check: salta se gia' valutato di recente con stesso hash.
            if self
                .already_checked_recently(
                    &tpl.key,
                    tpl.version,
                    &content_hash,
                    &guideline_set_hash,
                    interval_hours,
                )
                .await
            {
                skipped += 1;
                continue;
            }

            // ── Conformance check (evaluate-only) ────────────────────────────
            let eval = match prompt_variants::call_prompt_revise(
                self.pool.as_ref(),
                &tpl.key,
                &tpl.content,
                prompt_variants::ReviseMode::Evaluate,
                prompt_variants::SignalKind::Guideline,
                &[],
                serde_json::json!({}),
            )
            .await
            {
                Some(r) if r.status == "completed" => r,
                Some(r) => {
                    warn!(
                        "guideline_alignment: conformance status='{}' per '{}', skip",
                        r.status, tpl.key
                    );
                    skipped += 1;
                    continue;
                }
                None => {
                    skipped += 1;
                    continue;
                }
            };

            let conformance_id = match self
                .insert_conformance(
                    &tpl.key,
                    tpl.version,
                    &content_hash,
                    &guideline_set_hash,
                    &eval,
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    error!(
                        "guideline_alignment: insert conformance '{}' fallito: {e}",
                        tpl.key
                    );
                    skipped += 1;
                    continue;
                }
            };
            checked += 1;

            // ── Revisione automatica (solo se sotto soglia e abilitata) ──────
            if eval.overall_score >= threshold || !autovariant {
                continue;
            }

            // Genera la revisione (evaluate_and_revise).
            let revision = match prompt_variants::call_prompt_revise(
                self.pool.as_ref(),
                &tpl.key,
                &tpl.content,
                prompt_variants::ReviseMode::EvaluateAndRevise,
                prompt_variants::SignalKind::Guideline,
                &[],
                serde_json::json!({}),
            )
            .await
            {
                Some(r) if r.status == "completed" => r,
                Some(r) => {
                    warn!(
                        "guideline_alignment: revisione status='{}' per '{}', skip",
                        r.status, tpl.key
                    );
                    continue;
                }
                None => continue,
            };

            let revised = match revision.revised_template {
                Some(ref c) if !c.trim().is_empty() => c.clone(),
                _ => {
                    debug!(
                        "guideline_alignment: revisione senza revised_template per '{}'",
                        tpl.key
                    );
                    continue;
                }
            };

            if prompt_variants::is_safelisted(&tpl.key) {
                // Prompt protetto: proposta admin, mai auto-applicata.
                match self
                    .insert_proposal(
                        &tpl.key,
                        tpl.version,
                        &revised,
                        revision.rationale.as_deref(),
                        conformance_id,
                    )
                    .await
                {
                    Ok(true) => {
                        proposals += 1;
                        info!(
                            "guideline_alignment: proposta creata per '{}' (score={:.3})",
                            tpl.key, eval.overall_score
                        );
                    }
                    Ok(false) => debug!(
                        "guideline_alignment: proposta gia' pending per '{}', skip",
                        tpl.key
                    ),
                    Err(e) => error!(
                        "guideline_alignment: insert proposta '{}' fallito: {e}",
                        tpl.key
                    ),
                }
            } else {
                // Prompt agent.*: variante + esperimento A/B.
                if self.has_running_experiment(&tpl.key).await {
                    debug!(
                        "guideline_alignment: esperimento gia' running per '{}', skip",
                        tpl.key
                    );
                    continue;
                }

                // Traffic canary allineato al default optimizer (10%).
                let traffic_pct = self.read_setting_i64("optimizer_canary_traffic_pct", 10).await;
                match prompt_variants::insert_variant_and_experiment(
                    self.pool.as_ref(),
                    &tpl.key,
                    tpl.version,
                    &revised,
                    traffic_pct,
                )
                .await
                {
                    Ok(()) => {
                        variants += 1;
                        info!(
                            "guideline_alignment: variante A/B creata per '{}' (score={:.3})",
                            tpl.key, eval.overall_score
                        );
                    }
                    Err(e) => error!(
                        "guideline_alignment: insert variante '{}' fallito: {e}",
                        tpl.key
                    ),
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "guideline_alignment: completato in {}ms (checked={} skipped={} proposals={} variants={})",
            duration_ms, checked, skipped, proposals, variants
        );

        WorkerOutcome::ok(self.name(), duration_ms)
            .with_metric("checked", checked as f32)
            .with_metric("proposals", proposals as f32)
            .with_metric("variants", variants as f32)
    }
}
