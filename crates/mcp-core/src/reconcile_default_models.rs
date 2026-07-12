//! Riconcilia `nexus_provider_default_model` e `settings.provider_model_*` quando
//! puntano a modelli disabilitati/legacy nel catalog (ADR 0025, mig 0321).
//!
//! Punto unico (regola L): invocato dopo `catalog_sync` e quando un health probe
//! rileva un errore model-specific sul default del provider.

use sqlx::PgPool;

/// Aggiorna le righe il cui modello corrente NON e' `is_enabled=true` nel catalog,
/// scegliendo il miglior modello agentic-eligibile per provider (featured prima,
/// poi costo crescente). Idempotente: le righe gia' valide non vengono toccate.
pub async fn reconcile_provider_default_models(db: &PgPool) -> Result<u64, sqlx::Error> {
    let default_rows = sqlx::query(
        r#"
        WITH best AS (
            SELECT provider, model FROM (
                SELECT provider, model,
                       row_number() OVER (
                           PARTITION BY provider
                           ORDER BY is_featured DESC,
                                    input_cost_per_million_tokens ASC NULLS LAST
                       ) AS rn
                FROM ai_price_catalog
                WHERE is_enabled = true
                  AND supports_tool_use = true
                  AND agentic_thinking_policy <> 'exclude'
            ) t WHERE rn = 1
        )
        UPDATE nexus_provider_default_model d
        SET model_id = b.model
        FROM best b
        WHERE b.provider = d.provider
          AND NOT EXISTS (
              SELECT 1 FROM ai_price_catalog c
              WHERE c.provider = d.provider
                AND c.model = d.model_id
                AND c.is_enabled = true
          )
        "#,
    )
    .execute(db)
    .await?;

    let settings_rows = sqlx::query(
        r#"
        WITH best AS (
            SELECT provider, model FROM (
                SELECT provider, model,
                       row_number() OVER (
                           PARTITION BY provider
                           ORDER BY is_featured DESC,
                                    input_cost_per_million_tokens ASC NULLS LAST
                       ) AS rn
                FROM ai_price_catalog
                WHERE is_enabled = true
                  AND supports_tool_use = true
                  AND agentic_thinking_policy <> 'exclude'
            ) t WHERE rn = 1
        )
        UPDATE settings s
        SET value = b.model, updated_at = NOW()
        FROM best b
        WHERE s.key = 'provider_model_' || b.provider
          AND NOT EXISTS (
              SELECT 1 FROM ai_price_catalog c
              WHERE c.provider = b.provider
                AND c.model = s.value
                AND c.is_enabled = true
          )
        "#,
    )
    .execute(db)
    .await?;

    let total = default_rows.rows_affected() + settings_rows.rows_affected();
    if total > 0 {
        tracing::info!(
            "reconcile_default_models: aggiornate {} righe (default_model={}, settings={})",
            total,
            default_rows.rows_affected(),
            settings_rows.rows_affected()
        );
    }
    Ok(total)
}
