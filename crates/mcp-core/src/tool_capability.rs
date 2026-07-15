//! Punto unico (regola L, ADR 0024) delle SCRITTURE di tool-capability su
//! `ai_price_catalog`: incremento del counter `consecutive_tool_failures`,
//! degrado a soglia (`supports_tool_use=false`) e ripristino dopo un successo.
//!
//! Tutti i writer automatici — il tool-probe (`model_health_probe`) e il
//! tracking runtime dei run agentici (`chat_messages::agent_run`) — DEVONO
//! passare da qui. Il guard `NOT capability_locked` (le righe con lock
//! esplicito dell'admin non vengono mai degradate dagli automatismi) e' cosi'
//! applicato in un solo posto: l'incidente deepseek-v4 (2026-06-10) nacque
//! proprio dal runtime che degradava righe curate senza guard, mentre il
//! probe lo aveva. Il lock e' una colonna DEDICATA (`capability_locked`, mig
//! 0590), separata dalla provenienza `capability_source`: prima il guard era
//! `capability_source='auto'` e ogni riga dichiarata a mano diventava
//! INFALSIFICABILE per sempre (fase 2 design gate qualificazione, ANELLO 4).
//!
//! Il ripristino e' simmetrico tra le due fonti di degrado: un successo con
//! tool (run reale o probe) riabilita la capability qualunque sia stata la
//! fonte (`malformed_tool_calls` dal runtime, `tool_probe_failed:%` dal probe).
//! Senza questa simmetria un degrado runtime non era mai riabilitabile dal
//! probe (e viceversa): catch-22 con degrado permanente.

use sqlx::PgPool;

/// Reason scritto quando il degrado viene dal tracking runtime dei run reali.
pub const REASON_MALFORMED_TOOL_CALLS: &str = "malformed_tool_calls";
/// Prefisso del reason scritto dal tool-probe.
pub const REASON_TOOL_PROBE_PREFIX: &str = "tool_probe_failed:";

/// Predicato SQL che riconosce un `auto_disabled_reason` appartenente al
/// ciclo TOOL-CAPABILITY (writer automatici: runtime e tool-probe), distinto
/// dai reason del ciclo `is_enabled` (missing_from_api, billing_cooldown,
/// hollow_completion_runtime, ...). I call site che azzerano il reason nel
/// ciclo is_enabled (es. il re-enable del catalog_sync) DEVONO preservare
/// questi valori, altrimenti producono righe ORFANE (supports_tool_use=false
/// con reason NULL) che il gate di ri-test non riaggancia piu'.
pub const TOOL_REASON_PREDICATE_SQL: &str = "(auto_disabled_reason = 'malformed_tool_calls' \
     OR auto_disabled_reason LIKE 'tool_probe_failed:%')";

/// Equivalente Rust-side di [`TOOL_REASON_PREDICATE_SQL`].
pub fn is_tool_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|r| {
        r == REASON_MALFORMED_TOOL_CALLS || r.starts_with(REASON_TOOL_PROBE_PREFIX)
    })
}

/// Punto unico del criterio "riga degradata dagli automatismi, il tool-probe
/// deve ri-testarla". Copre i reason dei due writer automatici (runtime e
/// probe) E il caso ORFANO `reason NULL con counter > 0`: un re-enable esterno
/// (catalog_sync "ricomparso API", ciclo billing cooldown) puo' azzerare
/// `auto_disabled_reason` senza ripristinare `supports_tool_use` — senza
/// questo ramo il degrado diventa permanente (incidente magistral-small-2509,
/// 2026-06-10). Le righe con lock esplicito (`capability_locked`, mig 0590)
/// restano fuori dal ri-test automatico.
pub fn was_auto_degraded(
    supports_tool_use: bool,
    capability_locked: bool,
    reason: Option<&str>,
    consecutive_tool_failures: i32,
) -> bool {
    !supports_tool_use
        && !capability_locked
        && (is_tool_reason(reason) || (reason.is_none() && consecutive_tool_failures > 0))
}

/// Esito di [`record_tool_failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFailureRecord {
    /// Riga con lock esplicito (`capability_locked`) o inesistente: nessuna scrittura.
    Protected,
    /// Counter incrementato, sotto soglia.
    Counted { failures: i32 },
    /// Soglia raggiunta: `supports_tool_use=false` (is_enabled NON toccato).
    MarkedNonToolCapable { failures: i32 },
}

/// Registra un tool-failure (MALFORMED / output vuoto sul tool-forcing) per il
/// modello: incrementa `consecutive_tool_failures` e, a soglia, marca
/// `supports_tool_use=false` con `auto_disabled_reason=reason`.
///
/// Guard `NOT capability_locked` (mig 0590): le righe con lock esplicito non
/// vengono ne' contate ne' degradate (ritorna [`ToolFailureRecord::Protected`]).
pub async fn record_tool_failure(
    db: &PgPool,
    provider: &str,
    model: &str,
    threshold: i32,
    reason: &str,
) -> ToolFailureRecord {
    let new_count: Option<i32> = sqlx::query_scalar(
        "UPDATE ai_price_catalog
            SET consecutive_tool_failures = consecutive_tool_failures + 1,
                updated_at = NOW()
          WHERE provider = $1 AND model = $2
            AND NOT capability_locked
      RETURNING consecutive_tool_failures",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(failures) = new_count else {
        tracing::debug!(
            "tool_capability: {provider}/{model} riga lockata o assente — tool-failure non contato"
        );
        return ToolFailureRecord::Protected;
    };

    // Decisione a soglia delegata alla funzione pura testata (agent_types).
    let action =
        crate::agent_types::tool_failure_action(true, true, true, false, failures - 1, threshold);
    if matches!(
        action,
        crate::agent_types::ToolCapabilityAction::MarkNonToolCapable
    ) {
        // Oltre alle righe ancora true, riscrive il reason sulle righe gia'
        // false ma ORFANE (reason NULL, azzerato da un re-enable esterno):
        // cosi' il prossimo round del probe le riaggancia via was_auto_degraded.
        let _ = sqlx::query(
            "UPDATE ai_price_catalog
                SET supports_tool_use = false,
                    auto_disabled_reason = $3,
                    updated_at = NOW()
              WHERE provider = $1 AND model = $2
                AND NOT capability_locked
                AND (supports_tool_use = true OR auto_disabled_reason IS NULL)",
        )
        .bind(provider)
        .bind(model)
        .bind(reason)
        .execute(db)
        .await;
        tracing::warn!(
            "tool_capability: MARK NON-TOOL-CAPABLE {provider}/{model} dopo {failures} \
             tool-failure consecutivi (reason={reason}). is_enabled invariato: resta per chat."
        );
        ToolFailureRecord::MarkedNonToolCapable { failures }
    } else {
        ToolFailureRecord::Counted { failures }
    }
}

/// Ripristina lo stato tool dopo un successo con tool: azzera
/// `consecutive_tool_failures` e riabilita `supports_tool_use` se il degrado
/// era automatico — reason `malformed_tool_calls` / `tool_probe_failed:%`,
/// QUALUNQUE sia stata la fonte, oppure riga ORFANA (false con reason NULL e
/// counter > 0, prodotta da un re-enable esterno che ha azzerato il reason).
/// Le curature admin (`capability_source='manual'`, o false senza counter per
/// scelta del classify) non vengono riabilitate.
///
/// `also_generic` (solo runtime): azzera anche `consecutive_failures` e
/// `auto_disabled_at` (counter hollow generico che governa is_enabled).
pub async fn reset_tool_failures_on_success(
    db: &PgPool,
    provider: &str,
    model: &str,
    also_generic: bool,
) {
    let auto_reason = TOOL_REASON_PREDICATE_SQL;
    let orphan_degrade = "(supports_tool_use = false AND auto_disabled_reason IS NULL \
                           AND consecutive_tool_failures > 0)";
    let generic_sets = if also_generic {
        "consecutive_failures = 0, auto_disabled_at = NULL,"
    } else {
        ""
    };
    let generic_where = if also_generic {
        "OR consecutive_failures > 0"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE ai_price_catalog
            SET {generic_sets}
                consecutive_tool_failures = 0,
                supports_tool_use = CASE WHEN {auto_reason} OR {orphan_degrade} THEN true
                                         ELSE supports_tool_use END,
                auto_disabled_reason = CASE WHEN {auto_reason} THEN NULL
                                            ELSE auto_disabled_reason END,
                updated_at = NOW()
          WHERE provider = $1 AND model = $2
            AND (consecutive_tool_failures > 0 {generic_where} OR {auto_reason})"
    );
    let _ = sqlx::query(&sql)
        .bind(provider)
        .bind(model)
        .execute(db)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_tool_reason_riconosce_i_writer_automatici() {
        assert!(is_tool_reason(Some("malformed_tool_calls")));
        assert!(is_tool_reason(Some("tool_probe_failed:error")));
        assert!(!is_tool_reason(Some("missing_from_api")));
        assert!(!is_tool_reason(Some("hollow_completion_runtime")));
        assert!(!is_tool_reason(None));
    }

    #[test]
    fn was_auto_degraded_copre_reason_dei_writer() {
        assert!(was_auto_degraded(
            false,
            false,
            Some("malformed_tool_calls"),
            0
        ));
        assert!(was_auto_degraded(
            false,
            false,
            Some("tool_probe_failed:timeout"),
            0
        ));
    }

    #[test]
    fn was_auto_degraded_riaggancia_le_righe_orfane() {
        // Regressione incidente magistral-small-2509 (2026-06-10): re-enable
        // esterno azzera il reason lasciando false + counter > 0. Il ri-test
        // DEVE riagganciare la riga, altrimenti il degrado e' permanente.
        assert!(was_auto_degraded(false, false, None, 4));
    }

    #[test]
    fn was_auto_degraded_non_tocca_locked_ne_false_by_design() {
        // Lock esplicito (mig 0590): mai ri-testato in automatico.
        assert!(!was_auto_degraded(false, true, None, 4));
        assert!(!was_auto_degraded(
            false,
            true,
            Some("malformed_tool_calls"),
            4
        ));
        // false dal classify senza alcun fallimento registrato: non e' un
        // degrado automatico, niente ri-test.
        assert!(!was_auto_degraded(false, false, None, 0));
        // Reason di un ciclo diverso (is_enabled): non e' un degrado tool.
        assert!(!was_auto_degraded(
            false,
            false,
            Some("missing_from_api"),
            2
        ));
        // Riga ancora tool-capable: il gate principale la testa gia'.
        assert!(!was_auto_degraded(true, false, None, 0));
    }

    /// Mig 0590: il guard del ciclo tool-capability e' il lock ESPLICITO
    /// (`capability_locked`), non piu' la provenienza `capability_source`.
    #[sqlx::test]
    async fn record_tool_failure_rispetta_il_lock(pool: sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE ai_price_catalog ( \
                 provider TEXT NOT NULL, \
                 model TEXT NOT NULL, \
                 consecutive_tool_failures INT NOT NULL DEFAULT 0, \
                 supports_tool_use BOOLEAN NOT NULL DEFAULT true, \
                 auto_disabled_reason TEXT, \
                 capability_locked BOOLEAN NOT NULL DEFAULT false, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
        )
        .execute(&pool)
        .await
        .expect("schema mock");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, capability_locked) \
             VALUES ('p', 'locked', true), ('p', 'free', false)",
        )
        .execute(&pool)
        .await
        .expect("seed");

        // Riga con lock esplicito: nessun conteggio, nessun degrado.
        assert_eq!(
            record_tool_failure(&pool, "p", "locked", 3, REASON_MALFORMED_TOOL_CALLS).await,
            ToolFailureRecord::Protected
        );
        // Riga senza lock: il fallimento viene contato.
        assert_eq!(
            record_tool_failure(&pool, "p", "free", 3, REASON_MALFORMED_TOOL_CALLS).await,
            ToolFailureRecord::Counted { failures: 1 }
        );
    }
}
