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

/// Progetti con attivita' recente (sessione chat o agent_processes recenti).
/// Evita di emettere metriche per progetti dormienti.
///
/// Separazione DB per-progetto (regola G/L): `chat_sessions` e `agent_processes`
/// sono domini MIGRATI (vedi `db/migrations/project/0001_chat.sql`,
/// `0002_run.sql`), quindi vivono nel DB del progetto e NON si possono
/// interrogare dal meta con una JOIN su `projects`. Iteriamo i progetti
/// (`list_all_project_ids`, tabella globale meta) e sondiamo l'attivita' sul pool
/// di CIASCUNO (`project_data_pool_from`) — stesso pattern del `task_watchdog`
/// per i processi orfani. Query per-pool best-effort: un errore (pool non
/// disponibile o query fallita) degrada a "non attivo", mai rompe il round.
async fn recent_active_projects(
    meta: &sqlx::PgPool,
    idle_minutes: i64,
) -> Result<Vec<(Uuid, String)>, String> {
    // Elenco (id + slug) dalla tabella globale `projects` (meta-DB).
    let projects = sqlx::query(
        r#"
        SELECT id AS project_id,
               LOWER(REPLACE(REPLACE(name, ' ', '-'), '_', '-')) AS slug
          FROM projects
        "#,
    )
    .fetch_all(meta)
    .await
    .map_err(|e| e.to_string())?;

    let idle = idle_minutes.to_string();
    let mut active: Vec<(Uuid, String)> = Vec::new();
    for row in projects {
        let project_id: Uuid = row.get("project_id");
        let slug: String = row.get("slug");
        // Pool dove risiedono i dati vivi del progetto. Non disponibile ->
        // progetto considerato non attivo per questo giro (WARN + skip).
        let Some(pool) = crate::project_db_routes::project_data_pool_or_warn(
            meta,
            project_id,
            "recent_active_projects",
        )
        .await
        else {
            continue;
        };
        if project_has_recent_activity(&pool, project_id, idle.as_str()).await {
            active.push((project_id, slug));
            if active.len() >= 50 {
                break;
            }
        }
    }
    Ok(active)
}

/// True se il progetto mostra attivita' recente (sessioni chat aggiornate o
/// processi agente vivi) entro `idle_minutes`. Sul DB del progetto; errore
/// query -> false (il progetto risulta non attivo per questo giro).
async fn project_has_recent_activity(pool: &sqlx::PgPool, project_id: Uuid, idle: &str) -> bool {
    sqlx::query_scalar(
        r#"
        SELECT
            EXISTS (
                SELECT 1 FROM chat_sessions
                 WHERE project_id = $1
                   AND updated_at > NOW() - ($2::text || ' minutes')::interval
            )
            OR EXISTS (
                SELECT 1 FROM agent_processes
                 WHERE project_id = $1
                   AND COALESCE(stopped_at, created_at) > NOW() - ($2::text || ' minutes')::interval
            )
        "#,
    )
    .bind(project_id)
    .bind(idle)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

/// Calcola le KPI di default. Tutto best-effort: se una sorgente fallisce, la
/// metrica relativa viene comunque emessa col valore "—" cosi' l'utente vede
/// lo slot e non rimane con il pannello vuoto.
async fn compute_metrics(
    state: &AppState,
    project_id: Uuid,
    slug: &str,
) -> Vec<(&'static str, serde_json::Value, &'static str)> {
    let services = count_services(state, project_id, slug).await;
    let containers = count_containers_up(slug).await;
    let ports = count_ports_used(state, project_id).await;
    let problems = count_problems_open(&state.db, project_id)
        .await
        .unwrap_or(0);
    let usage = read_usage_24h(&state.db, project_id).await;
    let runs_24h = count_agent_runs_24h(&state.db, project_id)
        .await
        .unwrap_or(0);
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
    let ports_label = format!("{}/{}", ports, super::services::PROJECT_PORT_BUCKET_SIZE);
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
    let model_label = primary_model.as_deref().unwrap_or("—").to_string();

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
    // Separazione DB per-progetto: agent_runs e' una tabella migrata, instradiamo
    // sul pool del progetto (errore propagato: il chiamante degrada la metrica).
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs \
         WHERE project_id = $1 AND created_at > NOW() - INTERVAL '24 hours'",
    )
    .bind(project_id)
    .fetch_one(&proj_pool)
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
// Su Windows la variante `InstalledOnly` non viene mai costruita (nessun probe
// del bus systemd: il conteggio da agent_processes e' sempre esatto), ma resta
// nel match del chiamante: silenzia il lint "variant never constructed".
#[cfg_attr(windows, allow(dead_code))]
enum ServiceCount {
    Counted { active: usize, installed: usize },
    InstalledOnly { installed: usize }, // manager non rispondeva
}

/// `(active, installed)` per gli unit `{slug}-*.service`. Conta gli installati
/// scandagliando i file in `~/.config/systemd/user/` e i "active" via systemctl
/// (se manager attivo); se systemd-user non c'e' (WSL/detached) ritorna
/// `InstalledOnly` per evitare il fuorviante "0/N".
///
/// Ramo Linux: sorgente di verita' i file unit systemd `--user`.
#[cfg(unix)]
async fn count_services(_state: &AppState, _project_id: Uuid, slug: &str) -> Option<ServiceCount> {
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

/// Ramo Windows: niente systemd `--user`. I servizi di progetto sono tracciati
/// nella tabella `agent_processes` (kind='service'), sorgente di verita' del
/// service manager Nexus su Windows. Si contano le label distinte (installati) e
/// quelle in stato `running`/`starting` (attivi), instradando sul pool dati del
/// progetto (regola separazione DB per-progetto). Errore DB -> None, che il
/// chiamante degrada a "—" senza tentare path Linux.
#[cfg(windows)]
async fn count_services(state: &AppState, project_id: Uuid, _slug: &str) -> Option<ServiceCount> {
    // DB progetto non disponibile -> None (il chiamante mostra "—"), con WARN.
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "count_services: DB progetto non disponibile, conteggio non osservabile"
                );
                return None;
            }
        };
    // COUNT DISTINCT label evita di gonfiare i numeri con lo storico (piu' righe
    // per stessa label, ORDER BY created_at DESC nel resto del modulo).
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT \
             COUNT(DISTINCT label) AS installed, \
             COUNT(DISTINCT label) FILTER (WHERE status IN ('running', 'starting')) AS active \
         FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service'",
    )
    .bind(project_id)
    .fetch_optional(&proj_pool)
    .await
    .ok()
    .flatten();
    let (installed, active) = row?;
    Some(ServiceCount::Counted {
        active: active.max(0) as usize,
        installed: installed.max(0) as usize,
    })
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
    // Stesso filtro del pannello (logs.rs): le violazioni di governance
    // restano contate anche in 'diagnosing'/'failed_remediation'.
    let diag: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 \
           AND (status = 'open' \
                OR (signal_kind = 'policy_violation' \
                    AND status IN ('diagnosing', 'failed_remediation')))",
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
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
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
