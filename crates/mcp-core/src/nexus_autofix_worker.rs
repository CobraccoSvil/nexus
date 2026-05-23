// ═══════════════════════════════════════════════════════════════════════════
// nexus_autofix_worker.rs — NexusAutoFixAgent: intercetta failure E2E e
// genera proposte di fix nel registry change_drafts.
//
// Workflow (MVP, no PR automatiche in questo step):
//   1. Periodicamente (default 5 min): SELECT nexus_e2e_runs WHERE
//      status='failed' AND NOT EXISTS (autofix per quel run)
//   2. Per ogni failure: carica log_excerpt + scenario
//   3. (TODO step successivo) chiama LLM con purpose `autofix_planner` per
//      generare {root_cause, files_to_change, proposed_patch}
//   4. INSERT change_drafts con trigger_kind='autofix', status='pending'
//   5. (TODO step successivo) worker downstream applica la patch via
//      worktree + gh pr create se settings('meta_docs.autofix_enabled')='true'
//
// In questo MVP saltiamo lo step 3 e 5: ci limitiamo a registrare il draft
// con la causa euristica (scenario name + estratto log) per validare la
// pipeline end-to-end. La LLM call sara' aggiunta quando un E2E reale
// fallira' nella nostra suite.
// ═══════════════════════════════════════════════════════════════════════════

use crate::AppState;
use serde_json::{json, Value};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_AUTOFIX_INTERVAL_SECS: u64 = 300;

pub fn start_nexus_autofix_worker(state: AppState) {
    tokio::spawn(async move {
        // Delay iniziale per non sovraccaricare il boot
        tokio::time::sleep(Duration::from_secs(90)).await;
        loop {
            let interval = read_interval(&state).await;
            if let Err(e) = tick(&state).await {
                tracing::warn!(error = %e, "nexus_autofix_worker: tick error");
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
    tracing::info!("nexus_autofix_worker: avviato");
}

async fn read_interval(state: &AppState) -> u64 {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'meta_docs.autofix_interval_secs'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse::<u64>().ok())
    .unwrap_or(DEFAULT_AUTOFIX_INTERVAL_SECS)
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    // Check enabled flag
    let enabled: bool = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'meta_docs.autofix_enabled'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(true);

    if !enabled {
        return Ok(());
    }

    // Trova E2E runs falliti che non hanno gia' un autofix draft
    let rows = sqlx::query(
        r#"
        SELECT r.id, r.scenario, r.failed_assertion, r.log_excerpt, r.started_at
        FROM nexus_e2e_runs r
        WHERE r.status = 'failed'
          AND r.started_at > NOW() - INTERVAL '24 hours'
          AND NOT EXISTS (
              SELECT 1 FROM change_drafts d
              WHERE d.trigger_kind = 'autofix'
                AND (d.draft_json->'context'->>'e2e_run_id') = r.id::text
          )
        ORDER BY r.started_at DESC
        LIMIT 5
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    tracing::info!(
        failures = rows.len(),
        "nexus_autofix_worker: rilevati E2E fallimenti non ancora processati"
    );

    for r in rows {
        let run_id: Uuid = r.try_get("id")?;
        let scenario: String = r.try_get("scenario").unwrap_or_default();
        let failed_assertion: Option<String> = r.try_get("failed_assertion").ok();
        let log_excerpt: Option<String> = r.try_get("log_excerpt").ok();

        let summary = format!(
            "E2E fallito: {} — {}",
            scenario,
            failed_assertion.as_deref().unwrap_or("(no assertion)")
        );

        // Draft MVP: contesto + diagnostica grezza. Il vero piano di fix
        // (LLM-driven) verra' calcolato dal worker apply downstream o da
        // un sub-agent Claude Code che legge questo draft.
        let draft = json!({
            "razionale": format!(
                "Il test E2E '{}' e' fallito. Analizzare la causa e proporre un fix.",
                scenario
            ),
            "impact_analysis": {
                "files_to_modify": [],
                "files_potentially_affected": [],
                "breaking_changes": false,
                "migration_required": false,
                "tests_to_update": [scenario.clone()],
            },
            "diff_proposto": null,
            "verification_steps": [
                format!("Ri-eseguire: npx playwright test e2e/nexus-self/{}.spec.ts", scenario),
                "Verificare assenza di errori in core.log e neural.log".to_string(),
            ],
            "alternative_considerate": [],
            "doc_da_aggiornare": [],
            "context": {
                "e2e_run_id": run_id.to_string(),
                "scenario": scenario,
                "failed_assertion": failed_assertion,
                "log_excerpt": log_excerpt,
            }
        });

        let draft_id = Uuid::new_v4();
        let res = sqlx::query(
            r#"
            INSERT INTO change_drafts (id, trigger_kind, summary, draft_json, status)
            VALUES ($1, 'autofix', $2, $3, 'pending')
            "#,
        )
        .bind(draft_id)
        .bind(&summary)
        .bind(&draft as &Value)
        .execute(&state.db)
        .await;

        match res {
            Ok(_) => tracing::info!(
                draft_id = %draft_id,
                e2e_run = %run_id,
                scenario = %scenario,
                "nexus_autofix_worker: draft creato"
            ),
            Err(e) => tracing::warn!(error = %e, "nexus_autofix_worker: insert draft fallito"),
        }
    }

    Ok(())
}
