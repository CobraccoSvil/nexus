//! `governance_telemetry`: PUNTO UNICO I/O (regola L) della TELEMETRIA strutturata
//! per modello, usata dalla governance telemetria-aware (scelta a runtime tra
//! candidati gia' ammissibili, per probabilita' di successo).
//!
//! Confine d'inversione (come `escalation_port`): qui SOLO l'I/O (lettura
//! `ai_model_health_history` + `ai_price_catalog` + snapshot cooldown); la
//! SELEZIONE (ranking) e' il modulo PURO
//! [`nexus_agent_graph::decisions::governance`] (golden-abile in isolamento).
//!
//! Regola M: tutti i segnali sono STRUTTURATI e raccolti alla fonte:
//!   - esiti recenti (`healthy`/`latency_ms`/`error_kind`) dal worker
//!     `model_health_probe` (`ai_model_health_history`, mig 0172);
//!   - contatori `consecutive_failures` / `consecutive_tool_failures` dal catalog
//!     (`ai_price_catalog`, mig 0172/0269);
//!   - `provider_in_cooldown` dal gate ADR 0020 (snapshot in-memory).
//! Nessun parsing del testo umano dell'errore (l'`error_kind` e' gia' la categoria
//! macchina emessa dal probe con la nomenclatura di `provider_error_classifier`).
//!
//! Regola G: flag e soglie sono DB-driven (settings `agent.governance.*`, cache
//! 60s di nexus-auth). Con il master flag OFF (default) il chiamante NON invoca il
//! ranking -> comportamento bit-identico.
//!
//! FAIL-OPEN (sicurezza): qualunque guasto di lettura -> telemetria vuota per i
//! candidati mancanti (punteggio neutro 1.0 nel modulo puro) -> nessun riordino
//! effettivo, MAI un errore che rompe il routing/escalation.

use sqlx::PgPool;

use nexus_agent_graph::decisions::governance::{GovernancePolicy, ModelTelemetry};

use crate::provider_cooldown::is_provider_in_cooldown;

/// Master flag della governance telemetria-aware (escalation + catalog reorder).
/// Default OFF (opt-in, regola G): con OFF il ranking non viene invocato.
const GOVERNANCE_ENABLED_SETTING: &str = "agent.governance.telemetry_aware";
/// Numero di check recenti (per modello) considerati nell'error-rate.
const GOVERNANCE_RECENT_WINDOW_SETTING: &str = "agent.governance.recent_window";
/// Error-rate recente oltre cui un candidato e' "recently_failed" (retrocesso).
const GOVERNANCE_EXCLUDE_ERROR_RATE_SETTING: &str = "agent.governance.exclude_error_rate";
/// Fallimenti consecutivi (o tool) oltre cui un candidato e' "recently_failed".
const GOVERNANCE_EXCLUDE_CONSECUTIVE_SETTING: &str =
    "agent.governance.exclude_consecutive_failures";
/// Check recenti minimi perche' l'error-rate sia affidabile.
const GOVERNANCE_MIN_RECENT_CHECKS_SETTING: &str = "agent.governance.min_recent_checks";
/// Latenza (ms) di riferimento per la penalita' di latenza (tie-breaker).
const GOVERNANCE_LATENCY_REF_SETTING: &str = "agent.governance.latency_ref_ms";
/// Affinita' di tier nel failover: penalita' moltiplicativa per livello di tier
/// sotto quello corrente (`pick_failover_model`). Range valido (0, 1].
const GOVERNANCE_FAILOVER_DOWNGRADE_PENALTY_SETTING: &str =
    "agent.governance.failover_downgrade_penalty";

/// Default della finestra di check recenti (mig 0523). Non un magic fallback su
/// un modello (regola G non si applica): e' un parametro di calcolo locale, come
/// `agentic_min_tier`. Resta configurabile da DB.
const RECENT_WINDOW_DEFAULT: i64 = 10;

/// `true` se il master flag della governance telemetria-aware e' ON. Default OFF
/// (opt-in): setting assente / malformato -> `false` (regola G: nessun fallback
/// che accenda una feature). Cache 60s (nexus-auth). Best-effort: errore DB ->
/// `false` (comportamento storico).
pub async fn governance_enabled(db: &PgPool) -> bool {
    setting_bool(db, GOVERNANCE_ENABLED_SETTING).await
}

/// Legge un setting booleano (`true`/`1`/`yes`/`on` = true), default `false`.
/// Punto unico locale (regola L) della lettura bool della governance.
pub(crate) async fn setting_bool(db: &PgPool, key: &str) -> bool {
    nexus_auth::get_setting(db, key)
        .await
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Costruisce la [`GovernancePolicy`] dai settings DB (regola G). Ogni valore
/// assente/malformato ricade sul default della policy (soglie di calcolo, non
/// magic fallback su un comportamento). Cache 60s. Best-effort.
pub async fn load_governance_policy(db: &PgPool) -> GovernancePolicy {
    let def = GovernancePolicy::default();
    let exclude_error_rate = setting_f64(db, GOVERNANCE_EXCLUDE_ERROR_RATE_SETTING)
        .await
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(def.exclude_error_rate);
    let exclude_consecutive_failures = setting_i64(db, GOVERNANCE_EXCLUDE_CONSECUTIVE_SETTING)
        .await
        .filter(|v| *v > 0)
        .unwrap_or(def.exclude_consecutive_failures);
    let min_recent_checks = setting_i64(db, GOVERNANCE_MIN_RECENT_CHECKS_SETTING)
        .await
        .filter(|v| *v > 0)
        .map(|v| v as u32)
        .unwrap_or(def.min_recent_checks);
    let latency_ref_ms = setting_i64(db, GOVERNANCE_LATENCY_REF_SETTING)
        .await
        .filter(|v| *v > 0)
        .unwrap_or(def.latency_ref_ms);
    let failover_downgrade_penalty = setting_f64(db, GOVERNANCE_FAILOVER_DOWNGRADE_PENALTY_SETTING)
        .await
        .filter(|v| *v > 0.0 && *v <= 1.0)
        .unwrap_or(def.failover_downgrade_penalty);
    GovernancePolicy {
        exclude_error_rate,
        exclude_consecutive_failures,
        min_recent_checks,
        latency_ref_ms,
        failover_downgrade_penalty,
    }
}

/// Finestra di check recenti (per modello) DB-driven, clampata a `[1, 100]`.
async fn recent_window(db: &PgPool) -> i64 {
    setting_i64(db, GOVERNANCE_RECENT_WINDOW_SETTING)
        .await
        .unwrap_or(RECENT_WINDOW_DEFAULT)
        .clamp(1, 100)
}

async fn setting_f64(db: &PgPool, key: &str) -> Option<f64> {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|v| v.trim().parse::<f64>().ok())
}

async fn setting_i64(db: &PgPool, key: &str) -> Option<i64> {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
}

/// Riga grezza della query di telemetria (una per candidato con storico/catalog).
#[derive(Debug, sqlx::FromRow)]
struct TelemetryRow {
    provider: String,
    model: String,
    recent_checks: i64,
    recent_failures: i64,
    avg_latency_ms: i64,
    consecutive_failures: i64,
    consecutive_tool_failures: i64,
    last_error_kind: Option<String>,
}

/// Carica la [`ModelTelemetry`] per l'insieme di candidati `(provider, model)`.
///
/// Un'unica query aggrega gli ultimi `recent_window` check per modello
/// (`ai_model_health_history`) + i contatori del catalog + l'ultimo `error_kind`;
/// `provider_in_cooldown` e' risolto in memoria dal gate (ADR 0020). I candidati
/// SENZA riga (nessuno storico) non compaiono nel risultato: il modulo puro li
/// tratta come telemetria di default (neutra), quindi non vengono penalizzati.
///
/// FAIL-OPEN: errore SQL -> `Vec::new()` (nessuna telemetria -> nessun riordino).
pub async fn load_model_telemetry(
    db: &PgPool,
    candidates: &[(String, String)],
) -> Vec<ModelTelemetry> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let window = recent_window(db).await;
    let providers: Vec<String> = candidates.iter().map(|(p, _)| p.clone()).collect();
    let models: Vec<String> = candidates.iter().map(|(_, m)| m.clone()).collect();

    // UNNEST delle due colonne parallele come insieme di coppie candidate; LEFT
    // JOIN su health-aggregata (ultimi N check) + catalog + ultimo error_kind.
    let rows: Vec<TelemetryRow> = match sqlx::query_as::<_, TelemetryRow>(
        "WITH cand AS ( \
             SELECT provider, model FROM UNNEST($1::text[], $2::text[]) AS c(provider, model) \
         ), \
         recent AS ( \
             SELECT h.provider, h.model, h.healthy, h.latency_ms, \
                    ROW_NUMBER() OVER (PARTITION BY h.provider, h.model ORDER BY h.checked_at DESC) AS rn \
             FROM ai_model_health_history h \
             JOIN cand ON cand.provider = h.provider AND cand.model = h.model \
         ), \
         health AS ( \
             SELECT provider, model, \
                    COUNT(*)::bigint AS recent_checks, \
                    COUNT(*) FILTER (WHERE NOT healthy)::bigint AS recent_failures, \
                    COALESCE(AVG(latency_ms) FILTER (WHERE latency_ms IS NOT NULL), 0)::bigint AS avg_latency_ms \
             FROM recent WHERE rn <= $3 \
             GROUP BY provider, model \
         ), \
         last_err AS ( \
             SELECT DISTINCT ON (h.provider, h.model) h.provider, h.model, h.error_kind \
             FROM ai_model_health_history h \
             JOIN cand ON cand.provider = h.provider AND cand.model = h.model \
             WHERE NOT h.healthy \
             ORDER BY h.provider, h.model, h.checked_at DESC \
         ) \
         SELECT c.provider AS provider, c.model AS model, \
                COALESCE(hh.recent_checks, 0) AS recent_checks, \
                COALESCE(hh.recent_failures, 0) AS recent_failures, \
                COALESCE(hh.avg_latency_ms, 0) AS avg_latency_ms, \
                COALESCE(pc.consecutive_failures, 0)::bigint AS consecutive_failures, \
                COALESCE(pc.consecutive_tool_failures, 0)::bigint AS consecutive_tool_failures, \
                le.error_kind AS last_error_kind \
         FROM cand c \
         LEFT JOIN health hh ON hh.provider = c.provider AND hh.model = c.model \
         LEFT JOIN ai_price_catalog pc ON pc.provider = c.provider AND pc.model = c.model \
         LEFT JOIN last_err le ON le.provider = c.provider AND le.model = c.model \
         WHERE hh.provider IS NOT NULL OR pc.provider IS NOT NULL",
    )
    .bind(&providers)
    .bind(&models)
    .bind(window)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "nexus_governance",
                error = %e,
                "governance_telemetry: lettura telemetria fallita, fail-open (nessun riordino)"
            );
            return Vec::new();
        }
    };

    rows.into_iter()
        .map(|r| ModelTelemetry {
            provider: r.provider.to_ascii_lowercase(),
            model: r.model,
            recent_checks: r.recent_checks.max(0) as u32,
            recent_failures: r.recent_failures.max(0) as u32,
            avg_latency_ms: r.avg_latency_ms.max(0),
            consecutive_failures: r.consecutive_failures.max(0),
            consecutive_tool_failures: r.consecutive_tool_failures.max(0),
            last_error_kind: r.last_error_kind.filter(|k| !k.trim().is_empty()),
            // Gate ADR 0020 in memoria (regola L: fonte unica del cooldown).
            provider_in_cooldown: is_provider_in_cooldown(&r.provider),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema minimale usato da `load_model_telemetry`: catalog (contatori) +
    /// storico health (esiti recenti) + settings (finestra, assente -> default).
    async fn create_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, category TEXT)",
        )
        .execute(pool)
        .await
        .expect("create settings");
        sqlx::query(
            "CREATE TABLE ai_price_catalog ( \
                 provider TEXT NOT NULL, model TEXT NOT NULL, \
                 consecutive_failures INT NOT NULL DEFAULT 0, \
                 consecutive_tool_failures INT NOT NULL DEFAULT 0 )",
        )
        .execute(pool)
        .await
        .expect("create ai_price_catalog");
        sqlx::query(
            "CREATE TABLE ai_model_health_history ( \
                 provider TEXT NOT NULL, model TEXT NOT NULL, healthy BOOLEAN NOT NULL, \
                 latency_ms INT, error_kind TEXT, checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
        )
        .execute(pool)
        .await
        .expect("create ai_model_health_history");
    }

    /// La query aggrega gli ultimi N check per modello (recent_checks/failures/
    /// avg_latency), l'ultimo error_kind fallito, e i contatori del catalog; i
    /// candidati SENZA storico ne' catalog NON compaiono (telemetria neutra a valle).
    #[sqlx::test]
    async fn load_model_telemetry_aggrega_health_e_catalog(pool: PgPool) {
        create_schema(&pool).await;
        // pa/ma: 3 check (1 ok + 2 falliti), latenze 100/200/NULL, ultimo fallito
        // = rate_limit; catalog consecutive_failures=1.
        sqlx::query(
            "INSERT INTO ai_model_health_history (provider, model, healthy, latency_ms, error_kind, checked_at) VALUES \
             ('pa','ma', true,  100, NULL,             NOW() - INTERVAL '3 minutes'), \
             ('pa','ma', false, 200, 'model_not_found', NOW() - INTERVAL '2 minutes'), \
             ('pa','ma', false, NULL,'rate_limit',      NOW() - INTERVAL '1 minutes')",
        )
        .execute(&pool)
        .await
        .expect("insert health pa");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, consecutive_failures, consecutive_tool_failures) VALUES \
             ('pa','ma', 1, 0), ('pb','mb', 5, 2)",
        )
        .execute(&pool)
        .await
        .expect("insert catalog");

        let candidates = vec![
            ("pa".to_string(), "ma".to_string()),
            ("pb".to_string(), "mb".to_string()),
            ("pc".to_string(), "mc".to_string()), // ne' storico ne' catalog
        ];
        let tel = load_model_telemetry(&pool, &candidates).await;

        // pc/mc assente (nessuna fonte) -> non compare.
        assert_eq!(tel.len(), 2, "solo i candidati con storico o catalog");

        let pa = tel
            .iter()
            .find(|t| t.model == "ma")
            .expect("pa/ma presente");
        assert_eq!(pa.recent_checks, 3);
        assert_eq!(pa.recent_failures, 2);
        assert_eq!(pa.avg_latency_ms, 150); // avg(100, 200), NULL escluso
        assert_eq!(pa.consecutive_failures, 1);
        assert_eq!(pa.last_error_kind.as_deref(), Some("rate_limit")); // ultimo fallito
        assert!(!pa.provider_in_cooldown);

        let pb = tel
            .iter()
            .find(|t| t.model == "mb")
            .expect("pb/mb presente");
        assert_eq!(pb.recent_checks, 0); // nessuno storico health
        assert_eq!(pb.consecutive_failures, 5);
        assert_eq!(pb.consecutive_tool_failures, 2);
        assert_eq!(pb.last_error_kind, None);
    }

    /// Lista candidati vuota -> nessuna query, vettore vuoto (fail-fast).
    #[sqlx::test]
    async fn load_model_telemetry_candidati_vuoti(pool: PgPool) {
        create_schema(&pool).await;
        assert!(load_model_telemetry(&pool, &[]).await.is_empty());
    }

    /// `governance_enabled` default OFF: setting assente -> false (opt-in, regola G).
    #[sqlx::test]
    async fn governance_enabled_default_off(pool: PgPool) {
        create_schema(&pool).await;
        assert!(!governance_enabled(&pool).await);
    }
}
