mod admin;
mod agent_processes;
mod agent_router_server;
mod agent_todos_routes;
mod agent_tool_result_cache;
mod agent_tools;
mod agent_types;
mod auth;
mod billing;
mod brain_agent_client;
mod build_graph;
mod cache;
mod catalog_sync_worker;
mod change_drafts;
mod chat_agent;
mod chat_attachments;
mod chat_learning;
mod chat_messages;
mod chat_sessions;
mod claude_agents;
mod context_settings;
mod db;
mod deepseek_balance_sync;
mod dispatcher_routes;
mod dlp;
mod documents;
mod domain;
mod environment;
mod github;
mod internal_learning;
mod internal_routing;
mod long_running;
mod mcp_client;
mod mcp_connectors;
mod middleware;
mod model_catalog_sync;
mod model_health_probe;
mod models;
mod nexus_autofix_worker;
mod nexus_bridge;
mod nexus_builtin;
mod nexus_database_stats;
mod nexus_gateway;
mod nexus_routing;
mod nexus_tool_catalog;
mod nexus_tools;
mod orchestrator;
pub mod playwright_live;
mod plugins;
mod port_registry;
mod profiles;
mod project_context;
mod project_db;
mod project_db_routes;
mod project_files;
mod project_git;
mod project_workspace;
mod projects;
mod prompt_templates;
mod provider_cooldown;
mod provider_error_classifier;
mod provider_health_probe;
mod quality_guard;
mod rag;
mod routes;
mod routing_config;
mod routing_matrix;
mod routing_matrix_auto_promoter;
mod routing_slots;
mod sandbox;
mod security;
mod services_watchdog;
mod settings;
mod static_preview;
mod sudo_manager;
mod sudo_routes;
mod task_watchdog;
mod tool_runner_server;
mod vector_memory;
mod wiki;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{header as http_header, HeaderValue, Method};
use axum::{
    extract::{DefaultBodyLimit, State},
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
    /// Channel per stream live degli eventi Playwright (run live monitoring).
    /// Chiave: job_id (UUID di una riga `jobs` con kind='playwright_test').
    playwright_channels: playwright_live::PlaywrightChannels,
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
    /// Dispatcher centrale di eventi cross-pannello.
    /// Un canale broadcast per project_id, riceve tutti gli eventi rilevanti
    /// (jobs, ports, problems, services, files, git, flags, monitor).
    /// Vedi `crates/nexus-events/`.
    pub(crate) project_channels: nexus_events::ProjectChannels,
    /// Mappa monitor in-memory per project_id. Aggiornata dal tool agente
    /// `dispatcher_update_monitor` ed esposta nel snapshot bootstrap.
    /// `monitor_id -> { value, label }`.
    pub(crate) monitor_registry: Arc<
        parking_lot::RwLock<
            std::collections::HashMap<Uuid, std::collections::HashMap<String, serde_json::Value>>,
        >,
    >,
}

#[tokio::main(worker_threads = 32)]
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

    // Porta HTTP dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    // Risolta subito dopo il pool: se il DB e' down panica qui (coerente con
    // RoutingMatrixCache::init poco sotto).
    let mcp_http_port = nexus_auth::resolve_port(&db, "mcp_core_http_port").await;

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
            "SELECT id, pid FROM agent_processes WHERE status IN ('running', 'starting')",
        )
        .fetch_all(&db)
        .await
        .unwrap_or_default();

        for row in stale {
            let id: uuid::Uuid = row.get("id");
            let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
            let still_alive = match pid {
                Some(p) => tokio::process::Command::new("kill")
                    .args(["-0", &p.to_string()])
                    .status()
                    .await
                    .map(|s| s.success())
                    .unwrap_or(false),
                None => false,
            };

            if !still_alive {
                let _ = sqlx::query(
                    "UPDATE agent_processes SET status='failed', stopped_at=NOW() WHERE id=$1",
                )
                .bind(id)
                .execute(&db)
                .await;
                tracing::info!("Stale process {} (pid={:?}) marked failed", id, pid);
            } else {
                tracing::info!(
                    "Process {} (pid={:?}) still running, re-attaching monitor",
                    id,
                    pid
                );
                // still_alive=true implica pid.is_some(); difensivamente saltiamo
                // la re-attach se la condizione viene violata in futuro.
                let Some(pid_val) = pid else {
                    tracing::warn!("Process {} alive ma pid=None: skip re-attach", id);
                    continue;
                };
                let db_clone = db.clone();
                // Rilancia un task che segue stdout+stderr tramite /proc/{pid}/fd/1,2
                tokio::spawn(async move {
                    let stdout_path = format!("/proc/{}/fd/1", pid_val);
                    let stderr_path = format!("/proc/{}/fd/2", pid_val);
                    let mut child = match tokio::process::Command::new("tail")
                        .args([
                            "-f",
                            "--pid",
                            &pid_val.to_string(),
                            &stdout_path,
                            &stderr_path,
                        ])
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
                                let alive = tokio::process::Command::new("kill")
                                    .args(["-0", &pid_val.to_string()])
                                    .status()
                                    .await
                                    .map(|s| s.success())
                                    .unwrap_or(false);
                                if !alive {
                                    let exit_code: Option<i32> = tokio::process::Command::new("sh")
                                        .args([
                                            "-c",
                                            &format!(
                                                "cat /proc/{}/status 2>/dev/null | grep -c VmPeak",
                                                pid_val
                                            ),
                                        ])
                                        .output()
                                        .await
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
                        let mut flush_tick =
                            tokio::time::interval(std::time::Duration::from_secs(2));
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
                                let provider =
                                    key.strip_prefix("nexus:billing_cooldown:").unwrap_or(&key);
                                crate::provider_cooldown::restore_cooldown(
                                    provider, remaining, reason,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Restore COMPLEMENTARE dal DB persistente (ADR 0020): se Redis e' stato
    // svuotato/riavviato, il blocco sopra non ripristina nulla e il gate parte
    // VUOTO -> il primo run dopo il restart "scopre" i provider esausti
    // chiamandoli (anthropic 400 / openai 429 ad ogni turno). nexus_provider_health
    // e' la fonte persistente piu' affidabile: riallinea il gate allo stato noto
    // cosi' il run li salta senza ri-testarli (il polling resta l'unico tester).
    crate::provider_cooldown::restore_billing_cooldowns_from_db(&db).await;

    let neural_client = {
        let mut attempts = 0u32;
        // Resilienza all'avvio (regola H): il brain (Neural Core) puo' avere un
        // cold start lento (build debug, warmup Vertex) o subire un hang
        // transitorio del filesystem 9p su WSL. Il vecchio loop si arrendeva
        // dopo 10 tentativi x 2s = 20s, facendo USCIRE mcp-core in cascata
        // quando il brain non era ancora pronto (instabilita' ricorrente: mcp-core
        // morto, da rilanciare a mano). Ora attendiamo molto piu' a lungo con
        // backoff (2s -> cap 10s, ~9 minuti) prima di arrenderci davvero.
        const MAX_ATTEMPTS: u32 = 60;
        loop {
            match NeuralCoreClient::connect(&neural_core_url).await {
                Ok(c) => {
                    if attempts > 0 {
                        tracing::info!("Neural Core connesso dopo {attempts} tentativi");
                    }
                    break c;
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= MAX_ATTEMPTS {
                        anyhow::bail!(
                            "Failed to connect to Neural Core after {attempts} attempts: {e}"
                        );
                    }
                    let backoff = std::cmp::min(2 + attempts as u64, 10);
                    tracing::warn!(
                        "Neural Core not ready (attempt {attempts}/{MAX_ATTEMPTS}): {e} — retry in {backoff}s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
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

    // ADR 0020: cache build graph (mig 0312). Inizializzata come singleton
    // globale, non panica se DB down (lazy: la prima get_or_compute provera').
    build_graph::BuildGraphCache::init_global(db.clone()).await;
    tracing::info!("build_graph::BuildGraphCache inizializzato (TTL configurabile via settings)");
    // Recovery: sincronizza registro con file .service esistenti su disco.
    port_registry_cache.startup_recovery().await;
    // GC periodico: rilascia le allocazioni porta dynamic orfane (nessun
    // listener, oltre la grace period) lasciate dai tentativi falliti degli
    // agenti. Intervallo/grace DB-driven con default sicuri.
    {
        let db_gc = db.clone();
        let gc_interval = settings::get_setting(&db, "agent.port_gc.interval_seconds")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(120);
        let gc_grace = settings::get_setting(&db, "agent.port_gc.grace_seconds")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(180);
        tokio::spawn(async move {
            port_registry::port_gc_loop(db_gc, gc_interval, gc_grace).await;
        });
    }

    // ADR 0017 v2 F8: rimosso `ensure_knowledge_collection` (collection
    // legacy `knowledge_notes`). La collection unificata `wiki_content` viene
    // garantita lazily dalle funzioni `vector_memory::ensure_wiki_content_collection`
    // / `upsert_wiki_content_point` al primo write del re-ingest bootstrap.

    // URL gateway dalla porta nel DB (regola G: niente env/hardcoded).
    let gw_port = nexus_auth::resolve_port(&db, "nexus_gateway_port").await;
    let gw_url = format!("http://127.0.0.1:{gw_port}");
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
        if sandbox_available {
            "attiva"
        } else {
            "non disponibile"
        },
        if sandbox_available {
            "isolato in container nexus-sandbox"
        } else {
            "eseguito con env filtrato (fallback)"
        }
    );

    let state = AppState {
        db,
        redis,
        orchestrator,
        agent_channels: Arc::new(DashMap::new()),
        playwright_channels: playwright_live::new_channels(),
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
        project_channels: nexus_events::dispatcher::new_registry(),
        monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
    };
    // Singleton globale per emit da contesti senza &ProjectChannels (NexusToolHandler).
    nexus_events::dispatcher::init_global(state.project_channels.clone());

    chat_learning::spawn_vector_compaction_scheduler(state.clone());
    nexus_builtin::seed_tools_and_server(&state.db).await;
    // Reindex semantico Qdrant dei tool MCP (fire-and-forget, +30s delay).
    // Indicizza solo i tool con embedding mancante o hash cambiato.
    nexus_builtin::spawn_tool_reindex(state.db.clone(), state.orchestrator.neural.clone());

    // ── ADR 0017 v2 F3 — re-ingest automatico se `wiki_docs` e' vuota ─────
    // Bootstrap one-shot: alla prima esecuzione dopo la migrazione 0295 la
    // tabella e' vuota e i vault Markdown sono la sola fonte di verita'.
    // Lo lanciamo come task background (non blocca lo startup HTTP) e logga
    // il `ReingestReport` al termine.
    {
        let state_bootstrap = state.clone();
        tokio::spawn(async move {
            let empty: bool = match sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM wiki_docs",
            )
            .fetch_one(&state_bootstrap.db)
            .await
            {
                Ok(n) => n == 0,
                Err(e) => {
                    tracing::warn!(error = %e, "wiki.bootstrap: COUNT wiki_docs fallita, skip");
                    return;
                }
            };
            if !empty {
                tracing::debug!("wiki.bootstrap: wiki_docs gia' popolata, skip re-ingest auto");
                return;
            }
            tracing::info!("wiki: tabella vuota, lancio re-ingest automatico dai vault");
            match wiki::reingest::reingest_all(&state_bootstrap, None, None).await {
                Ok(report) => {
                    tracing::info!(
                        meta = report.meta_docs_ingested,
                        projects = report.project_docs_ingested_by_project.len(),
                        skipped = report.files_skipped,
                        errors = report.errors.len(),
                        elapsed_ms = report.elapsed_ms,
                        "wiki.bootstrap: re-ingest completato"
                    );
                    if !report.errors.is_empty() {
                        for err in report.errors.iter().take(10) {
                            tracing::warn!(error = %err, "wiki.bootstrap: errore re-ingest");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "wiki.bootstrap: re-ingest fallito");
                }
            }
        });
    }

    // ── ADR 0017 v2 F4 — auto-recompute link al primo avvio post-F3 ───────
    // Se `wiki_docs > 0` ma `wiki_links == 0` significa che la mig 0295 e' stata
    // applicata, F3 ha popolato i doc, ma F4 non e' mai stato eseguito. Lo
    // lanciamo in background per non bloccare l'HTTP. Il task aspetta 60s per
    // dare tempo al re-ingest F3 di completare (se in corso).
    {
        let state_links = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let counts: Option<(i64, i64)> = sqlx::query_as::<_, (i64, i64)>(
                "SELECT (SELECT COUNT(*) FROM wiki_docs), (SELECT COUNT(*) FROM wiki_links)",
            )
            .fetch_optional(&state_links.db)
            .await
            .ok()
            .flatten();
            let Some((docs, links)) = counts else {
                tracing::warn!("wiki.links.bootstrap: COUNT fallita, skip");
                return;
            };
            if docs == 0 {
                tracing::debug!("wiki.links.bootstrap: wiki_docs vuota, skip recompute");
                return;
            }
            if links > 0 {
                tracing::debug!(
                    links = links,
                    "wiki.links.bootstrap: wiki_links gia' popolata, skip recompute auto"
                );
                return;
            }
            tracing::info!(
                docs = docs,
                "wiki.links.bootstrap: avvio recompute automatico (scope=meta)"
            );
            match wiki::links_worker::recompute_links_for_scope(
                &state_links,
                Some(wiki::model::WikiScope::Meta),
                None,
            )
            .await
            {
                Ok(rep) => tracing::info!(
                    scanned = rep.docs_scanned,
                    wikilinks = rep.wikilinks_resolved,
                    semantic_new = rep.semantic_links_created,
                    semantic_upd = rep.semantic_links_updated,
                    errors = rep.errors.len(),
                    elapsed_ms = rep.elapsed_ms,
                    "wiki.links.bootstrap: completato"
                ),
                Err(e) => tracing::error!(error = %e, "wiki.links.bootstrap: fallito"),
            }
        });
    }

    // ── ADR 0017 v2 F5 — worker periodico LLM triple extraction ───────────
    // Non lanciamo full re-extract automatica al primo boot (costo ~$0.14):
    // l'admin la triggera manualmente via POST /api/wiki/extract-triples?wait=true
    // o lascia che il worker periodico processi un batch ogni interval_secs
    // rispettando i cap diurni configurati in settings.
    wiki::triple_extractor::start_triple_extractor_worker(state.clone());

    // ── ADR 0017 v2 TODO 1 — watcher bidirezionale vault->DB ──────────────
    // Osserva `docs/.nexus-vault/` e `<project_root>/.nexus-vault/` per i
    // progetti registrati al boot. Su evento Create/Modify di un file `.md`
    // chiama `wiki::reingest::reingest_path`. Settings DB-driven (mig 0301).
    // TODO: i progetti registrati post-startup vengono osservati solo dopo un
    // restart di mcp-core (vedi nota in `wiki::watcher`).
    wiki::watcher::start_wiki_watcher(std::sync::Arc::new(state.clone()));

    // ── ADR 0017 v2 TODO 6+7 — chat-note + run-summary worker ─────────────
    // Due loop periodici (delay iniziale rispettivamente 60s e 90s) che
    // ingestano i messaggi chat utente e i resoconti dei run terminali come
    // wiki_docs (kind='chat_note' / 'run_summary'). Settings DB-driven sotto
    // chiave `agent.wiki.chat_note_*` e `agent.wiki.run_summary_*` (mig 0305).
    wiki::chat_note_worker::start_chat_note_worker(std::sync::Arc::new(state.clone()));
    wiki::run_summary_worker::start_run_summary_worker(std::sync::Arc::new(state.clone()));

    // ── ADR 0017 v2 — worker periodici link + titoli su TUTTI gli scope ───
    // Loop DB-driven (interval `agent.wiki.link_worker_interval_secs` /
    // `agent.wiki.title_gen_interval_secs`) che processano scope=meta E lo
    // scope=project di ogni progetto registrato. Prima esisteva solo il
    // bootstrap one-shot dei link (scope=meta) e gli endpoint manuali: i
    // progetti restavano senza link/titoli finche' non triggerati a mano. Il
    // cap diurno del title_gen resta applicato per-scope (no spam LLM).
    wiki::links_worker::start_links_worker(std::sync::Arc::new(state.clone()));
    wiki::title_gen::start_title_gen_worker(std::sync::Arc::new(state.clone()));

    // ── PR hardening: avvio writer audit centralizzato + port enforcer ───
    // Audit writer: consuma il canale `record_audit(...)` e fa batch INSERT
    // in `nexus_resource_audit` ogni 100 eventi o 5s.
    security::audit::start_writer(state.db.clone());
    // Port enforcer: scansiona porte TCP in LISTEN ogni 5s, killa processi
    // di progetto che bindano porte fuori dal bucket assegnato.
    // Bug 7 fix (mig 0182): worker che sincronizza ai_price_catalog con i
    // modelli realmente esposti dalle API provider. Evita che la routing
    // matrix punti a modelli deprecati (es. DeepSeek v3 ora rimosso dall'API).
    {
        let db_sync = state.db.clone();
        let orch_sync = std::sync::Arc::new(state.orchestrator.clone());
        tokio::spawn(async move {
            model_catalog_sync::catalog_sync_loop(db_sync, Some(orch_sync)).await;
        });
    }

    tokio::spawn(security::port_enforcer::port_enforcer_loop(
        state.db.clone(),
        state.project_channels.clone(),
    ));

    // Il worker billing_cooldown_recovery_loop viene avviato piu' sotto, dopo
    // aver inizializzato la config health/cooldown provider DB-driven (cosi'
    // riceve l'interval dai settings invece di un valore hardcoded).

    // ── Lettura batch settings DB ────────────────────────────────────────────
    // Leggiamo in parallelo tutti i flag comportamentali dalla tabella settings.
    // I valori sono usati nei blocchi successivi. Le env var restano come
    // override di emergenza con priorita' piu' alta (gestita in ogni blocco).
    let (
        s_llm_classifier,
        s_health_probe_enabled,
        s_health_probe_interval,
        s_tool_runner,
        s_tool_runner_addr,
        s_http_timeout,
        s_http_pool,
        s_model_health_enabled,
        s_model_health_interval,
        s_model_health_threshold,
        s_catalog_sync_enabled,
        s_catalog_sync_interval,
    ) = tokio::join!(
        settings::get_setting(&state.db, "llm_classifier_enabled"),
        settings::get_setting(&state.db, "provider_health_probe_enabled"),
        settings::get_setting(&state.db, "provider_health_probe_interval_s"),
        settings::get_setting(&state.db, "tool_runner_enabled"),
        settings::get_setting(&state.db, "tool_runner_addr"),
        settings::get_setting(&state.db, "http_timeout_secs"),
        settings::get_setting(&state.db, "http_pool_max"),
        settings::get_setting(&state.db, "model_health_probe_enabled"),
        settings::get_setting(&state.db, "model_health_probe_interval_s"),
        settings::get_setting(&state.db, "model_health_probe_failure_threshold"),
        settings::get_setting(&state.db, "model_catalog_sync_enabled"),
        settings::get_setting(&state.db, "model_catalog_sync_interval_s"),
    );

    // Indirizzo ToolRunner: env var (override emergenza) > DB > hardcoded.
    let tool_runner_addr_str = std::env::var("TOOL_RUNNER_ADDR")
        .ok()
        .or_else(|| {
            s_tool_runner_addr
                .ok()
                .flatten()
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "127.0.0.1:50071".to_string());

    // Inizializza il flag AtomicBool per il classificatore LLM.
    let llm_classifier_db = s_llm_classifier
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    crate::orchestrator::set_llm_classifier_enabled(llm_classifier_db);
    tracing::info!(
        "llm_classifier_enabled: {} (fonte: DB{})",
        llm_classifier_db,
        if std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").is_ok() {
            " + env override attivo"
        } else {
            ""
        }
    );

    // Inizializza la configurazione HTTP globale (timeout, pool) dal DB.
    let http_timeout = s_http_timeout
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok());
    let http_pool = s_http_pool
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<usize>().ok());
    nexus_http::init_global_config(http_timeout, http_pool);

    // ── Config health/cooldown provider DB-driven (regola G, migrazione 0252) ──
    // Tempi di cooldown, circuit breaker, timeout probe e cadenza del recovery
    // loop letti dalla tabella settings. Partiamo dai default storici e
    // sovrascriviamo solo le chiavi presenti, cosi' un setting mancante non
    // cambia il comportamento.
    {
        let mut pht = provider_cooldown::ProviderHealthTimings::default();
        let p_u64 = |opt: Option<String>, d: u64| -> u64 {
            opt.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(d)
        };
        let p_usize = |opt: Option<String>, d: usize| -> usize {
            opt.and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(d)
        };
        let (
            s_recov_interval,
            s_recov_probe_to,
            s_cd_default,
            s_cd_min,
            s_cd_max,
            s_cd_long,
            s_cb_window,
            s_cb_threshold,
            s_cb_ext,
            s_probe_to,
            s_slow_cd,
            s_outage,
        ) = tokio::join!(
            settings::get_setting(&state.db, "provider.billing_recovery_interval_s"),
            settings::get_setting(&state.db, "provider.recovery_probe_timeout_s"),
            settings::get_setting(&state.db, "provider.cooldown_default_s"),
            settings::get_setting(&state.db, "provider.cooldown_min_s"),
            settings::get_setting(&state.db, "provider.cooldown_max_s"),
            settings::get_setting(&state.db, "provider.cooldown_long_s"),
            settings::get_setting(&state.db, "provider.circuit_breaker_window_s"),
            settings::get_setting(&state.db, "provider.circuit_breaker_threshold"),
            settings::get_setting(&state.db, "provider.circuit_breaker_extended_cooldown_s"),
            settings::get_setting(&state.db, "provider.health_probe_timeout_s"),
            settings::get_setting(&state.db, "provider.slow_cooldown_s"),
            settings::get_setting(&state.db, "provider.outage_threshold"),
        );
        pht.billing_recovery_interval_s = p_u64(
            s_recov_interval.ok().flatten(),
            pht.billing_recovery_interval_s,
        );
        pht.recovery_probe_timeout_s = p_u64(
            s_recov_probe_to.ok().flatten(),
            pht.recovery_probe_timeout_s,
        );
        pht.cooldown_default_s = p_u64(s_cd_default.ok().flatten(), pht.cooldown_default_s);
        pht.cooldown_min_s = p_u64(s_cd_min.ok().flatten(), pht.cooldown_min_s);
        pht.cooldown_max_s = p_u64(s_cd_max.ok().flatten(), pht.cooldown_max_s);
        pht.cooldown_long_s = p_u64(s_cd_long.ok().flatten(), pht.cooldown_long_s);
        pht.circuit_breaker_window_s =
            p_u64(s_cb_window.ok().flatten(), pht.circuit_breaker_window_s);
        pht.circuit_breaker_threshold =
            p_usize(s_cb_threshold.ok().flatten(), pht.circuit_breaker_threshold);
        pht.circuit_breaker_extended_cooldown_s = p_u64(
            s_cb_ext.ok().flatten(),
            pht.circuit_breaker_extended_cooldown_s,
        );
        pht.health_probe_timeout_s = p_u64(s_probe_to.ok().flatten(), pht.health_probe_timeout_s);
        pht.slow_cooldown_s = p_u64(s_slow_cd.ok().flatten(), pht.slow_cooldown_s);
        pht.outage_threshold = p_usize(s_outage.ok().flatten(), pht.outage_threshold);
        provider_cooldown::init_provider_health_timings(pht);
        tracing::info!(
            "provider health timings (DB): recovery_interval={}s probe_timeout={}s cooldown_long={}s outage_threshold={}",
            pht.billing_recovery_interval_s, pht.recovery_probe_timeout_s,
            pht.cooldown_long_s, pht.outage_threshold,
        );

        // Worker billing_cooldown_recovery_loop: riabilita i provider a cooldown
        // scaduto SOLO dopo un probe attivo andato a buon fine (probe-then-reenable).
        let db_billing = state.db.clone();
        let orch_billing = std::sync::Arc::new(state.orchestrator.clone());
        let recov_interval = pht.billing_recovery_interval_s;
        tokio::spawn(async move {
            provider_cooldown::billing_cooldown_recovery_loop(
                orch_billing,
                db_billing,
                recov_interval,
            )
            .await;
        });
    }

    // Worker `provider_health_probe`: pinga periodicamente i provider LLM
    // per rilevare cooldown / quota esaurita PRIMA del primo errore utente.
    // Valore canonico: settings.provider_health_probe_enabled/interval_s nel DB.
    // Override emergenza: NEXUS_PROVIDER_HEALTH_PROBE_ENABLED, NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S.
    let probe_enabled = s_health_probe_enabled
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let probe_interval = s_health_probe_interval
        .ok()
        .flatten()
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
        state.agent_channels.clone(),
    );

    // Worker `services_watchdog`: in dev/WSL (senza systemd Restart=on-failure)
    // monitora i microservizi Nexus (brain, gateway, *-service, web-ide) via TCP
    // probe e li riavvia se cadono. Config DB-driven (agent.watchdog.*, mig 0272),
    // gating runtime via agent.watchdog.enabled. mcp-core escluso (ospita il loop).
    services_watchdog::spawn_services_watchdog(state.db.clone());

    // Worker `catalog_sync`: aggiorna periodicamente ai_price_catalog dal
    // JSON LiteLLM. Cadenza configurabile via settings.model_catalog_sync_interval_s
    // (default 12h, minimo 1h). Disabilitabile via settings.model_catalog_sync_enabled.
    let catalog_sync_enabled = s_catalog_sync_enabled
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let catalog_sync_interval = s_catalog_sync_interval
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(43200);
    catalog_sync_worker::spawn_catalog_sync_worker(
        state.db.clone(),
        catalog_sync_enabled,
        catalog_sync_interval,
    );

    // Worker `model_health_probe`: pinga ogni modello enabled in ai_price_catalog
    // per rilevare modelli broken model-specific (model_not_found, hollow_completion,
    // unsupported, ecc.). Auto-disable dopo N fallimenti consecutivi
    // (settings.model_health_probe_failure_threshold, default 3).
    let model_probe_enabled = s_model_health_enabled
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let model_probe_interval = s_model_health_interval
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1800);
    let model_probe_threshold = s_model_health_threshold
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(3);
    model_health_probe::spawn_model_health_probe(
        std::sync::Arc::new(state.orchestrator.clone()),
        state.db.clone(),
        model_probe_enabled,
        model_probe_interval,
        model_probe_threshold,
    );

    // Worker `deepseek_balance_sync`: DeepSeek e' l'unico provider con
    // endpoint pubblico /user/balance. Sincronizza provider_budget_status
    // con il dato reale ogni 15 min (default).
    deepseek_balance_sync::spawn_deepseek_balance_sync(state.db.clone(), true, 900);

    // Worker `routing_matrix_auto_promoter`: ricostruisce le righe della
    // routing matrix dal catalog modelli ogni 6h (default), promuovendo i
    // nuovi modelli appena rilasciati e sostituendo quelli auto-disabled
    // dal model_health_probe. Le righe con manual_override=true (admin) NON
    // vengono toccate. Vedi `routing_matrix_auto_promoter.rs`.
    let auto_promote_enabled: Option<String> =
        settings::get_setting(&state.db, "routing_matrix_auto_promote_enabled")
            .await
            .ok()
            .flatten();
    let auto_promote_interval: Option<String> =
        settings::get_setting(&state.db, "routing_matrix_auto_promote_interval_s")
            .await
            .ok()
            .flatten();
    let ap_enabled = auto_promote_enabled
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let ap_interval = auto_promote_interval
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(21600);
    routing_matrix_auto_promoter::spawn_routing_matrix_auto_promoter(
        state.db.clone(),
        ap_enabled,
        ap_interval,
    );

    // ADR 0017 v2 F8: i worker legacy `knowledge_workers::*` (link_inference,
    // cleanup, promote) e `meta_docs_*` (watcher bidirezionale + refresh) sono
    // stati rimossi assieme ai moduli `knowledge/` e `meta_docs/`. Le tabelle
    // backing sono droppate dalla mig 0295. La funzione "auto-link" e' ora
    // gestita da `wiki::links_worker` (vedi mod.rs F4); il "vault watcher"
    // bidirezionale per `docs/.nexus-vault/` e' un TODO ADR 0017 — finche'
    // non viene reimplementato come `wiki::watcher`, gli edit Obsidian
    // restano in sincrono solo via `POST /api/wiki/reingest`.
    agent_tool_result_cache::start_cleanup_worker(state.db.clone());

    // NexusAutoFixAgent: intercetta E2E fallimenti e genera change_drafts.
    nexus_autofix_worker::start_nexus_autofix_worker(state.clone());

    // ToolRunner gRPC server (Fase 1 refactor orchestrazione LangGraph).
    // Valore canonico: settings.tool_runner_enabled nel DB (admin panel).
    // Override emergenza: ENABLE_TOOL_RUNNER=1 (priorita' piu' alta del DB).
    {
        let env_override = std::env::var("ENABLE_TOOL_RUNNER").ok().as_deref() == Some("1");
        let db_enabled = s_tool_runner
            .ok()
            .flatten()
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if env_override || db_enabled {
            let addr: SocketAddr = tool_runner_addr_str
                .parse()
                .expect("tool_runner_addr (DB o env TOOL_RUNNER_ADDR) non valido");
            let deps = tool_runner_server::ToolRunnerDeps {
                db: state.db.clone(),
                neural: state.orchestrator.neural.clone(),
                agent_channels: state.agent_channels.clone(),
                playwright_channels: state.playwright_channels.clone(),
                terminal_consumers: state.terminal_consumers.clone(),
                template_cache: state.template_cache.clone(),
                dependency_status: state.dependency_status.clone(),
                project_channels: state.project_channels.clone(),
                monitor_registry: state.monitor_registry.clone(),
                port_registry: state.port_registry.clone(),
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
            // Indirizzo: env var (override emergenza) > DB (canonico) > hardcoded.
            let db_agent_addr = settings::get_setting(&state.db, "agent_router_addr")
                .await
                .ok()
                .flatten()
                .map(|v| v.trim().to_string());
            let agent_router_addr_str = std::env::var("AGENT_ROUTER_ADDR")
                .ok()
                .or(db_agent_addr)
                .unwrap_or_else(|| "127.0.0.1:50501".to_string());
            let addr: SocketAddr = agent_router_addr_str
                .parse()
                .expect("agent_router_addr (DB o env AGENT_ROUTER_ADDR) non valido");
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
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers([
            http_header::CONTENT_TYPE,
            http_header::AUTHORIZATION,
            http_header::ACCEPT,
            http_header::COOKIE,
        ])
        .allow_credentials(true);

    let app = routes::build_app_router(state, cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], mcp_http_port));
    tracing::info!("mcp-core listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Graceful shutdown: SIGTERM o Ctrl-C → flush NexusBridge (Q-table + replication)
    // prima che il processo termini, per evitare perdita dati in-flight.
    let shutdown_signal = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut terminate =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
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

        // Safety net force-exit (Bug D fix #80): se axum::serve non si chiude
        // entro 10s dopo il signal (es. SSE/long-poll in-flight), forziamo
        // l'exit del processo. I dati critici (Q-table) sono gia' stati
        // flushati da `bridge.shutdown()`. La unit systemd ha TimeoutStopSec=15
        // come ulteriore safety net (SIGKILL forzato dopo 15s).
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            tracing::warn!(
                "shutdown timeout 10s superato (probabilmente SSE/long-poll in-flight), force-exit"
            );
            std::process::exit(0);
        });
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
    // Indirizzo: env var (override emergenza) > DB (canonico) > hardcoded.
    let tools_grpc_ok = {
        let db_addr = settings::get_setting(&state.db, "tool_runner_addr")
            .await
            .ok()
            .flatten()
            .map(|v| v.trim().to_string());
        let addr = std::env::var("TOOL_RUNNER_ADDR")
            .ok()
            .or(db_addr)
            .unwrap_or_else(|| "127.0.0.1:50071".into());
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    };

    // Verifica TCP connect rapido al Brain REST (porta 8001): gli agent run
    // usano POST /agent/run/stream su questa porta. neural_core (gRPC 50051)
    // puo' essere online mentre il server REST e' giu'.
    // Indirizzo: env var (override) > DB (canonico) > hardcoded.
    let brain_rest_ok = {
        let db_url = settings::get_setting(&state.db, "brain_rest_url")
            .await
            .ok()
            .flatten()
            .and_then(|v| {
                // Estrae host:port da URL come "http://127.0.0.1:8001"
                v.trim()
                    .trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .split('/')
                    .next()
                    .map(|s| s.to_string())
            });
        let addr = std::env::var("BRAIN_REST_ADDR")
            .ok()
            .or(db_url)
            .unwrap_or_else(|| "127.0.0.1:8001".into());
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    };

    let status = if db_ok && redis_ok && tools_grpc_ok && brain_rest_ok {
        "ok"
    } else {
        "degraded"
    };

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
            qdrant: state
                .dependency_status
                .qdrant
                .load(std::sync::atomic::Ordering::Relaxed),
            embedder: state
                .dependency_status
                .embedder
                .load(std::sync::atomic::Ordering::Relaxed),
            brain_rest: brain_rest_ok,
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
