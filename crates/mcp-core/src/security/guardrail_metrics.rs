//! Telemetria di controllo dei guard-rail risorse (formato Prometheus text).
//!
//! Appeso all'endpoint esistente `GET /nexus/metrics` (scraped da Prometheus
//! ogni 30s, dashboard Grafana provisioned). Fonti: `nexus_resource_audit`
//! (eventi atomici: blocked/detected/killed/failed) e `service_diagnoses`
//! `signal_kind='policy_violation'` (lifecycle riparazioni). Cache 30s per non
//! pesare sullo scrape; i COUNT totali da DB sono monotoni e validi come
//! counter Prometheus.

use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::RwLock;

const METRICS_CACHE_TTL: Duration = Duration::from_secs(30);

static METRICS_CACHE: Lazy<RwLock<Option<(String, Instant)>>> = Lazy::new(|| RwLock::new(None));

/// Rende il blocco di metriche guardrail (testo Prometheus). Best-effort: su
/// errore DB ritorna un commento, mai un errore (lo scrape non deve fallire).
pub async fn render_guardrail_metrics(db: &PgPool) -> String {
    {
        let guard = METRICS_CACHE.read().await;
        if let Some((text, at)) = guard.as_ref() {
            if at.elapsed() < METRICS_CACHE_TTL {
                return text.clone();
            }
        }
    }

    let mut out = String::with_capacity(2048);

    // Violazioni per (kind, action, outcome) dall'audit.
    out.push_str("# HELP nexus_resource_violations_total Violazioni di governance risorse registrate (audit)\n");
    out.push_str("# TYPE nexus_resource_violations_total counter\n");
    match sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT resource_kind, action, outcome, COUNT(*) \
           FROM nexus_resource_audit \
          WHERE outcome IN ('blocked', 'detected', 'killed', 'failed') \
          GROUP BY resource_kind, action, outcome",
    )
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for (kind, action, outcome, n) in rows {
                out.push_str(&format!(
                    "nexus_resource_violations_total{{kind=\"{kind}\",action=\"{action}\",outcome=\"{outcome}\"}} {n}\n"
                ));
            }
        }
        Err(e) => out.push_str(&format!("# error nexus_resource_audit: {e}\n")),
    }

    // Riparazioni per esito dal lifecycle delle diagnosi.
    out.push_str("# HELP nexus_resource_remediations_total Riparazioni automatiche violazioni risorse per esito\n");
    out.push_str("# TYPE nexus_resource_remediations_total counter\n");
    match sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT \
            COUNT(*) FILTER (WHERE triggered_run_id IS NOT NULL), \
            COUNT(*) FILTER (WHERE status = 'resolved'), \
            COUNT(*) FILTER (WHERE status = 'failed_remediation') \
           FROM service_diagnoses WHERE signal_kind = 'policy_violation'",
    )
    .fetch_one(db)
    .await
    {
        Ok((started, resolved, failed)) => {
            out.push_str(&format!(
                "nexus_resource_remediations_total{{outcome=\"started\"}} {started}\n"
            ));
            out.push_str(&format!(
                "nexus_resource_remediations_total{{outcome=\"resolved\"}} {resolved}\n"
            ));
            out.push_str(&format!(
                "nexus_resource_remediations_total{{outcome=\"failed\"}} {failed}\n"
            ));
        }
        Err(e) => out.push_str(&format!("# error service_diagnoses: {e}\n")),
    }

    // Violazioni aperte adesso (gauge) per classe.
    out.push_str("# HELP nexus_resource_violations_open Violazioni di governance aperte (pannello Problemi)\n");
    out.push_str("# TYPE nexus_resource_violations_open gauge\n");
    match sqlx::query_as::<_, (String, i64)>(
        "SELECT split_part(COALESCE(metric, 'resource/unknown'), '/', 1), COUNT(*) \
           FROM service_diagnoses \
          WHERE signal_kind = 'policy_violation' \
            AND status IN ('open', 'diagnosing', 'failed_remediation') \
          GROUP BY 1",
    )
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            if rows.is_empty() {
                out.push_str("nexus_resource_violations_open{kind=\"port\"} 0\n");
            }
            for (kind, n) in rows {
                out.push_str(&format!(
                    "nexus_resource_violations_open{{kind=\"{kind}\"}} {n}\n"
                ));
            }
        }
        Err(e) => out.push_str(&format!("# error open violations: {e}\n")),
    }

    // Allocazioni porte registrate (gauge).
    out.push_str("# HELP nexus_port_allocations_total Porte allocate nel registro Nexus\n");
    out.push_str("# TYPE nexus_port_allocations_total gauge\n");
    match sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM nexus_port_allocations")
        .fetch_one(db)
        .await
    {
        Ok(n) => out.push_str(&format!("nexus_port_allocations_total {n}\n")),
        Err(e) => out.push_str(&format!("# error nexus_port_allocations: {e}\n")),
    }

    let mut guard = METRICS_CACHE.write().await;
    *guard = Some((out.clone(), Instant::now()));
    out
}
