//! Riconcilia `nexus_provider_default_model` e `settings.provider_model_*` quando
//! puntano a modelli disabilitati/legacy nel catalog (ADR 0025, mig 0321).
//!
//! Punto unico (regola L): invocato dopo `catalog_sync` e quando un health probe
//! rileva un errore model-specific sul default del provider.

use sqlx::PgPool;

/// La CTE `best`: il miglior modello agentic-eleggibile per OGNI provider
/// (featured prima, poi costo crescente).
///
/// Esiste come funzione perche' la stessa CTE era scritta DUE VOLTE in questo
/// file — una duplicazione nemmeno cross-modulo, interna — e le due copie
/// dovevano restare allineate a mano. Il censimento dei punti unici (2026-07-15)
/// l'ha trovata: e' la forma piu' piccola possibile del difetto che ha ucciso il
/// consiglio, cioe' due posti che rispondono alla stessa domanda.
///
/// GATE DI QUALIFICAZIONE (fase 3c): questo modulo scrive configurazione
/// PERSISTENTE (`nexus_provider_default_model`, `settings.provider_model_*`) che
/// sopravvive al riavvio. Senza gate poteva "riparare" un default puntandolo a un
/// modello non qualificato o preview: un guasto silenzioso e duraturo, scritto
/// dal meccanismo che serve a ripararne un altro. Il gate arriva dalla stessa
/// sorgente del routing live, non da una copia di regole.
///
/// Escluso anche il modello marcato MORTO dal probe: riconciliare verso un 404
/// e' esattamente il contrario dello scopo di questa funzione.
fn best_per_provider_cte(gate: crate::orchestrator::QualificationGate) -> String {
    let mut filtri = String::from(
        "WHERE is_enabled = true
           AND supports_tool_use = true
           AND agentic_thinking_policy <> 'exclude'
           AND (auto_disabled_reason IS NULL
                OR (auto_disabled_reason NOT LIKE 'invalid_model%'
                    AND auto_disabled_reason NOT LIKE 'model_not_found%'))",
    );
    if gate.require_qualified {
        filtri.push_str(
            " AND qualification_state = 'qualified'
              AND (qualification_expires_at IS NULL OR qualification_expires_at > now())",
        );
    }
    if gate.exclude_preview {
        filtri.push_str(" AND model !~* '(preview|experimental|[-_]exp([-_.]|$))'");
    }
    format!(
        "WITH best AS (
            SELECT provider, model FROM (
                SELECT provider, model,
                       row_number() OVER (
                           PARTITION BY provider
                           ORDER BY is_featured DESC,
                                    input_cost_per_million_tokens ASC NULLS LAST
                       ) AS rn
                FROM ai_price_catalog
                {filtri}
            ) t WHERE rn = 1
        )"
    )
}

/// Aggiorna le righe il cui modello corrente NON e' `is_enabled=true` nel catalog,
/// scegliendo il miglior modello agentic-eligibile per provider (featured prima,
/// poi costo crescente). Idempotente: le righe gia' valide non vengono toccate.
pub async fn reconcile_provider_default_models(db: &PgPool) -> Result<u64, sqlx::Error> {
    let gate = crate::orchestrator::qualification_gate(db).await;
    reconcile_with_gate(db, gate).await
}

/// Come [`reconcile_provider_default_models`] ma col gate ESPLICITO. La cache di
/// `qualification_gate` (60s, statica e in-process) renderebbe i test dipendenti
/// dall'ordine di esecuzione (regola F). Stesso pattern del servizio unico.
async fn reconcile_with_gate(
    db: &PgPool,
    gate: crate::orchestrator::QualificationGate,
) -> Result<u64, sqlx::Error> {
    let cte = best_per_provider_cte(gate);

    let default_rows = sqlx::query(&format!(
        r#"
        {cte}
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
        "#
    ))
    .execute(db)
    .await?;

    let settings_rows = sqlx::query(&format!(
        r#"
        {cte}
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
        "#
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// REGRESSIONE (censimento punti unici, fase 3c): questo modulo scrive
    /// configurazione PERSISTENTE che sopravvive al riavvio. Senza gate
    /// "riparava" un default puntandolo a un modello non qualificato o preview —
    /// un guasto silenzioso e duraturo, scritto dal meccanismo che ne ripara un
    /// altro.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_default_non_viene_riparato_verso_un_modello_non_qualificato(pool: PgPool) {
        // Schema REALE (regola O): `ai_price_catalog`, `nexus_provider_default_model`
        // e `settings` arrivano dalla migrazione (mig 0101/0002). Il DELETE isola
        // il test dal catalog reale senza sostituire lo schema; il provider 'p' e'
        // fittizio e non collide con le righe reali di `nexus_provider_default_model`.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // Il default punta a un modello MORTO: va riparato.
        sqlx::query(
            "INSERT INTO nexus_provider_default_model (provider, model_id) VALUES ('p', 'morto')",
        )
        .execute(&pool)
        .await
        .expect("default rotto");
        // Il candidato NON qualificato e' featured e piu' economico: vincerebbe
        // l'ORDER BY della CTE se il gate mancasse.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
              qualification_state, qualification_expires_at, is_featured, \
              input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
              last_probe_healthy_at) VALUES \
             ('p', 'morto',           false, true, 'none', 'qualified',   now() + interval '30 days', false, 1.0, 1.0, 'USD', now()), \
             ('p', 'non-qualificato', true,  true, 'none', 'unqualified', NULL, true,  0.1, 0.1, 'USD', now()), \
             ('p', 'qualificato',     true,  true, 'none', 'qualified',   now() + interval '30 days', false, 5.0, 5.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("catalog");

        let gate = crate::orchestrator::QualificationGate {
            require_qualified: true,
            exclude_preview: true,
        };
        reconcile_with_gate(&pool, gate).await.expect("reconcile");

        let scelto: String =
            sqlx::query_scalar("SELECT model_id FROM nexus_provider_default_model WHERE provider='p'")
                .fetch_one(&pool)
                .await
                .expect("default");
        assert_eq!(
            scelto, "qualificato",
            "col gate acceso il default va riparato SOLO verso un modello \
             qualificato: 'non-qualificato' e' featured e costa 50 volte meno, \
             quindi vincerebbe l'ordinamento — e resterebbe scritto nella \
             configurazione persistente"
        );
    }
}
