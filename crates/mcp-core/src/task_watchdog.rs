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
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::time::sleep;

use crate::agent_types::AgentStepEvent;
use crate::nexus_gateway::transport_facts;
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

/// Timeout per il probe embedder (in-process, via `Orchestrator::embed_text`).
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

    let qdrant_result = probe_qdrant(db).await;
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

async fn attempt_recovery(_db: &PgPool, service: &str, probe: &ProbeResult) {
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
        ("embedder", _) => {
            // L'embedder e' in-process (ONNX in nexus-orchestrator, probe via
            // Orchestrator::embed_text): non c'e' alcun servizio esterno da
            // riavviare (il brain Python e' stato rimosso, mig 0462/0532; il
            // vecchio restart di 'nexus-brain' rilanciava un servizio
            // inesistente). Si logga per la diagnosi; il probe sopra ha gia'
            // persistito lo stato in nexus_dependency_health.
            tracing::warn!(
                "task_watchdog: embedder in-process non sano ({kind}); nessun restart esterno applicabile"
            );
        }
        ("gateway", _) => {
            // ADR 0028 L3: il gateway e' ora una unit systemd --system con
            // Restart=always (deploy/systemd/nexus-gateway-system.service). Il
            // riavvio e' garantito da PID 1: il watchdog NON deve rilanciarlo a
            // mano (un secondo nohup creerebbe conflitto di porta col processo che
            // systemd sta gia' rilanciando). Si limita a loggare; il probe sopra
            // ha gia' persistito lo stato DOWN in nexus_dependency_health.
            note_gateway_down_systemd_recovers(kind);
        }
        _ => {
            tracing::debug!("task_watchdog: nessuna strategia di recovery per {service}/{kind}");
        }
    }
}

/// Kind del probe da segnali STRUTTURATI (regola M): il Display di
/// reqwest::Error e' sempre "error sending request for url (...)", non porta
/// MAI "refused"/"timeout" (verificato — regola O). Punto unico condiviso da
/// probe_gateway/probe_qdrant per lo stesso identico problema (regola L).
fn probe_error_kind(facts: &nexus_types::error_presentation::TransportFacts) -> &'static str {
    if facts.is_timeout {
        "timeout"
    } else if facts.io_kind.as_deref() == Some("ConnectionRefused") {
        "connection_refused"
    } else {
        "connection_error"
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
            let facts = transport_facts(&e, &url);
            let kind = probe_error_kind(&facts);
            let msg = e.to_string();
            ProbeResult {
                healthy: false,
                latency_ms: Some(started.elapsed().as_millis() as i32),
                error_kind: Some(kind.into()),
                error_message: Some(msg[..msg.len().min(300)].to_string()),
            }
        }
    }
}

/// ADR 0028 L3: il gateway LLM (binario Rust, crate `nexus-gateway`) e' ora una
/// unit systemd --system con `Restart=always`
/// (deploy/systemd/nexus-gateway-system.service). Il riavvio quando cade e'
/// garantito da PID 1.
///
/// Storia: con il vecchio avvio `setsid nohup` il watchdog rilanciava sempre la
/// stessa build (target/debug/nexus-gateway non aggiornato ai commit) -> il
/// gateway non si aggiornava col deploy e un secondo spawn poteva collidere
/// sulla porta. Sotto systemd il watchdog NON deve piu' rilanciare il processo:
/// si limita a registrare il DOWN (il probe ha gia' persistito lo stato in
/// `nexus_dependency_health`); systemd ripristina il servizio entro RestartSec.
fn note_gateway_down_systemd_recovers(error_kind: &str) {
    tracing::warn!(
        "task_watchdog: nexus-gateway DOWN (errore: {error_kind}) — riavvio delegato a systemd (Restart=always, unit nexus-gateway.service). Nessun rilancio manuale per evitare conflitto di porta."
    );
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

// NB: try_restart_systemd_or_process (restart via Sudo Manager, mig 0289/0416)
// e' stata rimossa con la bonifica zero-Python (mig 0532): il suo unico
// chiamante era il recovery dell'embedder verso il servizio 'brain', che non
// esiste piu'. I servizi core restano sotto systemd (Restart=always).

// ── Probe dipendenze ─────────────────────────────────────────────────────────

struct ProbeResult {
    healthy: bool,
    latency_ms: Option<i32>,
    error_kind: Option<String>,
    error_message: Option<String>,
}

/// Probe Qdrant via HTTP GET /healthz (pattern da environment.rs:394).
async fn probe_qdrant(db: &PgPool) -> ProbeResult {
    // Stessa fonte URL dei client (regola L): setting DB `qdrant_url` -> env ->
    // default REST 6333. Prima leggeva solo l'env, divergendo dai client e
    // dando falsi negativi se l'env puntava alla porta gRPC 6334.
    let qdrant_url = crate::settings::resolve_qdrant_url(db).await;
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
            let facts = transport_facts(&e, &health_url);
            let kind = probe_error_kind(&facts);
            let msg = e.to_string();
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

/// Servizi orfani di UN progetto: reap SOLO su una morte ACCERTATA del SERVIZIO,
/// dal punto unico `service_liveness` — non del processo registrato, che e' la
/// shell e non il server (i figli le sopravvivono). Ritorna gli id reapati.
///
/// Prima il criterio qui era `process_alive(pid)` grezzo, e sbagliava in ENTRAMBE
/// le direzioni: dichiarava vivo un pid RICICLATO su un estraneo, e morto un
/// servizio che stava servendo richieste. MISURATO l'08/08/2026 su gestione-corsi:
/// ucciso il solo capostipite `bash` 10896, questo ramo ha scritto `failed` 39
/// secondi dopo mentre il server rispondeva HTTP 200 sulla porta 34894 allocata a
/// quella label. La scrittura e' il danno vero: il pannello mostra la riga morta e
/// l'observer apre una diagnosi di crash, cioe' mette in moto la remediation su
/// cio' che gira.
async fn reap_servizi_morti(
    db: &PgPool,
    pool: &PgPool,
    project_id: uuid::Uuid,
) -> Vec<uuid::Uuid> {
    let running: Vec<(uuid::Uuid, String, Option<i32>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, label, pid, started_at FROM agent_processes \
         WHERE status = 'running' AND kind = 'service'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if running.is_empty() {
        return Vec::new();
    }
    // Le prove costano una query e una syscall: si raccolgono una volta per
    // progetto, e solo se c'e' almeno una riga da giudicare.
    let prove =
        crate::project_workspace::service_liveness::ProveDiVita::del_progetto(db, project_id).await;
    let mut reapati = Vec::new();
    for (id, label, pid, started_at) in running {
        let verdetto = prove.verdetto(&label, pid, started_at);
        if verdetto.autorizza_a_dichiararlo_morto() {
            reapati.extend(marca_servizio_morto(pool, id).await);
        } else if !verdetto.e_vivo() {
            // Non abbiamo osservato niente: la riga resta com'e'. Scrivere
            // 'failed' su una non-osservazione e' il modo in cui l'errore
            // diventa persistente e la lettura dopo lo conferma.
            tracing::warn!(
                project_id = %project_id,
                process_id = %id,
                label = %label,
                motivo = %verdetto.descrizione(),
                "task_watchdog: stato del servizio non accertabile, riga lasciata invariata"
            );
        }
    }
    reapati
}

/// Marca `failed` una riga di servizio: la scrittura, separata dal criterio.
async fn marca_servizio_morto(pool: &PgPool, id: uuid::Uuid) -> Option<uuid::Uuid> {
    const MOTIVO: &str =
        "\nWatchdog: servizio non piu vivo (processo registrato morto, nessuna porta in ascolto)";
    sqlx::query_scalar::<_, uuid::Uuid>(
        "UPDATE agent_processes \
         SET status = 'failed', \
             stopped_at = NOW(), \
             error_output = COALESCE(error_output, '') || $2 \
         WHERE id = $1 AND status = 'running' \
         RETURNING id",
    )
    .bind(id)
    .bind(MOTIVO)
    .fetch_optional(pool)
    .await
    .unwrap_or_default()
}

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

    // Agent processes bloccati. Separazione DB (sempre attiva, mig 0527):
    // agent_processes vive nel DB del progetto -> iteriamo i progetti e marchiamo
    // sul pool di ciascuno; un progetto col DB non disponibile viene saltato.
    //
    // Distinzione per `kind` (fix definitivo, regola H):
    //  - `kind <> 'service'`: processi one-shot (build/comando). Un blocco oltre
    //    la soglia indica stallo reale -> reap per eta' assoluta.
    //  - `kind = 'service'`: dev server long-running (vite, `node --watch`, ...).
    //    Girano all'infinito PER NATURA: reaparli per eta' assoluta li uccideva a
    //    ogni ciclo (~10 min) -> servizi sempre 'failed', pannello Porte e Console
    //    Debug vuoti. Si reapano SOLO per LIVENESS reale (pid morto = riga orfana
    //    dopo un riavvio di mcp-core che ha perso il monitor), stesso criterio
    //    degli agent_runs qui sotto (updated_at), non l'eta' assoluta.
    let mut stale_processes: Vec<uuid::Uuid> = Vec::new();
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "task_watchdog: DB progetto non disponibile, progetto saltato per questo giro");
                continue;
            }
        };

        // (a) One-shot bloccati: reap per eta' assoluta.
        let mut ids = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE agent_processes \
             SET status = 'failed', \
                 stopped_at = NOW(), \
                 error_output = COALESCE(error_output, '') || '\nWatchdog: processo bloccato per oltre 10 minuti' \
             WHERE status = 'running' \
               AND kind <> 'service' \
               AND started_at < NOW() - make_interval(mins => $1) \
             RETURNING id",
        )
        .bind(STALE_PROCESS_MINUTES)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        stale_processes.append(&mut ids);

        // (b) Servizi orfani: vedi `reap_servizi_morti`.
        stale_processes.extend(reap_servizi_morti(db, &pool, project_id).await);
    }

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
        sblocca_stream(agent_channels, run_id);
    }

    // Sospensioni MATURATE (rilievo A4, punto unico
    // run_reaper::expire_matured_suspensions): un run fermo su una decisione
    // umana che, nella modalita' in cui girava, nessuno poteva prendere. Sweep
    // separata dal reap perche' il criterio e il contratto di chiusura sono
    // altri (scadenza scritta sulla riga -> `blocked_needs_input`, non
    // `interrupted`); qui, come sopra, si sbloccano solo gli stream in ascolto.
    for run_id in &crate::run_reaper::expire_matured_suspensions(db).await {
        tracing::warn!(
            "task_watchdog: sospensione scaduta, run chiuso bloccato id={}",
            run_id
        );
        sblocca_stream(agent_channels, run_id);
    }
}

/// Sblocca gli EventSource ancora agganciati a un run che e' stato chiuso da
/// fuori (reap o scadenza): senza `is_final` il client resta in ascolto di uno
/// stream che nessuno alimentera' piu'. Estratta perche' le DUE sweep fanno la
/// stessa identica cosa (regola L) e una copia divergerebbe sul silenzio.
fn sblocca_stream(agent_channels: &AgentChannels, run_id: &uuid::Uuid) {
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
