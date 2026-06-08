//! Seed automatico del pannello Monitor con metriche di default del progetto.
//!
//! Prima il Monitor era opt-in: si popolava SOLO se l'agente chiamava il tool
//! `dispatcher_update_monitor`. In pratica i modelli non lo facevano mai e il
//! pannello restava vuoto ("Nessun monitor attivo"). Questo worker emette un
//! insieme minimo di KPI utili senza intervento dell'agente, riusando il punto
//! unico `agent_tools::monitor::set_monitor` (regola L).
//!
//! Metriche emesse per ogni progetto con attivita' recente:
//!   - svc_active   : "N/M servizi attivi"     (N=active, M=installati)
//!   - containers_up: "N container"             (docker ps filtrato per slug)
//!   - ports_used   : "N/50 porte bucket"       (allocate / dimensione bucket)
//!   - problems_open: numero issue aperte       (project_runtime_issues + service_diagnoses)
//!
//! Config DB-driven (regola G):
//!   - monitor.seed.enabled                (true)
//!   - monitor.seed.poll_seconds           (30)
//!   - monitor.seed.idle_minutes           (60)  — un progetto e' "attivo" se ha
//!     eventi/sessioni o dispatcher subscribers nell'ultima ora.

use std::time::Duration;

use sqlx::Row;
use tokio::time::sleep;
use uuid::Uuid;

use crate::agent_tools::monitor::set_monitor;
use crate::AppState;

const STARTUP_DELAY_S: u64 = 20;

pub fn spawn_monitor_seed_worker(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        loop {
            let enabled = load_bool(&state, "monitor.seed.enabled", true).await;
            let poll = load_u64(&state, "monitor.seed.poll_seconds", 30, 10).await;
            if enabled {
                if let Err(e) = run_one_round(&state).await {
                    tracing::warn!("monitor_seed: round fallito: {e}");
                }
            }
            sleep(Duration::from_secs(poll)).await;
        }
    });
    tracing::info!("monitor_seed worker: avviato (pannello Monitor popolato con KPI di default)");
}

async fn run_one_round(state: &AppState) -> Result<(), String> {
    let idle_min = load_u64(state, "monitor.seed.idle_minutes", 60, 5).await as i64;
    let projects = recent_active_projects(&state.db, idle_min).await?;
    for (project_id, slug) in projects {
        let metrics = compute_metrics(state, project_id, &slug).await;
        for (id, value, label) in metrics {
            set_monitor(
                &state.monitor_registry,
                &state.project_channels,
                project_id,
                id,
                value,
                Some(label.to_string()),
            );
        }
    }
    Ok(())
}

/// Progetti con attivita' recente (sessione chat, dispatcher events o servizi
/// installati). Evita di emettere metriche per progetti dormienti.
async fn recent_active_projects(
    db: &sqlx::PgPool,
    idle_minutes: i64,
) -> Result<Vec<(Uuid, String)>, String> {
    let rows = sqlx::query(
        r#"
        SELECT p.id AS project_id,
               LOWER(REPLACE(REPLACE(p.name, ' ', '-'), '_', '-')) AS slug
          FROM projects p
         WHERE EXISTS (
                   SELECT 1 FROM chat_sessions s
                    WHERE s.project_id = p.id
                      AND s.updated_at > NOW() - ($1::text || ' minutes')::interval
               )
            OR EXISTS (
                   SELECT 1 FROM agent_processes a
                    WHERE a.project_id = p.id
                      AND COALESCE(a.stopped_at, a.created_at) > NOW() - ($1::text || ' minutes')::interval
               )
         LIMIT 50
        "#,
    )
    .bind(idle_minutes.to_string())
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("project_id"),
                r.get::<String, _>("slug"),
            )
        })
        .collect())
}

/// Calcola le KPI di default. Tutto best-effort: se una sorgente fallisce, la
/// metrica relativa viene comunque emessa col valore "—" cosi' l'utente vede
/// lo slot e non rimane con il pannello vuoto.
async fn compute_metrics(
    state: &AppState,
    project_id: Uuid,
    slug: &str,
) -> Vec<(&'static str, serde_json::Value, &'static str)> {
    let services = count_services(slug).await;
    let containers = count_containers_up(slug).await;
    let ports = count_ports_used(state, project_id).await;
    let problems = count_problems_open(&state.db, project_id).await.unwrap_or(0);
    let usage = read_usage_24h(&state.db, project_id).await;
    let runs_24h = count_agent_runs_24h(&state.db, project_id).await.unwrap_or(0);
    let primary_model = read_primary_model(&state.db, project_id).await;

    let svc_label = match services {
        Some(ServiceCount::Counted { active, installed }) => format!("{active}/{installed}"),
        // Manager non disponibile (WSL detached): containers_up copre l'info reale,
        // qui mostriamo "—/N" per non fingere uno stato che non possiamo osservare.
        Some(ServiceCount::InstalledOnly { installed }) => format!("—/{installed}"),
        None => "—".to_string(),
    };
    let containers_label = match containers {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    };
    let ports_label = format!(
        "{}/{}",
        ports,
        super::services::PROJECT_PORT_BUCKET_SIZE
    );
    // Token: scala a "k"/"M" per leggibilita' a colpo d'occhio.
    let tokens_label = match usage {
        Some(ref u) if u.total_tokens >= 1_000_000 => {
            format!("{:.1}M", u.total_tokens as f64 / 1_000_000.0)
        }
        Some(ref u) if u.total_tokens >= 1_000 => {
            format!("{:.1}k", u.total_tokens as f64 / 1_000.0)
        }
        Some(ref u) => u.total_tokens.to_string(),
        None => "—".to_string(),
    };
    // Costo: USD con 2-4 decimali, leggibile in colpo d'occhio.
    let cost_label = match usage {
        Some(ref u) if u.total_cost_usd >= 1.0 => format!("${:.2}", u.total_cost_usd),
        Some(ref u) if u.total_cost_usd > 0.0 => format!("${:.4}", u.total_cost_usd),
        Some(_) => "$0".to_string(),
        None => "—".to_string(),
    };
    let model_label = primary_model
        .as_deref()
        .unwrap_or("—")
        .to_string();

    vec![
        (
            "nexus.svc_active",
            serde_json::Value::String(svc_label),
            "Servizi attivi",
        ),
        (
            "nexus.containers_up",
            serde_json::Value::String(containers_label),
            "Container up",
        ),
        (
            "nexus.ports_used",
            serde_json::Value::String(ports_label),
            "Porte bucket",
        ),
        (
            "nexus.problems_open",
            serde_json::Value::Number(serde_json::Number::from(problems)),
            "Problemi aperti",
        ),
        (
            "nexus.tokens_24h",
            serde_json::Value::String(tokens_label),
            "Token 24h",
        ),
        (
            "nexus.cost_24h",
            serde_json::Value::String(cost_label),
            "Costo 24h",
        ),
        (
            "nexus.agent_runs_24h",
            serde_json::Value::Number(serde_json::Number::from(runs_24h)),
            "Run agente 24h",
        ),
        (
            "nexus.model_primary",
            serde_json::Value::String(model_label),
            "Modello primario",
        ),
    ]
}

#[derive(Debug, Clone, Copy)]
struct UsageWindow {
    total_tokens: i64,
    total_cost_usd: f64,
}

/// Token e costo cumulativi nelle ultime 24h (ai_usage_ledger finalized).
/// Punto unico: la stessa fonte usata dal pannello Billing per la dashboard.
async fn read_usage_24h(db: &sqlx::PgPool, project_id: Uuid) -> Option<UsageWindow> {
    let row = sqlx::query(
        r#"
        SELECT COALESCE(SUM(total_tokens), 0)::BIGINT AS tokens,
               COALESCE(SUM(total_cost), 0)::FLOAT8   AS cost_usd
          FROM ai_usage_ledger
         WHERE project_id = $1
           AND status = 'finalized'
           AND created_at > NOW() - INTERVAL '24 hours'
        "#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    Some(UsageWindow {
        total_tokens: row.try_get::<i64, _>("tokens").unwrap_or(0),
        total_cost_usd: row.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
    })
}

/// Numero di agent_runs creati nelle ultime 24h per il progetto.
async fn count_agent_runs_24h(db: &sqlx::PgPool, project_id: Uuid) -> Result<i64, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs \
         WHERE project_id = $1 AND created_at > NOW() - INTERVAL '24 hours'",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())
}

/// Modello AI piu' usato nelle ultime 24h (per # chiamate). Se non c'e' nulla
/// nelle ultime 24h, ricade al "modello dell'ultimo run" del progetto.
/// Formato: "provider/model" (es. "deepseek/deepseek-v4-pro").
async fn read_primary_model(db: &sqlx::PgPool, project_id: Uuid) -> Option<String> {
    let recent: Option<(String, String)> = sqlx::query_as(
        r#"
        SELECT provider, model
          FROM ai_usage_ledger
         WHERE project_id = $1
           AND created_at > NOW() - INTERVAL '24 hours'
         GROUP BY provider, model
         ORDER BY COUNT(*) DESC
         LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if let Some((p, m)) = recent {
        return Some(format!("{p}/{m}"));
    }
    // Fallback: ultimo run del progetto qualunque sia la data.
    let last: Option<(String, String)> = sqlx::query_as(
        r#"
        SELECT provider, model
          FROM ai_usage_ledger
         WHERE project_id = $1
         ORDER BY created_at DESC
         LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    last.map(|(p, m)| format!("{p}/{m}"))
}

/// Esito conta servizi. Distingue il caso "manager attivo" (numeri esatti)
/// dal caso "manager non disponibile" (label "—/N" onesto: lo stato detached
/// non e' osservabile via systemctl, ma `containers_up` espone gia' i container
/// docker reali, quindi non mentiamo dicendo "0/N").
enum ServiceCount {
    Counted { active: usize, installed: usize },
    InstalledOnly { installed: usize }, // manager non rispondeva
}

/// `(active, installed)` per gli unit `{slug}-*.service`. Conta gli installati
/// scandagliando i file in `~/.config/systemd/user/` e i "active" via systemctl
/// (se manager attivo); se systemd-user non c'e' (WSL/detached) ritorna
/// `InstalledOnly` per evitare il fuorviante "0/N".
async fn count_services(slug: &str) -> Option<ServiceCount> {
    let home = std::env::var("HOME").ok()?;
    let dir = format!("{home}/.config/systemd/user");
    let prefix = format!("{slug}-");
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    let mut installed = 0usize;
    let mut units: Vec<String> = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        if let Some(name) = e.file_name().to_str() {
            if name.starts_with(&prefix) && name.ends_with(".service") {
                installed += 1;
                units.push(name.to_string());
            }
        }
    }
    // Sondiamo il manager con un'invocazione neutra: se non risponde / risponde
    // "Failed to connect to bus" significa che systemd --user non e' attivo
    // (tipico WSL). In quel caso il count "active" non e' osservabile da qui
    // (il wizard usa setsid+nohup, senza unit) -> InstalledOnly.
    let probe = tokio::process::Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .await
        .ok();
    let manager_ok = probe
        .as_ref()
        .map(|o| !String::from_utf8_lossy(&o.stderr).contains("Failed to connect to bus"))
        .unwrap_or(false);
    if !manager_ok {
        return Some(ServiceCount::InstalledOnly { installed });
    }
    let mut active = 0usize;
    for u in &units {
        let out = tokio::process::Command::new("systemctl")
            .args(["--user", "is-active", u])
            .output()
            .await
            .ok();
        if let Some(o) = out {
            if String::from_utf8_lossy(&o.stdout).trim() == "active" {
                active += 1;
            }
        }
    }
    Some(ServiceCount::Counted { active, installed })
}

/// Numero di container Docker del progetto in stato Up (filtro `name={slug}-`).
async fn count_containers_up(slug: &str) -> Option<usize> {
    let out = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name={slug}-"),
            "--filter",
            "status=running",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Numero di porte attualmente allocate al progetto nel registro.
async fn count_ports_used(state: &AppState, project_id: Uuid) -> usize {
    let reg = state.port_registry.current().await;
    reg.ports_for_project(&project_id).len()
}

/// Numero di problemi aperti aggregati (runtime_issues + service_diagnoses).
/// Stesso scope del pannello "Problemi" (logs.rs).
async fn count_problems_open(db: &sqlx::PgPool, project_id: Uuid) -> Result<i64, String> {
    let runtime: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_runtime_issues \
         WHERE project_id = $1 AND status IN ('open','in_progress')",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let diag: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND status = 'open'",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0);
    Ok(runtime + diag)
}

async fn load_bool(state: &AppState, key: &str, default: bool) -> bool {
    crate::settings::get_setting(&state.db, key)
        .await
        .ok()
        .flatten()
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(default)
}

async fn load_u64(state: &AppState, key: &str, default: u64, min: u64) -> u64 {
    crate::settings::get_setting(&state.db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .max(min)
}
