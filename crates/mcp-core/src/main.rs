mod agent_processes;
mod agent_types;
mod brain_agent_client;
mod dlp;
mod provider_cooldown;
mod models;
mod sandbox;
mod agent_tools;
mod admin;
mod auth;
mod billing;
mod cache;
mod chat_agent;
mod chat_learning;
mod chat_messages;
mod chat_sessions;
mod db;
mod domain;
mod github;
mod mcp_client;
mod mcp_connectors;
mod nexus_builtin;
mod nexus_bridge;
mod nexus_gateway;
mod nexus_routing;
mod nexus_tool_catalog;
mod nexus_tools;
mod middleware;
mod orchestrator;
mod routing_config;
mod routing_matrix;
mod routing_slots;
mod project_files;
mod project_git;
mod project_workspace;
mod projects;
mod plugins;
mod profiles;
mod long_running;
mod prompt_templates;
mod documents;
mod environment;
mod settings;
mod vector_memory;
mod project_context;
mod quality_guard;
mod project_db;
mod project_db_routes;
mod tool_runner_server;
mod agent_router_server;
mod nexus_database_stats;
mod internal_routing;
mod internal_learning;
mod port_registry;
mod provider_health_probe;
mod task_watchdog;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{header as http_header, HeaderValue, Method};
use axum::{
    extract::State,
    middleware as axum_mw,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use chrono::Utc;
use dashmap::{DashMap, DashSet};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    agent_types::AgentStepEvent,
    domain::HealthSummary,
    orchestrator::{NeuralCoreClient, Orchestrator},
};

/// Channel per aggiornamenti SSE del loop agente.
/// Chiave: run_id dell'agent_run, valore: sender broadcast.
pub type AgentChannels = Arc<DashMap<Uuid, broadcast::Sender<AgentStepEvent>>>;
/// Mappa terminali connessi per utente/progetto.
/// Chiave: "{user_id}:{project_id}", valore: numero consumer attivi.
pub type TerminalConsumers = Arc<DashMap<String, usize>>;

#[derive(Clone)]
struct AppState {
    db: PgPool,
    redis: redis::aio::MultiplexedConnection,
    orchestrator: Orchestrator,
    agent_channels: AgentChannels,
    terminal_consumers: TerminalConsumers,
    template_cache: prompt_templates::TemplateCache,
    /// `true` se l'immagine `nexus-sandbox:latest` è disponibile nel daemon Docker.
    /// Quando `true`, ogni processo agente gira in un container Docker isolato.
    sandbox_available: bool,
    /// Cache della matrice di routing letta da DB (vedi migrazione 0101).
    /// Refresh background ogni 60s. Sostituisce i model name hardcoded
    /// che erano sparsi in orchestrator.rs / chat_messages.rs / models.rs.
    routing_matrix: routing_matrix::RoutingMatrixCache,
    /// Cache parametri routing (settings.routing.*) — mig 0111. Refresh 60s.
    /// Sostituisce le costanti hardcoded come LLM_CLASSIFIER_MIN_CONFIDENCE.
    routing_thresholds: routing_config::RoutingThresholdsCache,
    /// Cache mapping intent -> tier/capability/preferred_provider — mig 0110.
    /// Sostituisce i match Rust statici in orchestrator.rs:444-490.
    intent_capability: routing_config::IntentCapabilityCache,
    /// Registro centralizzato porte TCP allocate ai progetti — mig 0114.
    /// Impedisce conflitti tra progetti e con porte interne Nexus.
    port_registry: port_registry::PortRegistryCache,
    /// Stato atomico delle dipendenze infrastrutturali (Qdrant, embedder).
    /// Aggiornato dal task_watchdog ogni 60s. Consultato dai task background
    /// (quality scan) per decidere se avviare operazioni vettoriali.
    dependency_status: task_watchdog::DependencyStatusRef,
    /// Set dei project_id la cui indicizzazione semantica e' attualmente in corso.
    /// Usato da `spawn_code_index_if_needed` per evitare lanci duplicati.
    pub(crate) indexing_projects: Arc<DashSet<Uuid>>,
    /// Set dei project_id per cui il file watcher inotify e' gia' attivo.
    /// Evita di avviare watcher duplicati sullo stesso progetto.
    pub(crate) watching_projects: Arc<DashSet<Uuid>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable".to_string()
    });
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    let neural_core_url =
        std::env::var("NEURAL_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:50051".to_string());

    nexus_tool_catalog::NexusToolCatalog::init_global();
    if let Some(cat) = nexus_tool_catalog::NexusToolCatalog::global() {
        tracing::info!(
            "NexusToolCatalog initialized: {} tool specs ({} implemented)",
            cat.len(),
            cat.implemented_count()
        );
    }

    let db = db::init_pool(&database_url).await?;

    // Inizializza NexusBridge con pool DB (Fase 6 + persistenza Q-values):
    // - Router Q-Learning con persistenza asincrona su nexus_q_values
    // - Caricamento Q-values esistenti in background (non blocca l'avvio)
    // - Non invasivo: se qualche componente fallisce, il bridge resta non
    //   inizializzato e i siti di chiamata fanno fallback silenzioso.
    nexus_bridge::NexusBridge::init_global_with_pool(Arc::new(db.clone())).await;

    // Riconcilia i processi lasciati in stato 'running'/'starting' da un riavvio precedente.
    // - PID non più vivo → status=failed
    // - PID ancora vivo → rimane running, si rilancia un task di monitoring su /proc/{pid}/fd
    {
        use sqlx::Row;
        let stale = sqlx::query(
            "SELECT id, pid FROM agent_processes WHERE status IN ('running', 'starting')"
        )
        .fetch_all(&db)
        .await
        .unwrap_or_default();

        for row in stale {
            let id: uuid::Uuid = row.get("id");
            let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
            let still_alive = pid.map(|p| {
                std::process::Command::new("kill")
                    .args(["-0", &p.to_string()])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            }).unwrap_or(false);

            if !still_alive {
                let _ = sqlx::query(
                    "UPDATE agent_processes SET status='failed', stopped_at=NOW() WHERE id=$1"
                )
                .bind(id)
                .execute(&db)
                .await;
                tracing::info!("Stale process {} (pid={:?}) marked failed", id, pid);
            } else {
                tracing::info!("Process {} (pid={:?}) still running, re-attaching monitor", id, pid);
                let pid_val = pid.unwrap();
                let db_clone = db.clone();
                // Rilancia un task che segue stdout+stderr tramite /proc/{pid}/fd/1,2
                tokio::spawn(async move {
                    let stdout_path = format!("/proc/{}/fd/1", pid_val);
                    let stderr_path = format!("/proc/{}/fd/2", pid_val);
                    let mut child = match tokio::process::Command::new("tail")
                        .args(["-f", "--pid", &pid_val.to_string(), &stdout_path, &stderr_path])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("Failed to re-attach to process {}: {}", pid_val, e);
                            // Non possiamo seguire l'output ma il processo risulta running
                            // Aspettiamo che termini tramite polling
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                let alive = std::process::Command::new("kill")
                                    .args(["-0", &pid_val.to_string()])
                                    .status()
                                    .map(|s| s.success())
                                    .unwrap_or(false);
                                if !alive {
                                    let exit_code: Option<i32> = std::process::Command::new("sh")
                                        .args(["-c", &format!("cat /proc/{}/status 2>/dev/null | grep -c VmPeak", pid_val)])
                                        .output()
                                        .ok()
                                        .and_then(|o| String::from_utf8(o.stdout).ok())
                                        .and_then(|s| s.trim().parse().ok());
                                    let _ = sqlx::query(
                                        "UPDATE agent_processes SET status='stopped', exit_code=$2, stopped_at=NOW() WHERE id=$1"
                                    )
                                    .bind(id)
                                    .bind(exit_code.unwrap_or(-1))
                                    .execute(&db_clone)
                                    .await;
                                    break;
                                }
                            }
                            return;
                        }
                    };

                    // Leggi output dal tail e appendilo al DB
                    use tokio::io::AsyncBufReadExt;
                    let stdout = child.stdout.take();
                    if let Some(stdout) = stdout {
                        let mut lines = tokio::io::BufReader::new(stdout).lines();
                        let mut buf = String::new();
                        let mut flush_tick = tokio::time::interval(std::time::Duration::from_secs(2));
                        loop {
                            tokio::select! {
                                line = lines.next_line() => {
                                    match line {
                                        Ok(Some(l)) => { buf.push_str(&l); buf.push('\n'); }
                                        _ => break,
                                    }
                                }
                                _ = flush_tick.tick() => {
                                    if !buf.is_empty() {
                                        let chunk = std::mem::take(&mut buf);
                                        let _ = sqlx::query(
                                            "UPDATE agent_processes SET output = LEFT(output || $1, 50000) WHERE id=$2"
                                        )
                                        .bind(&chunk)
                                        .bind(id)
                                        .execute(&db_clone)
                                        .await;
                                    }
                                }
                            }
                        }
                        if !buf.is_empty() {
                            let _ = sqlx::query(
                                "UPDATE agent_processes SET output = LEFT(output || $1, 50000) WHERE id=$2"
                            )
                            .bind(&buf)
                            .bind(id)
                            .execute(&db_clone)
                            .await;
                        }
                    }

                    // tail è uscito: il processo originale è terminato
                    let _ = sqlx::query(
                        "UPDATE agent_processes SET status='stopped', stopped_at=NOW() WHERE id=$1 AND status='running'"
                    )
                    .bind(id)
                    .execute(&db_clone)
                    .await;
                });
            }
        }
    }

    // Marca le agent_run rimaste in stato 'running' o 'awaiting_confirmation' come interrotte.
    // Questo accade quando il server si riavvia durante un'elaborazione attiva.
    {
        let interrupted_msg = "Il server è stato riavviato durante l'elaborazione. \
            L'operazione è stata interrotta. Puoi ripetere la richiesta.";
        let affected = sqlx::query(
            r#"
            UPDATE agent_runs
            SET status = 'interrupted',
                final_answer = $1,
                completed_at = NOW()
            WHERE status IN ('running', 'awaiting_confirmation')
              AND completed_at IS NULL
            "#,
        )
        .bind(interrupted_msg)
        .execute(&db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);

        if affected > 0 {
            tracing::warn!(
                "Marcate {} agent_run interrotte dal riavvio come 'interrupted'",
                affected
            );

            // Salva il messaggio di interruzione in chat_messages per ogni run orfano
            // che non ha già un messaggio assistente associato.
            let inserted = sqlx::query(
                r#"
                INSERT INTO chat_messages
                    (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                SELECT
                    gen_random_uuid(),
                    ar.session_id,
                    ar.project_id,
                    'assistant',
                    $1,
                    jsonb_build_object(
                        'agentRunId', ar.id::text,
                        'automationMode', 'agent',
                        'interrupted', true
                    ),
                    ar.run_message_id,
                    NOW()
                FROM agent_runs ar
                WHERE ar.status = 'interrupted'
                  AND ar.run_message_id IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM chat_messages cm
                      WHERE cm.request_message_id = ar.run_message_id
                        AND cm.role = 'assistant'
                  )
                "#,
            )
            .bind(interrupted_msg)
            .execute(&db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);

            if inserted > 0 {
                tracing::warn!(
                    "Inseriti {} messaggi assistente per run interrotti senza risposta",
                    inserted
                );
            }
        }

        // NOTA: il cleanup dei processi 'running'/'starting' è già gestito dal blocco di
        // riconciliazione PID sopra (kill -0). Non sovrascrivere qui i processi ancora vivi.
    }

    let redis = cache::init_redis(&redis_url).await?;

    // Espone il client Redis a `provider_cooldown` per la persistenza dei
    // cooldown lunghi (billing/quota). Senza questo, un restart di mcp-core
    // perderebbe i cooldown in-memory e il LED tornerebbe verde anche se
    // il provider e' realmente giu' (caso utente "LED openai verde").
    crate::provider_cooldown::init_redis_client(redis.clone());

    // Ripristina cooldown billing provider sopravvissuti al riavvio (persistiti su Redis).
    {
        let mut conn = redis.clone();
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Ok(keys) = redis::cmd("KEYS")
            .arg("nexus:billing_cooldown:*")
            .query_async::<Vec<String>>(&mut conn)
            .await
        {
            for key in keys {
                if let Ok(value) = redis::cmd("GET")
                    .arg(&key)
                    .query_async::<String>(&mut conn)
                    .await
                {
                    // Formato: "<until_unix_ts>|<reason>"
                    let mut parts = value.splitn(2, '|');
                    if let (Some(ts_str), Some(reason)) = (parts.next(), parts.next()) {
                        if let Ok(until_ts) = ts_str.parse::<u64>() {
                            if until_ts > now_ts {
                                let remaining = until_ts - now_ts;
                                let provider = key.strip_prefix("nexus:billing_cooldown:")
                                    .unwrap_or(&key);
                                crate::provider_cooldown::restore_cooldown(provider, remaining, reason);
                            }
                        }
                    }
                }
            }
        }
    }

    let neural_client = {
        let mut attempts = 0;
        loop {
            match NeuralCoreClient::connect(&neural_core_url).await {
                Ok(c) => break c,
                Err(e) => {
                    attempts += 1;
                    if attempts >= 10 {
                        anyhow::bail!(
                            "Failed to connect to Neural Core after {attempts} attempts: {e}"
                        );
                    }
                    tracing::warn!("Neural Core not ready (attempt {attempts}/10): {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    };
    let template_cache = prompt_templates::TemplateCache::new();

    // Inizializza la cache routing matrix (legge da DB + spawn refresh background 60s).
    // Va inizializzata PRIMA di Orchestrator::new perche' viene clonata dentro l'orchestrator.
    let routing_matrix_cache = routing_matrix::RoutingMatrixCache::init(db.clone()).await;

    // Cache parametri routing (mig 0111) e intent capability (mig 0110).
    // Stesso pattern: retry 5x5s + panic se DB irraggiungibile.
    let routing_thresholds_cache =
        routing_config::RoutingThresholdsCache::init_thresholds(db.clone()).await;
    let intent_capability_cache =
        routing_config::IntentCapabilityCache::init_intent_capability(db.clone()).await;
    // Cache matrice slot-based (mig 0133, Livello 4 NLU). Diversamente dalle
    // altre cache, non panica se assente: il routing classico (intent,mode)
    // resta il fallback predefinito quando la matrice slots non e' popolata.
    let slots_matrix_cache = routing_slots::SlotsRoutingMatrixCache::init(&db).await;

    // Cache registro porte (mig 0114): porte TCP allocate ai progetti.
    // Non panica se tabella vuota (nessuna allocazione al primo avvio).
    let port_registry_cache = port_registry::PortRegistryCache::init(db.clone()).await;
    // Recovery: sincronizza registro con file .service esistenti su disco.
    port_registry_cache.startup_recovery().await;

    let gw_url = std::env::var("NEXUS_GATEWAY_URL")
        .unwrap_or_else(|_| "http://localhost:4060".to_string());
    let gw_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
        .unwrap_or_else(|_| "dev-internal-token".to_string());
    let nexus_gw = nexus_gateway::NexusGatewayClient::new(gw_url.clone(), gw_token);
    let orchestrator = {
        let base = Orchestrator::new(
            neural_client,
            template_cache.clone(),
            routing_matrix_cache.clone(),
            routing_thresholds_cache.clone(),
            intent_capability_cache.clone(),
            slots_matrix_cache.clone(),
        );
        if nexus_gw.is_healthy().await {
            tracing::info!("Nexus Gateway disponibile su {gw_url} — PATH A attivo");
            base.with_gateway(nexus_gw)
        } else {
            tracing::warn!("Nexus Gateway non raggiungibile su {gw_url} — uso PATH B (Brain gRPC)");
            base
        }
    };

    // Verifica disponibilità sandbox Docker all'avvio e imposta il flag globale
    let sandbox_available = sandbox::is_sandbox_available().await;
    sandbox::set_sandbox_available(sandbox_available);
    tracing::info!(
        sandbox = sandbox_available,
        "Sandbox Docker {}: ogni processo agente sarà {}",
        if sandbox_available { "attiva" } else { "non disponibile" },
        if sandbox_available { "isolato in container nexus-sandbox" } else { "eseguito con env filtrato (fallback)" }
    );

    let state = AppState {
        db,
        redis,
        orchestrator,
        agent_channels: Arc::new(DashMap::new()),
        terminal_consumers: Arc::new(DashMap::new()),
        template_cache,
        sandbox_available,
        routing_matrix: routing_matrix_cache,
        routing_thresholds: routing_thresholds_cache,
        intent_capability: intent_capability_cache,
        port_registry: port_registry_cache,
        dependency_status: std::sync::Arc::new(task_watchdog::DependencyStatus::new()),
        indexing_projects: Arc::new(DashSet::new()),
        watching_projects: Arc::new(DashSet::new()),
    };
    chat_learning::spawn_vector_compaction_scheduler(state.clone());
    nexus_builtin::seed_tools_and_server(&state.db).await;
    // Reindex semantico Qdrant dei tool MCP (fire-and-forget, +30s delay).
    // Indicizza solo i tool con embedding mancante o hash cambiato.
    nexus_builtin::spawn_tool_reindex(state.db.clone(), state.orchestrator.neural.clone());

    // ── Lettura batch settings DB ────────────────────────────────────────────
    // Leggiamo in parallelo tutti i flag comportamentali dalla tabella settings.
    // I valori sono usati nei blocchi successivi. Le env var restano come
    // override di emergenza con priorita' piu' alta (gestita in ogni blocco).
    let (
        s_llm_classifier,
        s_health_probe_enabled,
        s_health_probe_interval,
        s_tool_runner,
        s_http_timeout,
        s_http_pool,
    ) = tokio::join!(
        settings::get_setting(&state.db, "llm_classifier_enabled"),
        settings::get_setting(&state.db, "provider_health_probe_enabled"),
        settings::get_setting(&state.db, "provider_health_probe_interval_s"),
        settings::get_setting(&state.db, "tool_runner_enabled"),
        settings::get_setting(&state.db, "http_timeout_secs"),
        settings::get_setting(&state.db, "http_pool_max"),
    );

    // Inizializza il flag AtomicBool per il classificatore LLM.
    let llm_classifier_db = s_llm_classifier.ok().flatten()
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true);
    crate::orchestrator::set_llm_classifier_enabled(llm_classifier_db);
    tracing::info!(
        "llm_classifier_enabled: {} (fonte: DB{})",
        llm_classifier_db,
        if std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").is_ok() { " + env override attivo" } else { "" }
    );

    // Inizializza la configurazione HTTP globale (timeout, pool) dal DB.
    let http_timeout = s_http_timeout.ok().flatten()
        .and_then(|v| v.trim().parse::<u64>().ok());
    let http_pool = s_http_pool.ok().flatten()
        .and_then(|v| v.trim().parse::<usize>().ok());
    nexus_http::init_global_config(http_timeout, http_pool);

    // Worker `provider_health_probe`: pinga periodicamente i provider LLM
    // per rilevare cooldown / quota esaurita PRIMA del primo errore utente.
    // Valore canonico: settings.provider_health_probe_enabled/interval_s nel DB.
    // Override emergenza: NEXUS_PROVIDER_HEALTH_PROBE_ENABLED, NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S.
    let probe_enabled = s_health_probe_enabled.ok().flatten()
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true);
    let probe_interval = s_health_probe_interval.ok().flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(300);
    provider_health_probe::spawn_health_probe(
        std::sync::Arc::new(state.orchestrator.clone()),
        state.db.clone(),
        probe_enabled,
        probe_interval,
    );

    // Worker `task_watchdog`: monitora proattivamente le dipendenze
    // infrastrutturali (Qdrant, embedder) e rileva task background bloccati.
    // Disattivabile via env NEXUS_TASK_WATCHDOG_ENABLED=false.
    task_watchdog::spawn_task_watchdog(
        state.db.clone(),
        std::sync::Arc::new(state.orchestrator.clone()),
        state.dependency_status.clone(),
    );

    // Worker `model_catalog_sync`: ogni 24h aggiorna ai_price_catalog
    // dal JSON LiteLLM (prezzi, context_window, supports_function_calling).
    // Nuovi modelli scoperti vengono inseriti con is_enabled=false: l'admin
    // li attiva quando vuole dopo aver impostato tier/capabilities.
    // Primo run a 60s dall'avvio per avere tempo di stabilizzarsi.
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            loop {
                match models::run_catalog_sync(&db).await {
                    Ok((added, updated, skipped)) => tracing::info!(
                        "model_catalog_sync: added={} updated={} skipped={}",
                        added, updated, skipped
                    ),
                    Err(e) => tracing::warn!("model_catalog_sync fallito: {}", e),
                }
                tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
            }
        });
    }

    // ToolRunner gRPC server (Fase 1 refactor orchestrazione LangGraph).
    // Valore canonico: settings.tool_runner_enabled nel DB (admin panel).
    // Override emergenza: ENABLE_TOOL_RUNNER=1 (priorita' piu' alta del DB).
    {
        let env_override = std::env::var("ENABLE_TOOL_RUNNER").ok().as_deref() == Some("1");
        let db_enabled = s_tool_runner.ok().flatten()
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if env_override || db_enabled {
            let addr: SocketAddr = std::env::var("TOOL_RUNNER_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50071".to_string())
                .parse()
                .expect("TOOL_RUNNER_ADDR non valido");
            let deps = tool_runner_server::ToolRunnerDeps {
                db: state.db.clone(),
                neural: state.orchestrator.neural.clone(),
                agent_channels: state.agent_channels.clone(),
                terminal_consumers: state.terminal_consumers.clone(),
                template_cache: state.template_cache.clone(),
                dependency_status: state.dependency_status.clone(),
            };
            if let Err(e) = tool_runner_server::spawn_tool_runner_server(deps, addr).await {
                tracing::error!("ToolRunner server: avvio fallito: {e}");
            }
        } else {
            tracing::info!("ToolRunner gRPC: disabilitato (tool_runner_enabled=false in DB)");
        }
    }

    // AgentRouter gRPC server (Fase 5f refactor): espone il Q-Learning
    // router di nexus-orchestrator al brain.
    // Priorita' abilitazione: env ENABLE_AGENT_ROUTER=1 (override emergenza)
    // > settings.agent_router_enabled nel DB (admin panel) > default false.
    {
        let env_override = std::env::var("ENABLE_AGENT_ROUTER").ok().as_deref() == Some("1");
        let db_enabled = settings::get_setting(&state.db, "agent_router_enabled")
            .await
            .ok()
            .flatten()
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if env_override || db_enabled {
            let addr: SocketAddr = std::env::var("AGENT_ROUTER_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50072".to_string())
                .parse()
                .expect("AGENT_ROUTER_ADDR non valido");
            if let Err(e) = agent_router_server::spawn_agent_router_server(addr).await {
                tracing::error!("AgentRouter server: avvio fallito: {e}");
            }
        } else {
            tracing::info!("AgentRouter gRPC: disabilitato (agent_router_enabled=false in DB)");
        }
    }

    let frontend_origin =
        std::env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let cors = CorsLayer::new()
        .allow_origin(frontend_origin.parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::PATCH])
        .allow_headers([
            http_header::CONTENT_TYPE,
            http_header::AUTHORIZATION,
            http_header::ACCEPT,
            http_header::COOKIE,
        ])
        .allow_credentials(true);

    let app =
        Router::new()
            // Public routes (no auth)
            .route("/health", get(health))
            .route("/api/health", get(health))
            .route("/api/dashboard", get(dashboard))
            // Nexus (Fase 8) — observability endpoint pubblici
            .route("/nexus/healthz", get(nexus_bridge::nexus_healthz))
            .route("/nexus/stats", get(nexus_bridge::nexus_stats))
            .route("/nexus/tools", get(nexus_bridge::nexus_tools))
            .route("/nexus/metrics", get(nexus_bridge::nexus_prometheus))
            .route("/nexus/test-routing", post(nexus_bridge::nexus_test_routing))
            .route("/auth/github", get(auth::github_login))
            .route("/auth/github/callback", get(auth::github_callback))
            .route(
                "/auth/figma/mcp/callback",
                get(plugins::figma_oauth_callback),
            )
            .route("/internal/settings/:key", get(settings::get_raw_value))
            .route(
                "/internal/nexus-database-stats",
                get(nexus_database_stats::nexus_database_stats),
            )
            // /api/internal/routing/decide — esposto al brain Python per
            // eliminare la duplicazione della routing matrix. Vedi
            // crates/mcp-core/src/internal_routing.rs per il contratto.
            .route(
                "/api/internal/routing/decide",
                post(internal_routing::decide_routing).get(internal_routing::decide_routing_get),
            )
            // /api/internal/routing/catalog — Fase D consolidamento: espone
            // il catalogo prezzi LLM al brain Python e dashboard admin.
            // Filtri query: ?tier=heavy&provider=anthropic&requires_capability=tool_use
            .route(
                "/api/internal/routing/catalog",
                get(internal_routing::list_catalog),
            )
            .route(
                "/api/internal/routing/purpose",
                get(internal_routing::resolve_purpose),
            )
            // /api/internal/learning/feedback — sostituisce la chiamata gRPC
            // submit_feedback da brain Python. Rust diventa unico writer
            // della Q-table (vedi internal_learning.rs).
            .route(
                "/api/internal/learning/feedback",
                post(internal_learning::submit_feedback),
            )
            // /api/internal/provider-error — bridge cooldown: il brain Python
            // notifica errori provider non osservati da Rust (es. catena
            // classificatore). Applica cooldown appropriato (lungo per billing,
            // breve per rate_limit/overloaded).
            .route(
                "/api/internal/provider-error",
                post(internal_routing::provider_error_handler),
            )
            .route(
                "/api/internal/prompt-templates/batch-assign-tools",
                post(prompt_templates::internal_batch_assign_tools_handler),
            )
            .route(
                "/api/chat",
                post(chat_messages::legacy_chat).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/orchestrator/chat",
                post(chat_messages::legacy_chat).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/sessions",
                get(chat_sessions::list_chat_sessions)
                    .post(chat_sessions::create_chat_session)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/chat/sessions/:id",
                axum::routing::patch(chat_sessions::rename_chat_session)
                    .delete(chat_sessions::delete_chat_session)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/chat/sessions/:id/compact",
                axum::routing::post(chat_sessions::compact_chat_session).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
                ),
            )
            .route(
                "/api/projects/:id/memories",
                axum::routing::get(chat_sessions::list_project_memories).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
                ),
            )
            .route(
                "/api/memories/:id/toggle",
                axum::routing::patch(chat_sessions::toggle_project_memory).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
                ),
            )
            .route(
                "/api/chat/sessions/:id/messages",
                get(chat_messages::list_chat_messages)
                    .post(chat_messages::send_chat_message)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/chat/sessions/:id/agent-stream",
                get(chat_agent::agent_stream).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/sessions/:session_id/active-run",
                get(chat_agent::get_active_run_for_session).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/agent-runs/:run_id",
                get(chat_agent::get_agent_run).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/agent-runs/:run_id/confirm",
                post(chat_agent::confirm_agent_run).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/agent-runs/:run_id/cancel",
                post(chat_agent::cancel_agent_run).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/messages/:id/resend",
                post(chat_messages::resend_chat_message).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/messages/:id/feedback-error",
                post(chat_messages::feedback_error).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/precheck",
                post(chat_messages::precheck_chat_message).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/feedback-assist",
                post(chat_messages::feedback_assist_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/chat/messages/:id",
                delete(chat_messages::delete_chat_message).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            // Protected routes (require login)
            .route(
                "/api/me",
                get(auth::me).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/github/account",
                get(github::github_account)
                    .delete(github::github_disconnect)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/github/connect",
                post(github::github_connect).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/github/repositories",
                get(github::github_list_user_repositories).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/auth/logout",
                post(auth::logout).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/mine",
                get(projects::list_user_projects).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/fs/directories",
                get(projects::browse_server_directories).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/fs/directories/create",
                post(projects::create_server_directory).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/register",
                post(projects::register_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/clone",
                post(projects::clone_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/clone-target-exists",
                get(projects::clone_target_exists).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id",
                get(projects::get_project)
                    .delete(projects::delete_project)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/analyze",
                post(projects::analyze_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/deep-analyze",
                post(projects::deep_analyze_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/insights",
                get(projects::get_project_insights).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/custom-instructions",
                get(projects::get_custom_instructions)
                    .patch(projects::update_custom_instructions)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/default-profile",
                patch(projects::patch_project_default_profile)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/quality-scan",
                post(projects::run_quality_scan).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/quality-scan/:scan_id",
                get(projects::get_quality_scan_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/quality-findings",
                get(projects::get_quality_findings).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/quality-findings/:finding_id/mark-fixed",
                post(projects::mark_finding_fixed).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/quality-scan-file",
                post(projects::scan_single_file).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/file-lines",
                get(projects::get_file_lines).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/index-status",
                get(projects::get_index_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/reindex-stale",
                post(projects::reindex_stale_files).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/deep-review",
                post(projects::submit_deep_review).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/deep-review/:job_id",
                get(projects::get_deep_review_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/ai/generate-prompt",
                post(projects::generate_system_prompt).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/terminal-commands/stream",
                get(projects::terminal_commands_stream).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/terminal-commands/presence",
                post(projects::terminal_presence).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/terminal-commands/:command_id/ack",
                post(projects::terminal_command_ack).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/terminal-commands/:command_id/finish",
                post(projects::terminal_command_finish).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/agent-processes/:process_id/stop",
                post(project_workspace::stop_agent_process).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/agent-processes/clear-finished",
                post(project_workspace::clear_finished_processes).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/agent-processes/:process_id/stream",
                get(project_workspace::stream_agent_process_logs).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/open",
                post(project_workspace::open_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/workbench-state",
                get(project_workspace::get_workbench_state)
                    .put(project_workspace::update_workbench_state)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/terminal/session",
                post(project_workspace::create_terminal_session).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/preferences/git-ui",
                get(project_git::get_git_ui_preferences)
                    .put(project_git::update_git_ui_preferences)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/tree",
                get(project_files::get_project_tree).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/files",
                get(project_files::get_project_file)
                    .put(project_files::save_project_file)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/files/create",
                post(project_files::create_project_entry).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/files/rename",
                post(project_files::rename_project_entry).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/files/delete",
                post(project_files::delete_project_entry).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/search",
                get(project_files::search_project).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            // ── project database ────────────────────────────────────
            .route(
                "/api/projects/:id/db",
                get(project_db_routes::get_project_db_config).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/config",
                post(project_db_routes::set_project_db_config).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/migrations",
                get(project_db_routes::list_project_migrations).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/migrations/apply",
                post(project_db_routes::apply_project_migrations).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/migrations/rollback",
                post(project_db_routes::rollback_project_migration).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/override-request",
                post(project_db_routes::request_ddl_override).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/detect",
                post(project_db_routes::detect_project_db).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/test-connection",
                post(project_db_routes::test_project_db_connection).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/connections",
                get(project_db_routes::list_project_db_connections).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/db/connections/:conn_id/set-primary",
                post(project_db_routes::set_primary_project_db_connection).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
                ),
            )
            .route(
                "/api/projects/:id/db/connections/:conn_id",
                axum::routing::delete(project_db_routes::delete_project_db_connection).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
                ),
            )
            // ── documents ──────────────────────────────────────────
            .route(
                "/api/projects/:id/documents",
                get(documents::list_documents).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/documents/:doc_id",
                get(documents::get_document)
                    .delete(documents::delete_document)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/documents/:doc_id/download",
                get(documents::download_document).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/documents/:doc_id/versions",
                get(documents::list_versions).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/problems",
                get(project_workspace::get_project_problems).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/output/channels",
                get(project_workspace::get_output_channels).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/output/events",
                get(project_workspace::get_output_events).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/:service/:action",
                post(project_workspace::control_project_service).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services",
                get(project_workspace::get_project_services_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/wizard/detect",
                get(project_workspace::wizard_detect_services).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/wizard/install",
                post(project_workspace::wizard_install_service).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/restart-all",
                post(project_workspace::restart_all_project_services).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/cleanup-ports",
                post(project_workspace::cleanup_project_ports).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/services/:service",
                axum::routing::delete(project_workspace::uninstall_project_service).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/changes",
                get(project_workspace::get_project_changes).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/ports",
                get(project_workspace::get_project_ports).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/port-allocations",
                get(project_workspace::get_port_allocations)
                    .post(project_workspace::create_port_allocation)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/port-allocations/:port",
                axum::routing::delete(project_workspace::delete_port_allocation).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/playwright/runs",
                get(project_workspace::get_playwright_runs).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/run-configs/detect",
                get(project_workspace::detect_run_configs).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/run-configs",
                get(project_workspace::get_run_configs)
                    .post(project_workspace::create_run_config)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/run-configs/:config_id",
                put(project_workspace::update_run_config)
                    .delete(project_workspace::delete_run_config)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/projects/:id/run-configs/:config_id/launch",
                post(project_workspace::launch_run_config).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/status",
                get(project_git::git_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/log",
                get(project_git::git_log).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/diff",
                get(project_git::git_diff).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/branches",
                get(project_git::git_branches).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/stage",
                post(project_git::git_stage).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/unstage",
                post(project_git::git_unstage).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/commit",
                post(project_git::git_commit).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/checkout",
                post(project_git::git_checkout).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/branch",
                post(project_git::git_create_branch).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/pull",
                post(project_git::git_pull).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/git/push",
                post(project_git::git_push).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/github/status",
                get(github::github_project_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/github/repositories",
                get(github::github_list_repositories).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/github/clone",
                post(github::github_clone_repository).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/github/publish-branch",
                post(github::github_publish_branch).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/github/pull-request",
                post(github::github_create_pull_request).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            // MCP Connectors (per-user)
            .route(
                "/api/mcp-servers",
                get(mcp_connectors::list_mcp_servers)
                    .post(mcp_connectors::create_mcp_server)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/mcp-servers/:id",
                put(mcp_connectors::update_mcp_server)
                    .delete(mcp_connectors::delete_mcp_server)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_auth,
                    )),
            )
            .route(
                "/api/mcp-servers/:id/toggle",
                put(mcp_connectors::toggle_mcp_server).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/mcp-servers/:id/test",
                post(mcp_connectors::test_mcp_server).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            // Profili utente (GPT/Gem style)
            .route(
                "/api/profiles",
                get(profiles::list_profiles)
                    .post(profiles::create_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_auth)),
            )
            .route(
                "/api/profiles/:id",
                put(profiles::update_profile)
                    .delete(profiles::delete_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_auth)),
            )
            .route(
                "/api/profiles/:id/default",
                post(profiles::set_default_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_auth)),
            )
            .route(
                "/api/profiles/:id/fork",
                post(profiles::fork_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_auth)),
            )
            // Plugin Manager (MCP-first)
            .route(
                "/api/plugins/catalog",
                get(plugins::list_plugin_catalog).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/installed",
                get(plugins::list_installed_plugins).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/figma/oauth/status",
                get(plugins::get_figma_oauth_status).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/figma/oauth/connect",
                post(plugins::start_figma_oauth).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/install",
                post(plugins::install_plugin).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id/update",
                post(plugins::update_plugin).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id",
                delete(plugins::uninstall_plugin).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id/toggle",
                put(plugins::toggle_plugin).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id/test",
                post(plugins::test_plugin).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id/health",
                get(plugins::get_plugin_health).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/:id/tool-policy",
                put(plugins::update_plugin_tool_policy).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/plugins/migrate-legacy/:id",
                post(plugins::migrate_legacy_mcp_server).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/admin/plugins/integrate/draft",
                post(plugins::draft_plugin_integration).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/plugins/integrate/publish",
                post(plugins::publish_plugin_integration).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            // Admin routes (require admin role)
            .route(
                "/api/admin/settings",
                get(settings::list_settings)
                    .put(settings::bulk_update)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_admin,
                    )),
            )
            .route(
                "/api/admin/fs/directories",
                get(settings::browse_directories).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/fs/directories/create",
                post(settings::create_directory).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/setting/:key",
                put(settings::update_setting).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/settings-by-category/:category",
                get(settings::list_by_category).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/billing/prices",
                get(billing::list_prices).post(billing::create_price).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            .route(
                "/api/admin/billing/prices/:id",
                put(billing::update_price).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/billing/quotas",
                get(billing::list_quotas).post(billing::create_quota).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            .route(
                "/api/admin/billing/quotas/:id",
                put(billing::update_quota).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/billing/usage",
                get(billing::admin_usage_report).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            // Admin — gestione profili di sistema
            .route(
                "/api/admin/profiles",
                get(profiles::admin_list_profiles)
                    .post(profiles::admin_create_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/profiles/:id",
                put(profiles::admin_update_profile)
                    .delete(profiles::admin_delete_profile)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Admin — profili custom degli utenti (read-only)
            .route(
                "/api/admin/user-profiles",
                get(profiles::admin_list_user_profiles)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/feedback/errors",
                get(chat_learning::admin_list_feedback_errors).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/feedback/:id/review",
                post(chat_learning::admin_review_feedback).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/learning/projects/:id/retrain-routing",
                post(chat_learning::admin_retrain_project_routing).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            .route(
                "/api/admin/learning/projects/:id/config",
                get(chat_learning::admin_get_project_learning_config)
                    .put(chat_learning::admin_update_project_learning_config)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_admin,
                    )),
            )
            .route(
                "/api/admin/vector/compact",
                post(chat_learning::admin_run_vector_compaction).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            .route(
                "/api/admin/vector/compact/runs",
                get(chat_learning::admin_list_vector_compaction_runs).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            // Prompt corrections (prompt_corrections Qdrant + Postgres)
            .route(
                "/api/admin/prompt-corrections",
                get(chat_learning::admin_list_prompt_corrections)
                    .post(chat_learning::admin_create_prompt_correction)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_admin,
                    )),
            )
            .route(
                "/api/admin/prompt-corrections/:id",
                delete(chat_learning::admin_delete_prompt_correction).layer(
                    axum_mw::from_fn_with_state(state.clone(), middleware::require_admin),
                ),
            )
            // Long-running patterns CRUD
            .route(
                "/api/admin/long-running",
                get(long_running::list_patterns)
                    .post(long_running::create_pattern)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/long-running/:id",
                put(long_running::update_pattern)
                    .delete(long_running::delete_pattern)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/sync-model-catalog",
                post(models::sync_model_catalog)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Admin users management
            .route(
                "/api/admin/users",
                get(admin::users::list_users)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/users/search",
                get(admin::users::search_users)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/users/:user_id",
                get(admin::users::get_user)
                    .put(admin::users::update_user)
                    .delete(admin::users::delete_user)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/users/:user_id/role",
                put(admin::users::update_user_role)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Admin projects management
            .route(
                "/api/admin/projects",
                get(admin::projects::list_all_projects)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/projects/port",
                post(admin::projects::port_projects)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/projects/:project_id/members",
                get(admin::projects::list_project_members)
                    .post(admin::projects::add_project_member)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/projects/:project_id/members/:user_id",
                put(admin::projects::update_project_member)
                    .delete(admin::projects::remove_project_member)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Environment status & fix
            .route(
                "/api/admin/environment/status",
                get(environment::get_environment_status)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/environment/fix",
                post(environment::fix_environment)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/qdrant-health",
                get(environment::qdrant_health_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/watchdog-status",
                get(task_watchdog::watchdog_status_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/embeddings/validate",
                post(environment::embeddings_validate_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/embeddings/apply",
                post(environment::embeddings_apply_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Admin — purpose models (nexus_purpose_model)
            .route(
                "/api/admin/routing/purpose-models",
                get(admin::routing::list_purpose_models)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/routing/purpose-model/:purpose",
                put(admin::routing::update_purpose_model)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/gateway/providers",
                get(environment::gateway_providers_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/gateway/reload",
                post(environment::gateway_reload_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            // Retrocompatibilità: percorsi /api/admin/gateway/* per il frontend vecchio
            // (rimosso dopo che il frontend sarà stato ridistribuito con i nuovi percorsi)
            .route(
                "/api/admin/gateway/providers",
                get(environment::gateway_providers_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/admin/gateway/reload",
                post(environment::gateway_reload_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_admin)),
            )
            .route(
                "/api/models",
                get(models::list_models).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/models/routing-preview",
                get(models::routing_preview).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/billing/usage/me",
                get(billing::my_usage_report).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/projects/:id/billing/usage",
                get(billing::project_usage_report).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/billing/session-usage",
                get(billing::get_session_usage).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            // Prompt Templates
            .route(
                "/api/prompt-templates",
                get(prompt_templates::list_templates_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/prompt-templates/:key",
                get(prompt_templates::get_template_handler)
                    .put(prompt_templates::upsert_template_handler)
                    .layer(axum_mw::from_fn_with_state(state.clone(), middleware::require_auth)),
            )
            .route(
                "/api/prompt-templates/:key/disable",
                post(prompt_templates::disable_template_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/prompt-templates/:key/enable",
                post(prompt_templates::enable_template_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/admin/prompt-templates/batch-assign-tools",
                post(prompt_templates::batch_assign_tools_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/available-mcp-tools",
                get(prompt_templates::available_mcp_tools_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
            )
            .route(
                "/api/admin/prompt-templates/:key/tools",
                get(prompt_templates::get_prompt_tools_handler)
                    .put(prompt_templates::update_prompt_tools_handler)
                    .layer(axum_mw::from_fn_with_state(
                        state.clone(),
                        middleware::require_admin,
                    )),
            )
            .route(
                "/api/prompt-templates/:key/ai-suggest",
                post(prompt_templates::ai_suggest_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/quality/findings/:id/false-positive",
                post(prompt_templates::mark_false_positive_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .route(
                "/api/quality/false-positive-stats",
                get(prompt_templates::false_positive_stats_handler).layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
            )
            .with_state(state)
            .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], 4000));
    tracing::info!("mcp-core listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Graceful shutdown: SIGTERM o Ctrl-C → flush NexusBridge (Q-table + replication)
    // prima che il processo termini, per evitare perdita dati in-flight.
    let shutdown_signal = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut terminate = signal(SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Ctrl-C ricevuto — avvio graceful shutdown");
                },
                _ = terminate.recv() => {
                    tracing::info!("SIGTERM ricevuto — avvio graceful shutdown");
                },
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Ctrl-C ricevuto — avvio graceful shutdown");
        }

        // Flush NexusBridge state (Q-table + replication:pending → PostgreSQL)
        if let Some(bridge) = nexus_bridge::NexusBridge::global() {
            bridge.shutdown().await;
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<HealthSummary> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    let redis_ok: bool = redis::cmd("PING")
        .query_async::<String>(&mut state.redis.clone())
        .await
        .map(|r| r == "PONG")
        .unwrap_or(false);

    // Verifica TCP connect rapido al ToolRunner gRPC: senza questo l'AI non può
    // invocare tool (read_file, str_replace…) e fallisce silenziosamente.
    let tools_grpc_ok = {
        let addr = std::env::var("TOOL_RUNNER_ADDR").unwrap_or_else(|_| "127.0.0.1:50071".into());
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    };

    let status = if db_ok && redis_ok && tools_grpc_ok { "ok" } else { "degraded" };

    Json(HealthSummary {
        service: "mcp-core".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_time: env!("BUILD_TIMESTAMP").to_string(),
        status: status.to_string(),
        timestamp: Utc::now(),
        components: domain::ComponentHealth {
            database: db_ok,
            redis: redis_ok,
            neural_core: state.orchestrator.neural_healthy().await,
            tools_grpc: tools_grpc_ok,
            qdrant: state.dependency_status.qdrant.load(std::sync::atomic::Ordering::Relaxed),
            embedder: state.dependency_status.embedder.load(std::sync::atomic::Ordering::Relaxed),
        },
    })
}

async fn dashboard(State(state): State<AppState>) -> Json<serde_json::Value> {
    let token_stats = sqlx::query_as::<_, domain::TokenStats>(
        r#"
        SELECT
            COALESCE(SUM(total_tokens), 0) AS total_consumed,
            COALESCE(SUM(total_cost), 0) AS total_cost
        FROM ai_usage_ledger
        WHERE created_at > NOW() - INTERVAL '30 days'
          AND status = 'finalized'
        "#,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let quality_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM quality_findings WHERE status = 'open'")
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    let shadow_db_status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM jobs WHERE job_type = 'shadow_db_validation' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "no_runs".to_string());

    let stats = token_stats.unwrap_or_default();

    let total_runs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM orchestrator_runs WHERE created_at > NOW() - INTERVAL '30 days'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let active_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status IN ('queued', 'running')")
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    Json(json!({
        "tokenUsage": {
            "consumed": stats.total_consumed,
            "saved": 0
        },
        "costUsage": {
            "total": stats.total_cost
        },
        "quality": {
            "findings": quality_count,
            "shadowDbStatus": shadow_db_status
        },
        "total_runs": total_runs,
        "tokens_consumed": stats.total_consumed,
        "tokens_saved": 0,
        "total_cost": stats.total_cost,
        "quality_findings": quality_count,
        "active_jobs": active_jobs,
    }))
}
