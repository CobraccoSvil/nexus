//! Task Watchdog — monitoraggio centralizzato dipendenze e task background.
//!
//! Responsabilita':
//!   1. Probe periodico delle dipendenze infrastrutturali (Qdrant, embedder)
//!      con persistenza in `nexus_dependency_health`.
//!   2. Auto-recovery: se un servizio e' down, tenta il ripristino automatico
//!      (restart container Docker, restart servizio systemd). Max 1 tentativo
//!      ogni 5 minuti per evitare loop di restart.
//!   3. Rilevamento e terminazione forzata di task background bloccati
//!      (quality scan, vector compaction, agent processes).
//!   4. Esposizione stato in-memory via `DependencyStatus` (atomico, zero-cost
//!      per i consumer — nessun lock, nessuna query DB nel hot path).
//!
//! Il loop principale gira ogni 60s (configurabile via env
//! `NEXUS_WATCHDOG_INTERVAL_S`). Ogni iterazione:
//!   - Probe Qdrant (HTTP GET /healthz, timeout 5s)
//!   - Probe embedder (gRPC embed_text "watchdog", timeout 10s)
//!   - Persiste risultati in DB (fire-and-forget)
//!   - Aggiorna DependencyStatus atomicamente
//!   - Se servizio DOWN: auto-recovery (con cooldown 5 min)
//!   - Query task bloccati e li marca 'failed'
//!   - Pulizia storico >24h
//!
//! Pattern: segue `provider_health_probe.rs` (loop asincrono, spawn, persist).

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::sleep;

use crate::agent_types::AgentStepEvent;
use crate::orchestrator::Orchestrator;
use crate::AgentChannels;

// ── Stato in-memory condiviso via Arc ────────────────────────────────────────

/// Stato atomico delle dipendenze. Letto dai task background per decidere
/// se avviare operazioni vettoriali (quality scan, semantic duplicates).
/// Zero overhead: `load(Relaxed)` e' una singola istruzione CPU.
#[derive(Debug)]
pub struct DependencyStatus {
    pub qdrant: AtomicBool,
    pub embedder: AtomicBool,
    /// Fix M48: stato del nexus-gateway (Node.js su :4060).
    pub gateway: AtomicBool,
    /// Unix timestamp dell'ultimo check completato.
    pub last_check: AtomicI64,
    /// Unix timestamp dell'ultimo tentativo di recovery (evita retry troppo frequenti).
    pub last_recovery_attempt: AtomicI64,
}

impl DependencyStatus {
    pub fn new() -> Self {
        Self {
            qdrant: AtomicBool::new(true),
            embedder: AtomicBool::new(true),
            gateway: AtomicBool::new(true),
            last_check: AtomicI64::new(0),
            last_recovery_attempt: AtomicI64::new(0),
        }
    }
}

pub type DependencyStatusRef = Arc<DependencyStatus>;

// ── Costanti ─────────────────────────────────────────────────────────────────

/// Timeout HTTP per il probe Qdrant. 5s e' conservativo per un servizio locale.
const QDRANT_PROBE_TIMEOUT_S: u64 = 5;

/// Timeout per il probe embedder (gRPC al brain Python).
const EMBEDDER_PROBE_TIMEOUT_S: u64 = 10;

/// Soglia per considerare un quality scan bloccato (5 minuti).
const STALE_SCAN_MINUTES: i32 = 5;

/// Soglia per considerare un agent process bloccato (10 minuti).
const STALE_PROCESS_MINUTES: i32 = 10;

// La soglia degli agent_run orfani e' ora DB-driven (mig 0392,
// agent.run_recovery.stale_after_seconds) e applicata dal punto unico
// run_reaper::reap_stale_runs sul criterio di LIVENESS (updated_at), non sull'eta'
// assoluta. Vedi terminate_stale_tasks.

// ── Spawn ────────────────────────────────────────────────────────────────────

/// Avvia il watchdog in background. Restituisce subito.
pub fn spawn_task_watchdog(
    db: PgPool,
    orchestrator: Arc<Orchestrator>,
    status: DependencyStatusRef,
    agent_channels: AgentChannels,
) {
    let enabled = std::env::var("NEXUS_TASK_WATCHDOG_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);
    if !enabled {
        tracing::info!("task_watchdog: DISABILITATO via env");
        return;
    }
    let interval_s = std::env::var("NEXUS_WATCHDOG_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .max(30);

    tracing::info!("task_watchdog: avviato, intervallo {}s", interval_s);

    tokio::spawn(async move {
        // Attesa iniziale: lascia tempo agli altri servizi di stabilizzarsi.
        sleep(Duration::from_secs(15)).await;
        loop {
            run_cycle(&db, &orchestrator, &status, &agent_channels).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

// ── Ciclo principale ─────────────────────────────────────────────────────────

async fn run_cycle(
    db: &PgPool,
    orchestrator: &Orchestrator,
    status: &DependencyStatus,
    agent_channels: &AgentChannels,
) {
    // 1. Probe dipendenze
    let was_qdrant_ok = status.qdrant.load(Ordering::Relaxed);
    let was_embedder_ok = status.embedder.load(Ordering::Relaxed);
    let was_gateway_ok = status.gateway.load(Ordering::Relaxed);

    let qdrant_result = probe_qdrant().await;
    let embedder_result = probe_embedder(orchestrator).await;
    let gateway_result = probe_gateway().await;

    // Aggiorna stato atomico
    status
        .qdrant
        .store(qdrant_result.healthy, Ordering::Relaxed);
    status
        .embedder
        .store(embedder_result.healthy, Ordering::Relaxed);
    status
        .gateway
        .store(gateway_result.healthy, Ordering::Relaxed);
    status
        .last_check
        .store(Utc::now().timestamp(), Ordering::Relaxed);

    // Log su cambio stato (evita spam nei log)
    if !qdrant_result.healthy && was_qdrant_ok {
        tracing::warn!(
            "task_watchdog: qdrant DOWN — {} ({}ms)",
            qdrant_result
                .error_message
                .as_deref()
                .unwrap_or("sconosciuto"),
            qdrant_result.latency_ms.unwrap_or(0),
        );
    } else if qdrant_result.healthy && !was_qdrant_ok {
        tracing::info!(
            "task_watchdog: qdrant RIPRISTINATO ({}ms)",
            qdrant_result.latency_ms.unwrap_or(0)
        );
    }

    if !embedder_result.healthy && was_embedder_ok {
        tracing::warn!(
            "task_watchdog: embedder DOWN — {} ({}ms)",
            embedder_result
                .error_message
                .as_deref()
                .unwrap_or("sconosciuto"),
            embedder_result.latency_ms.unwrap_or(0),
        );
    } else if embedder_result.healthy && !was_embedder_ok {
        tracing::info!(
            "task_watchdog: embedder RIPRISTINATO ({}ms)",
            embedder_result.latency_ms.unwrap_or(0)
        );
    }

    if !gateway_result.healthy && was_gateway_ok {
        tracing::warn!(
            "task_watchdog: nexus-gateway DOWN — {} ({}ms)",
            gateway_result
                .error_message
                .as_deref()
                .unwrap_or("sconosciuto"),
            gateway_result.latency_ms.unwrap_or(0),
        );
    } else if gateway_result.healthy && !was_gateway_ok {
        tracing::info!(
            "task_watchdog: nexus-gateway RIPRISTINATO ({}ms)",
            gateway_result.latency_ms.unwrap_or(0)
        );
    }

    // Persisti in DB (fire-and-forget)
    persist_probe(db, "qdrant", &qdrant_result).await;
    persist_probe(db, "embedder", &embedder_result).await;
    persist_probe(db, "gateway", &gateway_result).await;

    // ── Auto-recovery servizi down (max 1 tentativo ogni 5 minuti) ────────
    let now_ts = Utc::now().timestamp();
    let last_recovery = status.last_recovery_attempt.load(Ordering::Relaxed);
    let recovery_cooldown_expired = (now_ts - last_recovery) > 300;

    if recovery_cooldown_expired
        && (!qdrant_result.healthy || !embedder_result.healthy || !gateway_result.healthy)
    {
        status
            .last_recovery_attempt
            .store(now_ts, Ordering::Relaxed);
        if !qdrant_result.healthy {
            attempt_recovery(db, "qdrant", &qdrant_result).await;
        }
        if !embedder_result.healthy {
            attempt_recovery(db, "embedder", &embedder_result).await;
        }
        if !gateway_result.healthy {
            attempt_recovery(db, "gateway", &gateway_result).await;
        }
    }

    // 2. Detect e termina task bloccati
    terminate_stale_tasks(db, agent_channels).await;

    // 3. Pulizia storico >24h (una query leggera, eseguita ogni ciclo)
    let _ = sqlx::query(
        "DELETE FROM nexus_dependency_health WHERE checked_at < NOW() - INTERVAL '24 hours'",
    )
    .execute(db)
    .await;
}

// ── Auto-recovery ───────────────────────────────────────────────────────────

async fn attempt_recovery(db: &PgPool, service: &str, probe: &ProbeResult) {
    let kind = probe.error_kind.as_deref().unwrap_or("unknown");
    tracing::info!("task_watchdog: tentativo auto-recovery per {service} (errore: {kind})");

    match (service, kind) {
        ("qdrant", "connection_refused") => {
            // Qdrant potrebbe essere un container Docker fermo
            try_restart_container("qdrant").await;
        }
        ("qdrant", "timeout" | "too_many_files") => {
            // Qdrant sovraccarico — restart del container
            try_restart_container("qdrant").await;
        }
        ("embedder", "timeout") => {
            // Il brain gRPC potrebbe avere il canale bloccato — restart del processo
            try_restart_systemd_or_process(db, "brain").await;
        }
        ("embedder", "embed_error") => {
            // Errore nell'embedding — potrebbe essere un crash parziale del modello
            try_restart_systemd_or_process(db, "brain").await;
        }
        ("gateway", _) => {
            // Fix M48: gateway Node.js cade ripetutamente, lo riavvia.
            try_restart_gateway().await;
        }
        _ => {
            tracing::debug!("task_watchdog: nessuna strategia di recovery per {service}/{kind}");
        }
    }
}

/// Fix M48: probe HTTP del nexus-gateway su :4060/providers.
async fn probe_gateway() -> ProbeResult {
    let port = std::env::var("NEXUS_GATEWAY_PORT").unwrap_or_else(|_| "4060".into());
    let url = format!("http://localhost:{}/providers", port);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(QDRANT_PROBE_TIMEOUT_S))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult {
                healthy: false,
                latency_ms: None,
                error_kind: Some("client_error".into()),
                error_message: Some(e.to_string()),
            };
        }
    };
    let started = Instant::now();
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => ProbeResult {
            healthy: true,
            latency_ms: Some(started.elapsed().as_millis() as i32),
            error_kind: None,
            error_message: None,
        },
        Ok(r) => ProbeResult {
            healthy: false,
            latency_ms: Some(started.elapsed().as_millis() as i32),
            error_kind: Some("http_error".into()),
            error_message: Some(format!("HTTP {}", r.status())),
        },
        Err(e) => {
            let msg = e.to_string();
            let kind = if msg.contains("refused") || msg.contains("Connection refused") {
                "connection_refused"
            } else if msg.contains("timed out") || msg.contains("timeout") {
                "timeout"
            } else {
                "connection_error"
            };
            ProbeResult {
                healthy: false,
                latency_ms: Some(started.elapsed().as_millis() as i32),
                error_kind: Some(kind.into()),
                error_message: Some(msg[..msg.len().min(300)].to_string()),
            }
        }
    }
}

/// Fix M48: riavvia il nexus-gateway (Node.js) se cade. Usa lo stesso comando
/// di deploy-local.sh (setsid nohup node dist/server.js). Best-effort: errori
/// solo loggati. Cooldown 5 min gestito dal chiamante.
async fn try_restart_gateway() {
    let root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());
    let server_js = format!("{}/apps/nexus-gateway/dist/server.js", root);
    if !std::path::Path::new(&server_js).exists() {
        tracing::warn!(
            "task_watchdog: recovery gateway: {} non trovato, skip",
            server_js
        );
        return;
    }
    // Verifica che non sia gia stato avviato di recente (gestisce race con stop precedente).
    let _ = tokio::process::Command::new("pkill")
        .args(["-f", "apps/nexus-gateway/dist/server.js"])
        .output()
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // setsid nohup node ... > /tmp/nexus-gateway.log 2>&1 < /dev/null &
    let shell = format!(
        "setsid nohup env NODE_ENV=production NEXUS_GATEWAY_PORT={} node '{}' > /tmp/nexus-gateway.log 2>&1 < /dev/null &",
        std::env::var("NEXUS_GATEWAY_PORT").unwrap_or_else(|_| "4060".into()),
        server_js
    );
    match tokio::process::Command::new("sh")
        .args(["-c", &shell])
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            tracing::info!("task_watchdog: recovery gateway: spawn node OK");
        }
        Ok(o) => {
            tracing::warn!(
                "task_watchdog: recovery gateway: spawn fallito ({}): {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => tracing::warn!("task_watchdog: recovery gateway: shell exec fallito: {}", e),
    }
}

async fn try_restart_container(name_hint: &str) {
    let output = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name={name_hint}"),
            "--format",
            "{{.Names}} {{.Status}}",
        ])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            for line in stdout.lines() {
                let container_name = line.split_whitespace().next().unwrap_or("");
                if container_name.is_empty() {
                    continue;
                }
                // Non toccare container ideai-* (infrastruttura protetta)
                if container_name.starts_with("ideai-") {
                    tracing::info!(
                        "task_watchdog: recovery {name_hint}: container {container_name} e' infrastruttura protetta, skip restart"
                    );
                    continue;
                }
                if line.contains("Exited") || line.contains("exited") {
                    tracing::info!(
                        "task_watchdog: recovery {name_hint}: riavvio container {container_name}"
                    );
                    let _ = tokio::process::Command::new("docker")
                        .args(["start", container_name])
                        .output()
                        .await;
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            tracing::debug!("task_watchdog: recovery {name_hint}: docker ps fallito: {stderr}");
        }
        Err(e) => {
            tracing::debug!("task_watchdog: recovery {name_hint}: docker non disponibile: {e}");
        }
    }
}

/// Auto-recovery di un servizio core Nexus (es. brain) tramite il Sudo Manager
/// (regola L: unico canale privilegiato verso root, mig 0289). Dopo ADR 0028 L3 i
/// servizi core sono unit systemd --SYSTEM: il restart richiede root, ottenuto via
/// il purpose sudo `<name_hint>-restart` (es. `brain-restart` -> `systemctl restart
/// nexus-brain.service`, mig 0416). Il vecchio `systemctl --user restart` non
/// toccava piu' il servizio giusto e il fallback (pgrep + log) non riavviava nulla.
/// Best-effort: se il Sudo Manager non e' configurato logga WARN, non solleva mai;
/// il safety net resta systemd (Restart=always) per le uscite del processo.
async fn try_restart_systemd_or_process(db: &PgPool, name_hint: &str) {
    let purpose = format!("{name_hint}-restart");
    match crate::sudo_manager::execute(db, &purpose).await {
        Ok(outcome) if outcome.success => {
            tracing::info!(
                "task_watchdog: recovery {name_hint}: riavviato via sudo_manager ({purpose}, {}ms)",
                outcome.duration_ms
            );
        }
        Ok(outcome) => {
            tracing::warn!(
                "task_watchdog: recovery {name_hint}: purpose {purpose} exit={} stderr={}",
                outcome.exit_code,
                outcome.stderr.trim()
            );
        }
        Err(e) => {
            tracing::warn!(
                "task_watchdog: recovery {name_hint}: sudo_manager non disponibile per {purpose} ({e}); il servizio resta sotto gestione systemd (Restart=always)"
            );
        }
    }
}

// ── Probe dipendenze ─────────────────────────────────────────────────────────

struct ProbeResult {
    healthy: bool,
    latency_ms: Option<i32>,
    error_kind: Option<String>,
    error_message: Option<String>,
}

/// Probe Qdrant via HTTP GET /healthz (pattern da environment.rs:394).
async fn probe_qdrant() -> ProbeResult {
    let qdrant_url =
        std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
    let health_url = format!("{}/healthz", qdrant_url.trim_end_matches('/'));

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(QDRANT_PROBE_TIMEOUT_S))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult {
                healthy: false,
                latency_ms: None,
                error_kind: Some("client_error".into()),
                error_message: Some(e.to_string()),
            };
        }
    };

    let started = Instant::now();
    match client.get(&health_url).send().await {
        Ok(r) if r.status().is_success() => ProbeResult {
            healthy: true,
            latency_ms: Some(started.elapsed().as_millis() as i32),
            error_kind: None,
            error_message: None,
        },
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            let kind = if body.contains("Too many open files") {
                "too_many_files"
            } else {
                "http_error"
            };
            ProbeResult {
                healthy: false,
                latency_ms: Some(started.elapsed().as_millis() as i32),
                error_kind: Some(kind.into()),
                error_message: Some(format!(
                    "HTTP {} — {}",
                    status,
                    &body[..body.len().min(200)]
                )),
            }
        }
        Err(e) => {
            let msg = e.to_string();
            let kind = if msg.contains("timed out") || msg.contains("timeout") {
                "timeout"
            } else if msg.contains("refused") {
                "connection_refused"
            } else {
                "connection_error"
            };
            ProbeResult {
                healthy: false,
                latency_ms: Some(started.elapsed().as_millis() as i32),
                error_kind: Some(kind.into()),
                error_message: Some(msg[..msg.len().min(300)].to_string()),
            }
        }
    }
}

/// Probe embedder via gRPC embed_text (pattern da quality.rs:692).
async fn probe_embedder(orchestrator: &Orchestrator) -> ProbeResult {
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(EMBEDDER_PROBE_TIMEOUT_S),
        orchestrator.embed_text("watchdog"),
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    match result {
        Ok(Ok(_)) => ProbeResult {
            healthy: true,
            latency_ms: Some(latency_ms),
            error_kind: None,
            error_message: None,
        },
        Ok(Err(e)) => {
            let msg = e.to_string();
            ProbeResult {
                healthy: false,
                latency_ms: Some(latency_ms),
                error_kind: Some("embed_error".into()),
                error_message: Some(msg[..msg.len().min(300)].to_string()),
            }
        }
        Err(_) => ProbeResult {
            healthy: false,
            latency_ms: Some(latency_ms),
            error_kind: Some("timeout".into()),
            error_message: Some(format!("nessuna risposta in {}s", EMBEDDER_PROBE_TIMEOUT_S)),
        },
    }
}

/// Persiste un risultato probe in `nexus_dependency_health` (fire-and-forget).
async fn persist_probe(db: &PgPool, dependency: &str, result: &ProbeResult) {
    let _ = sqlx::query(
        "INSERT INTO nexus_dependency_health \
         (dependency, healthy, latency_ms, error_kind, error_message) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(dependency)
    .bind(result.healthy)
    .bind(result.latency_ms)
    .bind(result.error_kind.as_deref())
    .bind(result.error_message.as_deref())
    .execute(db)
    .await;
}

// ── Detect e termina task bloccati ───────────────────────────────────────────

async fn terminate_stale_tasks(db: &PgPool, agent_channels: &AgentChannels) {
    // Quality scans bloccate (>5 minuti in "running")
    let stale_scans = sqlx::query_scalar::<_, i64>(
        "UPDATE nexus_quality_scans \
         SET status = 'failed', \
             error_message = 'Watchdog: task bloccato per oltre 5 minuti', \
             completed_at = NOW() \
         WHERE status = 'running' \
           AND started_at < NOW() - make_interval(mins => $1) \
         RETURNING id",
    )
    .bind(STALE_SCAN_MINUTES)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for scan_id in &stale_scans {
        tracing::warn!(
            "task_watchdog: terminata quality scan bloccata id={}",
            scan_id
        );
    }

    // Vector compaction bloccate (>10 minuti)
    let stale_compactions = sqlx::query_scalar::<_, uuid::Uuid>(
        "UPDATE vector_compaction_runs \
         SET status = 'failed', \
             finished_at = NOW() \
         WHERE status = 'running' \
           AND started_at < NOW() - make_interval(mins => $1) \
         RETURNING id",
    )
    .bind(STALE_PROCESS_MINUTES)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for id in &stale_compactions {
        tracing::warn!(
            "task_watchdog: terminata vector compaction bloccata id={}",
            id
        );
    }

    // Agent processes bloccati (>10 minuti senza heartbeat)
    let stale_processes = sqlx::query_scalar::<_, uuid::Uuid>(
        "UPDATE agent_processes \
         SET status = 'failed', \
             stopped_at = NOW(), \
             error_output = COALESCE(error_output, '') || '\nWatchdog: processo bloccato per oltre 10 minuti' \
         WHERE status = 'running' \
           AND started_at < NOW() - make_interval(mins => $1) \
         RETURNING id",
    )
    .bind(STALE_PROCESS_MINUTES)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for id in &stale_processes {
        tracing::warn!("task_watchdog: terminato agent process bloccato id={}", id);
    }

    // Agent runs orfani: chiusura selettiva via PUNTO UNICO (regola L,
    // run_reaper::reap_stale_runs). Marca 'interrupted' SOLO i run 'running'
    // senza battito `updated_at` oltre soglia (heartbeat dal brain, mig 0392),
    // NON gli 'awaiting_confirmation' (resumibili via checkpoint LangGraph). Il
    // criterio e' la LIVENESS (updated_at), non l'eta' assoluta (created_at):
    // cosi' un run legittimamente lungo che batte non viene piu' ucciso.
    // L'UPDATE e il messaggio assistente sono nel punto unico; qui sblocchiamo
    // gli EventSource ancora in ascolto sui run reapati.
    let stale_seconds = crate::run_reaper::stale_seconds_from_settings(db).await;
    let reaped = crate::run_reaper::reap_stale_runs(db, stale_seconds).await;
    for run_id in &reaped {
        tracing::warn!("task_watchdog: terminato agent_run orfano id={}", run_id);
        if let Some(tx) = agent_channels.get(run_id) {
            let _ = tx.send(AgentStepEvent {
                run_id: run_id.to_string(),
                step: None,
                trace: None,
                is_final: true,
                token_delta: None,
                thinking_delta: None,
                meta_step: None,
            });
        }
        agent_channels.remove(run_id);
    }
}

// ── Handler HTTP: GET /api/admin/watchdog-status ─────────────────────────────

pub async fn watchdog_status_handler(
    State(state): State<crate::AppState>,
) -> Json<serde_json::Value> {
    let dep_status = &state.dependency_status;
    let qdrant_ok = dep_status.qdrant.load(Ordering::Relaxed);
    let embedder_ok = dep_status.embedder.load(Ordering::Relaxed);
    let last_check = dep_status.last_check.load(Ordering::Relaxed);
    let last_check_str = if last_check > 0 {
        chrono::DateTime::from_timestamp(last_check, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    } else {
        "mai".to_string()
    };

    // Ultimi dettagli dai probe piu' recenti
    let qdrant_detail = sqlx::query_as::<_, (bool, Option<i32>, Option<String>, Option<String>)>(
        "SELECT healthy, latency_ms, error_kind, error_message \
         FROM nexus_dependency_health \
         WHERE dependency = 'qdrant' \
         ORDER BY checked_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let embedder_detail = sqlx::query_as::<_, (bool, Option<i32>, Option<String>, Option<String>)>(
        "SELECT healthy, latency_ms, error_kind, error_message \
         FROM nexus_dependency_health \
         WHERE dependency = 'embedder' \
         ORDER BY checked_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    // Conteggi task bloccati attuali
    let stale_scans: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_quality_scans \
         WHERE status = 'running' \
           AND started_at < NOW() - INTERVAL '5 minutes'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let stale_compactions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM vector_compaction_runs \
         WHERE status = 'running' \
           AND started_at < NOW() - INTERVAL '10 minutes'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Task terminati dal watchdog nelle ultime 24h
    let terminated_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_quality_scans \
         WHERE status = 'failed' \
           AND error_message LIKE 'Watchdog:%' \
           AND completed_at > NOW() - INTERVAL '24 hours'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let mk_dep_json =
        |ok: bool, detail: Option<(bool, Option<i32>, Option<String>, Option<String>)>| match detail
        {
            Some((_, latency, error_kind, error_msg)) => json!({
                "healthy": ok,
                "latency_ms": latency,
                "error_kind": error_kind,
                "error": error_msg,
                "last_check": &last_check_str,
            }),
            None => json!({
                "healthy": ok,
                "last_check": &last_check_str,
            }),
        };

    Json(json!({
        "dependencies": {
            "qdrant": mk_dep_json(qdrant_ok, qdrant_detail),
            "embedder": mk_dep_json(embedder_ok, embedder_detail),
        },
        "stale_tasks": {
            "quality_scans": stale_scans,
            "compaction_runs": stale_compactions,
        },
        "tasks_terminated_last_24h": terminated_24h,
    }))
}
