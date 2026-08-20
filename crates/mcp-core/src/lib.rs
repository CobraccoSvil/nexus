// Su Windows molti sottosistemi sono Linux-only (systemd/wizard, sudo-runner,
// docker-compose, /proc, run-mode) e il loro codice e' cfg-gated out o non
// raggiunto: e' dead-code LECITO del porting. allow(dead_code) SOLO su Windows;
// su Linux quel codice e' usato, e la contromisura al dead-code GENUINO doveva
// essere il CI Linux, dove questo allow non si applica. Punto unico (regola L)
// invece di allow sparsi per-modulo.
//
// LIMITE MISURATO (08/08/2026): quella contromisura non ha potuto scattare. Il
// gate `verify.yml` muore su `@ai-orchestrator/web-ide#test`, cioe' nella fase
// turbo, PRIMA che `cargo clippy -- -D warnings` venga eseguito — rosso su ogni
// run da almeno il 03/08. Nel frattempo 711 righe di dead-code genuino sono
// sopravvissute in `agent_turn_setup.rs` (gli handler SSE del brain Python, morti
// da mig 0462/0532), invisibili qui per questo allow e mai segnalate la'. Finche'
// il CI non torna a raggiungere clippy, su questo crate il dead-code genuino non
// ha nessuno che lo guardi: si censisce a mano neutralizzando questa riga e
// leggendo i `never used` di `cargo check -p mcp-core --all-targets`.
#![cfg_attr(windows, allow(dead_code))]

mod admin;
mod agent_graph_adapter;
mod agent_processes;
mod agent_router_server;
mod agent_todos_routes;
mod agent_tool_result_cache;
mod agent_tools;
mod agent_turn_setup;
mod agent_types;
mod auth;
mod billing;
pub use nexus_build_graph as build_graph;
mod cache;
mod capability;
mod catalog_sync_worker;
/// Il metro del registro delle porte (ADR 0042, P0(b)): quante identita' di
/// servizio contiene `nexus_port_allocations` e quante ne descrivono una viva.
/// Sola lettura, eseguito on-demand; vive qui perche' la domanda «chi ascolta
/// adesso» ha un punto unico `pub(crate)` in questo crate (vedi il modulo).
#[cfg(test)]
mod censimento_porte;
mod change_drafts;
mod chat_agent;
mod chat_attachments;
mod chat_learning;
mod chat_messages;
mod chat_sessions;
mod claude_agents;
mod context_settings;
mod db;
mod db_settings;
mod dispatcher_routes;
mod dlp;
mod documents;
mod docx_render;
mod domain;
mod environment;
mod fanin_worker;
mod file_mutations;
/// Punto unico di «questa coppia (fornitore, modello) sa fare IL GIUDICE su
/// questo schema?»: memoria di processo delle astensioni STRUTTURALI del gate.
mod giudici_inadatti;
mod github;
mod http_metrics;
mod intent_classifier;
mod internal_learning;
mod internal_routing;
pub(crate) use nexus_types::llm_json;
mod governance_telemetry;
mod latency_telemetry;
mod tpm_telemetry;
mod long_running;
mod mcp_client;
mod mcp_connectors;
mod middleware;
mod model_catalog_sync;
mod model_health_probe;
mod model_observation;
mod model_qualification;
mod model_switch;
mod models;
mod mutations_api;
mod native_engine;
mod neural_compat;
mod nexus_bridge;
mod nexus_builtin;
mod nexus_database_stats;
mod nexus_gateway;
mod nexus_routing;
mod nexus_tool_catalog;
mod nexus_tools;
mod orchestrator;
mod playbook_engine;
mod playwright_env;
pub mod playwright_live;
mod plugins;
mod port_registry;
mod process_liveness;
mod process_resume;
mod process_util;
mod profile_selection;
mod profiles;
mod runtime_health;
// Estratto in crate workspace (split 7.4): re-export per mantenere
// validi i path crate::project_db:: dei moduli esistenti.
pub use nexus_project_db as project_db;
mod db_retention;
mod learned_instructions;
mod probe_agentic_loop;
mod probe_chain_measure;
mod probe_latent_state;
mod probe_world;
mod project_db_routes;
mod project_files;
mod project_git;
mod project_workspace;
mod projects;
mod prompt_memories;
mod prompt_templates;
mod provider_balance_sync;
mod provider_cooldown;
mod provider_error_classifier;
mod provider_inflight;
mod provider_health_probe;
mod provider_declaration;
mod provider_readiness;
mod provider_selectability;
mod provider_spend_cap;
mod rag;
mod reconcile_default_models;
mod routes;
mod routing_config;
mod routing_matrix;
mod routing_matrix_auto_promoter;
mod routing_slots;
mod run_lineage;
mod run_reaper;
mod run_totals;
pub use nexus_tool_kit::sandbox;
mod security;
mod services_watchdog;
mod session_autocommit;
/// Punto unico del perimetro contabile del contatore di chat: quali run
/// compongono l'insieme di cui si dichiara token e costo.
mod session_usage;
mod session_worklog;
mod settings;
mod static_preview;
mod sudo_manager;
mod sudo_routes;
/// Punto unico della verifica a suite: memoria degli esiti per stato del
/// codice + classificazione del rosso non riprodotto.
mod suite_verification;
mod system_services;
mod task_watchdog;
#[cfg(test)]
mod test_support;
mod tool_capability;
mod tool_runner_server;
mod trace_store;
mod ui_clarification;
mod ui_flags;
mod vector_memory;
mod verify_probe;
mod verify_profile;
mod wiki;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{header as http_header, HeaderValue, Method};
use axum::{extract::State, Json};
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
    /// Istante di avvio del processo mcp-core. Usato dal boot-grace dell'observer
    /// (`service_observer_remediation::within_boot_grace`): dopo un restart da
    /// deploy i servizi del progetto sono nel transitorio di riavvio e non vanno
    /// scambiati per crash. Timbrato una volta in `build_app_state`.
    pub(crate) boot_at: std::time::Instant,
}

// Fallback quando `tail` non parte (Unix): non potendo seguire l'output, resta in
// polling della liveness del processo; alla morte marca status='stopped' e prova a
// leggere l'exit code da /proc/{pid}/status. Estratto da `spawn_reattach_monitor`
// per tenere ciascuna funzione sotto la soglia di lunghezza (comportamento invariato).
#[cfg(unix)]
async fn reattach_fallback_poll(id: uuid::Uuid, pid: i32, avvio: Option<i64>, db: &sqlx::PgPool) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        // Punto unico: il polling dura ore, e un pid riciclato nel frattempo
        // terrebbe questo loop in attesa di un processo che non e' il nostro.
        if crate::process_liveness::stato_del_pid(pid as u32, avvio).autorizza_a_dichiararlo_morto()
        {
            let exit_code: Option<i32> = tokio::process::Command::new("sh")
                .args([
                    "-c",
                    &format!("cat /proc/{}/status 2>/dev/null | grep -c VmPeak", pid),
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
            .execute(db)
            .await;
            break;
        }
    }
}

// Legge l'output del `tail` riga per riga e lo appende al DB in batch (ogni 2s o a
// EOF). Estratto da `spawn_reattach_monitor` per contenere la lunghezza della
// funzione principale (comportamento invariato).
#[cfg(unix)]
async fn reattach_stream_output(
    mut stdout: tokio::process::ChildStdout,
    id: uuid::Uuid,
    db_clone: &sqlx::PgPool,
) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(&mut stdout).lines();
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
                    .execute(db_clone)
                    .await;
                }
            }
        }
    }
    if !buf.is_empty() {
        let _ = sqlx::query(
            "UPDATE agent_processes SET output = LEFT(output || $1, 50000) WHERE id=$2",
        )
        .bind(&buf)
        .bind(id)
        .execute(db_clone)
        .await;
    }
}

// Re-attach del monitoring su un processo sopravvissuto a un riavvio (Unix).
// Segue stdout+stderr del processo via /proc/{pid}/fd/1,2 con `tail -f --pid`,
// accumula l'output nel DB e, quando `tail` esce (processo terminato), marca
// status='stopped'. Se `tail` non parte, fa fallback a polling della liveness.
// Interamente Linux-centrico (dipende da /proc, `tail`, `sh`, `kill`).
// Estratta da `main` (era un `fn` nested, nessuna cattura d'ambiente): sposta
// la lunghezza/complessita fuori dal corpo di `main`, comportamento invariato.
// `avvio` e' l'istante d'avvio ATTESO (epoch unix, da `agent_processes.started_at`):
// senza, questo monitor resterebbe in polling su un pid che il SO puo' aver
// riassegnato a un estraneo. Firma su una riga, e non spezzata per parametro: le
// due varianti di piattaforma hanno lo stesso contratto, quindi una firma
// multi-riga e' un blocco IDENTICO in due punti — il rilevatore di duplicazione
// lo conta, e ha ragione a contarlo.
#[cfg(unix)]
fn spawn_reattach_monitor(id: uuid::Uuid, pid: i32, avvio: Option<i64>, db: sqlx::PgPool) {
    tokio::spawn(async move {
        let stdout_path = format!("/proc/{}/fd/1", pid);
        let stderr_path = format!("/proc/{}/fd/2", pid);
        let mut child = match tokio::process::Command::new("tail")
            .args(["-f", "--pid", &pid.to_string(), &stdout_path, &stderr_path])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to re-attach to process {}: {}", pid, e);
                reattach_fallback_poll(id, pid, avvio, &db).await;
                return;
            }
        };

        // Leggi output dal tail e appendilo al DB
        if let Some(stdout) = child.stdout.take() {
            reattach_stream_output(stdout, id, &db).await;
        }

        // tail è uscito: il processo originale è terminato
        let _ = sqlx::query(
            "UPDATE agent_processes SET status='stopped', stopped_at=NOW() WHERE id=$1 AND status='running'"
        )
        .bind(id)
        .execute(&db)
        .await;
    });
}

// Re-attach del monitoring su un processo sopravvissuto a un riavvio (Windows).
// /proc non esiste e non e' possibile ri-agganciare stdout/stderr di un processo
// gia' avviato da un'istanza precedente: ci si limita a poll della liveness via
// process_util::process_alive (OpenProcess) ogni 5s; quando il processo non e'
// piu' vivo si marca status='stopped' con exit_code=NULL (il codice di uscita
// reale non e' recuperabile senza aver aperto il processo come figlio).
// Estratta da `main` (era un `fn` nested, nessuna cattura d'ambiente).
#[cfg(windows)]
fn spawn_reattach_monitor(id: uuid::Uuid, pid: i32, avvio: Option<i64>, db: sqlx::PgPool) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            // Punto unico, con l'identita': questo loop vive quanto il processo,
            // e Windows ricicla i pid. Senza il confronto, il primo estraneo che
            // eredita il pid lo terrebbe «vivo» a tempo indeterminato; e senza il
            // ramo dell'ignoto, un pid momentaneamente non interrogabile lo
            // farebbe marcare stopped mentre e' vivo.
            if crate::process_liveness::stato_del_pid(pid as u32, avvio)
                .autorizza_a_dichiararlo_morto()
            {
                // exit_code non determinabile su Windows senza handle del figlio:
                // si lascia NULL (coerente col path Unix, che scrive -1 solo se
                // non riesce a leggere VmPeak).
                let _ = sqlx::query(
                    "UPDATE agent_processes SET status='stopped', stopped_at=NOW() WHERE id=$1",
                )
                .bind(id)
                .execute(&db)
                .await;
                break;
            }
        }
    });
}

// Diagnostica: al SIGTERM raccoglie indizi sul possibile MITTENTE. tokio non
// espone il si_pid del segnale, quindi facciamo best-effort scansionando /proc
// per i processi che tipicamente inviano SIGTERM a mcp-core (deploy, pkill/kill,
// un secondo mcp-core in single-instance). Serve a capire in 10s chi ha ucciso
// il backend, invece di doverlo ricostruire a ritroso (incidente 2026-06-07).
// Solo Unix: legge /proc ed e' invocata unicamente nel ramo SIGTERM cfg(unix)
// (su Windows sarebbe codice morto -> warning con clippy -D warnings).
// Estratta da `main` (era un `fn` nested, nessuna cattura d'ambiente).
#[cfg(unix)]
fn diagnose_signal_origin() -> String {
    let own = std::process::id();
    let ppid = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            // campo 4 (1-based) di /proc/self/stat = ppid; comm puo' avere spazi,
            // quindi si parte dopo l'ultima ')'.
            s.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1).map(String::from))
        })
        .unwrap_or_else(|| "?".into());
    let mut suspects: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid: u32 = match e.file_name().to_str().and_then(|s| s.parse().ok()) {
                Some(p) if p != own => p,
                _ => continue,
            };
            let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            if raw.is_empty() {
                continue;
            }
            let cmd: String = raw
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s))
                .collect::<Vec<_>>()
                .join(" ");
            let l = cmd.to_lowercase();
            if l.contains("deploy-local")
                || l.contains("pkill")
                || l.contains("supervisor")
                || l.contains("target/release/mcp-core")
                || l.contains("target/debug/mcp-core")
            {
                suspects.push(format!(
                    "pid={pid} cmd=\"{}\"",
                    cmd.chars().take(120).collect::<String>()
                ));
            }
        }
    }
    if suspects.is_empty() {
        format!(" [origine: ppid={ppid}; nessun mittente sospetto in /proc — probabile kill manuale o systemd]")
    } else {
        format!(
            " [origine: ppid={ppid}; candidati mittenti: {}]",
            suspects.join(" | ")
        )
    }
}

/// Interpreta un valore di setting testuale come flag booleano attivo/spento.
/// Punto unico (regola L) del pattern ripetuto in `main`: un valore assente
/// ricade sul `default`; un valore presente e' considerato SPENTO solo se e'
/// esattamente uno di `0|false|no|off` (case-insensitive), altrimenti ACCESO.
/// Comportamento identico ai blocchi inline che sostituisce.
fn setting_flag_enabled(opt: Option<String>, default: bool) -> bool {
    opt.map(|v| {
        !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
    .unwrap_or(default)
}

/// Ripristina i cooldown billing dei provider sopravvissuti al riavvio, letti
/// dalle chiavi Redis `nexus:billing_cooldown:*` (formato `<until_ts>|<reason>`).
/// Estratta da `main` (comportamento invariato) per contenerne complessita e
/// lunghezza; scarta silenziosamente chiavi malformate o gia' scadute.
async fn restore_billing_cooldowns_from_redis(mut conn: redis::aio::MultiplexedConnection) {
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let Ok(keys) = redis::cmd("KEYS")
        .arg("nexus:billing_cooldown:*")
        .query_async::<Vec<String>>(&mut conn)
        .await
    else {
        return;
    };
    for key in keys {
        let Ok(value) = redis::cmd("GET")
            .arg(&key)
            .query_async::<String>(&mut conn)
            .await
        else {
            continue;
        };
        // Formato: "<until_unix_ts>|<reason>"
        let mut parts = value.splitn(2, '|');
        let (Some(ts_str), Some(reason)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(until_ts) = ts_str.parse::<u64>() else {
            continue;
        };
        if until_ts > now_ts {
            let remaining = until_ts - now_ts;
            let provider = key.strip_prefix("nexus:billing_cooldown:").unwrap_or(&key);
            crate::provider_cooldown::restore_cooldown(provider, remaining, reason);
        }
    }
}

/// Boot-recovery per-progetto (separazione DB): itera i progetti registrati e
/// marca 'failed' i processi `agent_processes` in stato 'running'/'starting' il
/// cui PID non e' piu' vivo. Mark-only: NON re-attacha il monitor (side-effect
/// non idempotente). Estratta dalla closure `tokio::spawn` in `main` per
/// contenerne complessita e lunghezza; comportamento invariato.
async fn mark_stale_project_processes_failed(db_recover: sqlx::PgPool) {
    for project_id in project_db_routes::list_all_project_ids(&db_recover).await {
        match project_db_routes::project_data_pool_from(&db_recover, project_id).await {
            Ok(pool) => riconcilia_processi_di_progetto(&db_recover, &pool, project_id).await,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "boot-recovery processi stale: DB progetto non disponibile, progetto saltato per questo giro");
            }
        }
    }
}

/// Le due prove del SERVIZIO, raccolte una volta per progetto e solo se c'e'
/// almeno una riga di servizio da giudicare: costano una query e una syscall.
async fn prove_di_vita_se_servono(
    meta: &sqlx::PgPool,
    project_id: uuid::Uuid,
    righe: &[sqlx::postgres::PgRow],
) -> Option<project_workspace::service_liveness::ProveDiVita> {
    use sqlx::Row;
    let c_e_un_servizio = righe.iter().any(|r| {
        r.try_get::<String, _>("kind")
            .map(|k| k == "service")
            .unwrap_or(false)
    });
    if !c_e_un_servizio {
        return None;
    }
    Some(project_workspace::service_liveness::ProveDiVita::del_progetto(meta, project_id).await)
}

/// Le righe 'running'/'starting' di UN progetto: verdetto dal punto unico, e
/// scrittura solo su una morte ACCERTATA.
///
/// Per un processo one-shot la domanda e' «e' ancora il MIO processo?» (un pid
/// riciclato non lo e'); per un SERVIZIO e' un'altra ancora, perche' il pid
/// registrato e' la shell — e al boot il caso tipico e' proprio quello: mcp-core
/// e' ripartito, il monitor non c'e' piu', ma il server che aveva avviato e' un
/// discendente ancora in ascolto sulla sua porta. Un pid che il SO non ci lascia
/// interrogare resta com'e': persistere una non-osservazione e' il modo in cui
/// l'errore diventa definitivo.
async fn riconcilia_processi_di_progetto(
    meta: &sqlx::PgPool,
    pool: &sqlx::PgPool,
    project_id: uuid::Uuid,
) {
    use sqlx::Row;
    let stale = sqlx::query(
        "SELECT id, kind, label, pid, started_at FROM agent_processes \
         WHERE status IN ('running', 'starting')",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let prove = prove_di_vita_se_servono(meta, project_id, &stale).await;
    for row in stale {
        let Ok(id) = row.try_get::<uuid::Uuid, _>("id") else {
            continue;
        };
        let verdetto = project_workspace::service_liveness::verdetto_di_riga(
            &row.try_get::<String, _>("kind").unwrap_or_default(),
            &row.try_get::<String, _>("label").unwrap_or_default(),
            row.try_get("pid").unwrap_or(None),
            row.try_get("started_at").unwrap_or(None),
            prove.as_ref(),
        );
        if verdetto.autorizza_a_dichiararlo_morto() {
            let _ = sqlx::query(
                "UPDATE agent_processes SET status='failed', stopped_at=NOW() WHERE id=$1 AND status IN ('running','starting')",
            )
            .bind(id)
            .execute(pool)
            .await;
        } else if !verdetto.e_vivo() {
            tracing::warn!(
                project_id = %project_id,
                process_id = %id,
                motivo = %verdetto.descrizione(),
                "boot-recovery: stato del processo non accertabile, riga lasciata invariata"
            );
        }
    }
}

/// ADR 0017 v2 F3 — re-ingest automatico dai vault se `wiki_docs` e' vuota.
/// Bootstrap one-shot: alla prima esecuzione dopo la mig 0295 la tabella e'
/// vuota e i vault Markdown sono la sola fonte di verita'. Estratta dalla
/// closure `tokio::spawn` in `main` (comportamento invariato).
async fn wiki_bootstrap_reingest_if_empty(state_bootstrap: AppState) {
    let empty: bool = match sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM wiki_docs")
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
    match wiki::reingest::reingest_all(&state_bootstrap.wiki_deps(), None, None).await {
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
}

/// ADR 0017 v2 F4 — auto-recompute link al primo avvio post-F3. Se `wiki_docs > 0`
/// ma `wiki_links == 0` la mig 0295 e' applicata, F3 ha popolato i doc ma F4 non e'
/// mai stato eseguito. Attende 60s per dare tempo al re-ingest F3 di completare.
/// Estratta dalla closure `tokio::spawn` in `main` (comportamento invariato).
async fn wiki_bootstrap_recompute_links(state_links: AppState) {
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
        &state_links.wiki_deps(),
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
}

/// Cache di routing e registro porte inizializzate all'avvio, raggruppate per
/// non moltiplicare i valori di ritorno di `init_routing_and_port_caches`
/// (regola: helper <=6 parametri, niente tuple lunghe illeggibili).
/// - `routing_matrix`/`thresholds`/`intent`/`slots` vengono passate/mosse
///   nell'Orchestrator (vedi `build_orchestrator`).
/// - `port_registry` sopravvive fino alla costruzione di `AppState`.
struct RoutingAndPortCaches {
    routing_matrix: routing_matrix::RoutingMatrixCache,
    thresholds: routing_config::RoutingThresholdsCache,
    intent: routing_config::IntentCapabilityCache,
    slots: routing_slots::SlotsRoutingMatrixCache,
    port_registry: port_registry::PortRegistryCache,
}

/// Inizializza le cache di routing (routing matrix, parametri routing, intent
/// capability, matrice slot-based) e il registro porte, piu' il singleton globale
/// del build graph. Estratta da `main` (comportamento e ordine di inizializzazione
/// invariati). Le cache che panicano se il DB e' irraggiungibile (routing matrix,
/// thresholds, intent) lo fanno qui, coerentemente con il comportamento pre-refactor.
async fn init_routing_and_port_caches(db: &sqlx::PgPool) -> RoutingAndPortCaches {
    // Listino configurato? Verifica ALL'AVVIO (regola G): la currency di
    // piattaforma non ha piu' un default hardcoded, quindi la sua assenza va
    // scoperta qui — dove fallire e' gratuito e rumoroso — e non a ogni chiamata
    // LLM, dove propagare l'errore significherebbe respingere le richieste.
    if let Err(e) = nexus_pricing::assert_configured(db).await {
        panic!(
            "nexus-pricing: configurazione del listino assente o illeggibile: {e}\n\
             Applicare la migrazione 0294 (settings.billing_base_currency) prima di avviare."
        );
    }

    // Inizializza la cache routing matrix (legge da DB + spawn refresh background 60s).
    // Va inizializzata PRIMA di Orchestrator::new perche' viene clonata dentro l'orchestrator.
    let routing_matrix = routing_matrix::RoutingMatrixCache::init(db.clone()).await;

    // Cache parametri routing (mig 0111) e intent capability (mig 0110).
    // Stesso pattern: retry 5x5s + panic se DB irraggiungibile.
    let thresholds = routing_config::RoutingThresholdsCache::init_thresholds(db.clone()).await;
    let intent = routing_config::IntentCapabilityCache::init_intent_capability(db.clone()).await;
    // Cache matrice slot-based (mig 0133, Livello 4 NLU). Diversamente dalle
    // altre cache, non panica se assente: il routing classico (intent,mode)
    // resta il fallback predefinito quando la matrice slots non e' popolata.
    let slots = routing_slots::SlotsRoutingMatrixCache::init(db).await;

    // Cache registro porte (mig 0114): porte TCP allocate ai progetti.
    // Non panica se tabella vuota (nessuna allocazione al primo avvio).
    let port_registry = port_registry::PortRegistryCache::init(db.clone()).await;

    // ADR 0020: cache build graph (mig 0312). Inizializzata come singleton
    // globale, non panica se DB down (lazy: la prima get_or_compute provera').
    build_graph::BuildGraphCache::init_global(db.clone()).await;
    tracing::info!("build_graph::BuildGraphCache inizializzato (TTL configurabile via settings)");
    // Recovery: sincronizza registro con file .service esistenti su disco.
    port_registry.startup_recovery().await;

    RoutingAndPortCaches {
        routing_matrix,
        thresholds,
        intent,
        slots,
        port_registry,
    }
}

/// GC periodico delle porte: rilascia le allocazioni porta dynamic orfane (nessun
/// listener, oltre la grace period) lasciate dai tentativi falliti degli agenti.
/// Intervallo/grace DB-driven con default sicuri. Estratta dalla closure
/// `tokio::spawn` in `main` (comportamento invariato).
async fn spawn_port_gc(db: &sqlx::PgPool) {
    let db_gc = db.clone();
    let gc_interval = settings::get_setting(db, "agent.port_gc.interval_seconds")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(120);
    // La grace ha UN solo lettore (regola L): la legge anche il raccoglitore che
    // gira all'avvio di un servizio, e due default diversi darebbero due idee di
    // quando una riga diventa giudicabile.
    let gc_grace = project_workspace::raccolta_allocazione::grace_secs(db).await;
    tokio::spawn(async move {
        port_registry::port_gc_loop(db_gc, gc_interval, gc_grace).await;
    });
}

/// Costruisce l'Orchestrator: risolve URL/token del Nexus Gateway dal DB (regola
/// G: niente env/hardcoded per la porta) e, se il gateway e' raggiungibile,
/// attiva PATH A (gateway), altrimenti PATH B (Brain gRPC). Consuma le cache di
/// routing (routing matrix/thresholds/intent mosse, slots clonata) e ritorna
/// anche il registro porte che sopravvive nella `AppState`. Estratta da `main`
/// (comportamento invariato); la struct di contesto tiene i parametri <=6.
async fn build_orchestrator(
    db: &sqlx::PgPool,
    neural_client: NeuralCoreClient,
    template_cache: prompt_templates::TemplateCache,
    caches: RoutingAndPortCaches,
) -> (Orchestrator, port_registry::PortRegistryCache) {
    // URL gateway dalla porta nel DB (regola G: niente env/hardcoded).
    let gw_port = nexus_auth::resolve_port(db, "nexus_gateway_port").await;
    let gw_url = format!("http://127.0.0.1:{gw_port}");
    let nexus_gw = nexus_gateway::NexusGatewayClient::from_db(db).await;
    // Il gateway si INIETTA sempre: la sua disponibilita' non si decide qui.
    //
    // Prima una probe `is_healthy()` all'avvio sceglieva fra PATH A (gateway) e
    // PATH B (brain gRPC), e l'esito restava congelato per tutta la vita del
    // processo. Il 2026-07-16 mcp-core ha sondato il gateway 1,4s prima che
    // questo finisse di avviarsi: risultato, nessun gateway fino al riavvio
    // successivo, classificatore sempre in fallback e dimensionamento spento —
    // con il gateway che nel frattempo rispondeva 200. Il PATH B non esiste piu'
    // (il brain e' stato eliminato), quindi non c'e' nulla da scegliere: se il
    // gateway e' giu', lo dice la singola chiamata che fallisce, e al tentativo
    // dopo puo' essere di nuovo su (regola M: lo stato si osserva quando serve,
    // non si deduce una volta per sempre).
    let orchestrator = Orchestrator::new(
        neural_client,
        template_cache,
        nexus_gw,
        caches.routing_matrix,
        caches.thresholds,
        caches.intent,
        caches.slots.clone(),
    );
    tracing::info!("Nexus Gateway: client configurato su {gw_url}");
    (orchestrator, caches.port_registry)
}

/// Riconcilia i processi lasciati in stato 'running'/'starting' da un riavvio
/// precedente (PID non piu' vivo -> status=failed; PID vivo -> resta running e si
/// rilancia il monitoring OS-specifico). Estratta da `main` (comportamento
/// invariato). NOTA (separazione DB, sempre attiva da mig 0527): `agent_processes`
/// e' MIGRATA al DB per-progetto. Questo blocco opera sul META (solo righe
/// storiche pre-migrazione): il meta e' quasi vuoto -> no-op benigno. La
/// riconciliazione dei processi per-progetto e' gestita dal blocco mark-only in
/// `build_app_state`, che itera list_all_project_ids + project_data_pool_from.
/// NON duplicare qui.
///
/// Qui la domanda resta quella sul PROCESSO e non quella sul SERVIZIO
/// ([`project_workspace::service_liveness`]): la seconda prova e' un listener su
/// una porta allocata a `(project_id, label)`, e queste righe storiche stanno sul
/// meta senza il progetto a cui apparterrebbero. Le righe dei servizi vivi non
/// passano di qui — passano da `mark_stale_project_processes_failed`, che ha il
/// progetto e pone la domanda completa.
async fn reconcile_stale_processes(db: &sqlx::PgPool) {
    use sqlx::Row;
    let stale = sqlx::query(
        "SELECT id, pid, started_at FROM agent_processes WHERE status IN ('running', 'starting')",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for row in stale {
        let id: uuid::Uuid = row.get("id");
        let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
        let started_at: Option<chrono::DateTime<chrono::Utc>> =
            row.try_get("started_at").unwrap_or(None);
        // Punto unico (regola L): pid persistito -> `process_liveness`, che
        // guarda anche l'identita'. Il vecchio `process_alive` da solo non
        // vedeva i pid riciclati, e per contro avrebbe marcato 'failed' un
        // processo soltanto non interrogabile.
        let verdetto = crate::process_liveness::stato_da_riga(pid, started_at);

        if verdetto.autorizza_a_dichiararlo_morto() {
            let _ = sqlx::query(
                "UPDATE agent_processes SET status='failed', stopped_at=NOW() WHERE id=$1",
            )
            .bind(id)
            .execute(db)
            .await;
            tracing::info!(
                "Stale process {} (pid={:?}) marked failed: {}",
                id,
                pid,
                verdetto.descrizione()
            );
        } else if !verdetto.e_vivo() {
            tracing::warn!(
                process_id = %id,
                motivo = %verdetto.descrizione(),
                "reconcile_stale_processes: stato non accertabile, riga lasciata invariata"
            );
        } else {
            tracing::info!(
                "Process {} (pid={:?}) still running, re-attaching monitor",
                id,
                pid
            );
            // `e_vivo()` implica pid.is_some(); difensivamente saltiamo la
            // re-attach se la condizione viene violata in futuro.
            let Some(pid_val) = pid else {
                tracing::warn!("Process {} alive ma pid=None: skip re-attach", id);
                continue;
            };
            // Il monitoring di re-attach e' OS-specifico: su Unix segue stdout+stderr
            // via /proc/{pid}/fd/1,2 con `tail`; su Windows /proc non esiste, quindi
            // ci si limita a poll della liveness. Punto unico: spawn_reattach_monitor.
            // L'avvio atteso viaggia col pid: il monitor resta in polling per ore,
            // e senza di esso un riciclo del pid dopo la morte del processo lo
            // terrebbe «vivo» per sempre.
            spawn_reattach_monitor(id, pid_val, started_at.map(|t| t.timestamp()), db.clone());
        }
    }
}

/// Avvia tutti i worker fire-and-forget e i loop periodici DOPO la costruzione di
/// `AppState`, nell'ordine ESATTO richiesto: compattazione vettoriale, seed/reindex
/// tool, bootstrap/reingest wiki, triple extractor, watcher, chat-note, run-summary,
/// link/title/code-docs worker, learned instructions, audit writer, catalog sync,
/// reconciliation policy->catalog, port enforcer, resource linter, config
/// health/cooldown provider + recovery loop, health probe, watchdog, observer,
/// process resume, fanin, monitor seed, catalog/model health probe, deepseek balance,
/// routing matrix auto-promoter, cache cleanup, retention, ToolRunner e AgentRouter
/// gRPC server. Estratta da `main` per contenerne lunghezza/complessita
/// (comportamento e ordine invariati). Riceve `&AppState`: ogni worker clona cio'
/// che gli serve (`state.clone()` / `state.db.clone()` / `state.orchestrator.clone()`).
async fn spawn_background_workers(state: &AppState) {
    // L'ordine di avvio e' load-bearing e resta identico al pre-refactor; ogni
    // gruppo tematico e' un sub-helper coeso (regola L). Le letture settings sono
    // idempotenti e i valori indipendenti: distribuirle nei rispettivi helper non
    // cambia il comportamento osservabile (stessi worker, stessi parametri).
    seed_tools_and_learning(state).await;
    spawn_wiki_and_learning_workers(state);
    spawn_security_and_catalog_boot(state);
    configure_http_and_classifier(state).await;
    configure_provider_cooldown(state).await;
    spawn_provider_and_model_probes(state).await;
    spawn_infra_watchdogs(state);
    spawn_catalog_and_retention_workers(state).await;
    spawn_grpc_servers(state).await;
}

/// Compattazione vettoriale + seed dei tool builtin (await: deve completare prima
/// del reindex) + reindex semantico Qdrant. Estratto per isolare l'unico `.await`
/// bloccante mantenendo l'ordine originale (vector_compaction -> seed -> reindex).
async fn seed_tools_and_learning(state: &AppState) {
    chat_learning::spawn_vector_compaction_scheduler(state.clone());
    nexus_builtin::seed_tools_and_server(&state.db).await;
    // Reindex semantico Qdrant dei tool MCP (fire-and-forget, +30s delay).
    // Indicizza solo i tool con embedding mancante o hash cambiato.
    nexus_builtin::spawn_tool_reindex(state.db.clone());
}

/// Avvia i worker wiki/knowledge fire-and-forget (ADR 0017 v2): bootstrap re-ingest
/// F3 + recompute link F4, triple extractor F5, vault watcher, chat-note/run-summary,
/// link/title/code-docs worker e distiller delle learned instructions. Config
/// DB-driven per ciascuno. Estratto da `spawn_background_workers` (ordine invariato).
fn spawn_wiki_and_learning_workers(state: &AppState) {
    // F3: re-ingest one-shot se `wiki_docs` e' vuota (bootstrap dai vault Markdown).
    tokio::spawn(wiki_bootstrap_reingest_if_empty(state.clone()));
    // F4: auto-recompute link al primo avvio post-F3 (attende 60s il re-ingest).
    tokio::spawn(wiki_bootstrap_recompute_links(state.clone()));

    // F5: worker periodico LLM triple extraction (batch DB-driven, cap diurni).
    wiki::triple_extractor::start_triple_extractor_worker(state.wiki_deps());
    // TODO 1: watcher bidirezionale vault->DB (docs/.nexus-vault + progetti al boot).
    wiki::watcher::start_wiki_watcher(std::sync::Arc::new(state.wiki_deps()));
    // TODO 6+7: chat-note (delay 60s) + run-summary (delay 90s) worker.
    wiki::chat_note_worker::start_chat_note_worker(std::sync::Arc::new(state.wiki_deps()));
    wiki::run_summary_worker::start_run_summary_worker(std::sync::Arc::new(state.wiki_deps()));
    // Worker periodici link + titoli su scope meta E project (cap diurno per-scope).
    wiki::links_worker::start_links_worker(std::sync::Arc::new(state.wiki_deps()));
    wiki::title_gen::start_title_gen_worker(std::sync::Arc::new(state.wiki_deps()));
    // Arricchimento LLM dei wiki_docs kind=code (placeholder -> scheda + embedding).
    wiki::code_docs_enricher::start_code_docs_enricher_worker(std::sync::Arc::new(
        state.wiki_deps(),
    ));
    // Distiller learned instructions (livello 2 continuita', mig 0412).
    learned_instructions::spawn_learned_instructions_distiller(state.clone());
}

/// Avvia audit writer + catalog sync/reconcile al boot + port enforcer + resource
/// linter. Il catalog_sync_loop (mig 0182) mantiene ai_price_catalog allineato ai
/// modelli realmente esposti; la reconciliation policy->catalog al boot (regola H/L)
/// allinea subito is_enabled evitando che il primo routing scarti modelli capaci.
/// Estratto da `spawn_background_workers` (ordine e comportamento invariati).
fn spawn_security_and_catalog_boot(state: &AppState) {
    // Audit writer: batch INSERT in nexus_resource_audit (ogni 100 eventi o 5s).
    security::audit::start_writer(state.db.clone());

    let db_sync = state.db.clone();
    let orch_sync = std::sync::Arc::new(state.orchestrator.clone());
    tokio::spawn(async move {
        model_catalog_sync::catalog_sync_loop(db_sync, Some(orch_sync)).await;
    });

    let db_recon = state.db.clone();
    tokio::spawn(async move {
        match model_catalog_sync::reconcile_catalog_with_policy(&db_recon).await {
            Ok(stats) => tracing::info!(
                "boot policy reconciliation: enabled={} disabled={}",
                stats.enabled,
                stats.disabled,
            ),
            Err(e) => tracing::warn!("boot reconcile_catalog_with_policy fallito: {e}"),
        }
    });

    // Sentinella di salute del runtime: misura il ritardo di risveglio dei
    // task. Senza, un congelamento del runtime resta invisibile e i suoi
    // sintomi vengono attribuiti al provider/DB/gateway di turno (incidente
    // consiglio 2026-07-15: ~287s di task fermo, tre attribuzioni sbagliate).
    runtime_health::spawn_runtime_health_sentinel(state.db.clone());

    // Port enforcer: killa processi di progetto fuori dal bucket porte assegnato.
    tokio::spawn(security::port_enforcer::port_enforcer_loop(state.clone()));
    // Resource linter: diagnosi + auto-fix di porte/URL hardcoded (mig 0397/0398).
    security::resource_linter::spawn_resource_linter(state.clone());
}

/// Inizializza il flag AtomicBool del classificatore LLM e la config HTTP globale
/// (timeout, pool) dal DB. Env var come override d'emergenza. Estratto da
/// `spawn_background_workers` (comportamento invariato).
async fn configure_http_and_classifier(state: &AppState) {
    let (s_llm_classifier, s_http_timeout, s_http_pool) = tokio::join!(
        settings::get_setting(&state.db, "llm_classifier_enabled"),
        settings::get_setting(&state.db, "http_timeout_secs"),
        settings::get_setting(&state.db, "http_pool_max"),
    );

    let llm_classifier_db = setting_flag_enabled(s_llm_classifier.ok().flatten(), true);
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

    let http_timeout = s_http_timeout
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok());
    let http_pool = s_http_pool
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<usize>().ok());
    nexus_http::init_global_config(http_timeout, http_pool);
}

/// Config health/cooldown provider DB-driven (regola G, mig 0252): legge i timings
/// (vedi `fetch_provider_health_timings`), li applica come stato globale e avvia il
/// billing_cooldown_recovery_loop (probe-then-reenable). Estratto da
/// `spawn_background_workers`.
async fn configure_provider_cooldown(state: &AppState) {
    let pht = fetch_provider_health_timings(&state.db).await;
    provider_cooldown::init_provider_health_timings(pht);
    tracing::info!(
        "provider health timings (DB): recovery_interval={}s probe_timeout={}s cooldown_long={}s outage_threshold={}",
        pht.billing_recovery_interval_s, pht.recovery_probe_timeout_s,
        pht.cooldown_long_s, pht.outage_threshold,
    );

    let db_billing = state.db.clone();
    let orch_billing = std::sync::Arc::new(state.orchestrator.clone());
    let recov_interval = pht.billing_recovery_interval_s;
    tokio::spawn(async move {
        provider_cooldown::billing_cooldown_recovery_loop(orch_billing, db_billing, recov_interval)
            .await;
    });
}

/// Legge i tempi health/cooldown provider dai settings (mig 0252): default storici,
/// sovrascrive solo le chiavi presenti (setting mancante = invariato). Estratto da
/// `configure_provider_cooldown`.
async fn fetch_provider_health_timings(
    db: &sqlx::PgPool,
) -> provider_cooldown::ProviderHealthTimings {
    let mut pht = provider_cooldown::ProviderHealthTimings::default();
    // Ordine allineato all'assegnazione sotto (indici 0..12); u64 tranne
    // circuit_breaker_threshold (7) e outage_threshold (11), letti come usize.
    let (a, b, c, d, e, f, g, h, i, j, k, l) = tokio::join!(
        settings::get_setting(db, "provider.billing_recovery_interval_s"),
        settings::get_setting(db, "provider.recovery_probe_timeout_s"),
        settings::get_setting(db, "provider.cooldown_default_s"),
        settings::get_setting(db, "provider.cooldown_min_s"),
        settings::get_setting(db, "provider.cooldown_max_s"),
        // Chiave dal punto unico: la legge anche il gateway (regola L).
        settings::get_setting(
            db,
            nexus_types::provider_failure::durata::CHIAVE_COOLDOWN_LUNGO,
        ),
        settings::get_setting(db, "provider.circuit_breaker_window_s"),
        settings::get_setting(db, "provider.circuit_breaker_threshold"),
        settings::get_setting(db, "provider.circuit_breaker_extended_cooldown_s"),
        settings::get_setting(db, "provider.health_probe_timeout_s"),
        settings::get_setting(db, "provider.slow_cooldown_s"),
        settings::get_setting(db, "provider.outage_threshold"),
    );
    let u = |o: anyhow::Result<Option<String>>, d: u64| parse_u64_or(o.ok().flatten(), d);
    let z = |o: anyhow::Result<Option<String>>, d: usize| parse_usize_or(o.ok().flatten(), d);
    pht.billing_recovery_interval_s = u(a, pht.billing_recovery_interval_s);
    pht.recovery_probe_timeout_s = u(b, pht.recovery_probe_timeout_s);
    pht.cooldown_default_s = u(c, pht.cooldown_default_s);
    pht.cooldown_min_s = u(d, pht.cooldown_min_s);
    pht.cooldown_max_s = u(e, pht.cooldown_max_s);
    pht.cooldown_long_s = u(f, pht.cooldown_long_s);
    pht.circuit_breaker_window_s = u(g, pht.circuit_breaker_window_s);
    pht.circuit_breaker_threshold = z(h, pht.circuit_breaker_threshold);
    pht.circuit_breaker_extended_cooldown_s = u(i, pht.circuit_breaker_extended_cooldown_s);
    pht.health_probe_timeout_s = u(j, pht.health_probe_timeout_s);
    pht.slow_cooldown_s = u(k, pht.slow_cooldown_s);
    pht.outage_threshold = z(l, pht.outage_threshold);

    apply_adaptive_ttl_settings(db, &mut pht).await;
    pht
}

/// Applica il TTL adattivo del cooldown lungo per tipo d'errore (governance, mig 0523,
/// default OFF -> cooldown lungo invariato). Estratto da `fetch_provider_health_timings`.
async fn apply_adaptive_ttl_settings(
    db: &sqlx::PgPool,
    pht: &mut provider_cooldown::ProviderHealthTimings,
) {
    let (s_adaptive_ttl, s_adaptive_ttl_min) = tokio::join!(
        settings::get_setting(db, "agent.governance.cooldown_adaptive_ttl"),
        settings::get_setting(db, "agent.governance.cooldown_adaptive_ttl_min_s"),
    );
    pht.adaptive_billing_cooldown_enabled = parse_bool_or(
        s_adaptive_ttl.ok().flatten(),
        pht.adaptive_billing_cooldown_enabled,
    );
    pht.adaptive_billing_cooldown_min_s = parse_u64_or(
        s_adaptive_ttl_min.ok().flatten(),
        pht.adaptive_billing_cooldown_min_s,
    );
}

/// Avvia i probe periodici provider (`provider_health_probe`, interval default 300s)
/// e modello (`model_health_probe`, interval 1800s, auto-disable dopo N fallimenti).
/// Rilevano cooldown/quota e modelli broken PRIMA del primo errore utente. Config
/// DB-driven, env come override. Estratto da `spawn_background_workers`.
async fn spawn_provider_and_model_probes(state: &AppState) {
    let (s_probe_enabled, s_probe_interval, s_model_enabled, s_model_interval, s_model_threshold) = tokio::join!(
        settings::get_setting(&state.db, "provider_health_probe_enabled"),
        settings::get_setting(&state.db, "provider_health_probe_interval_s"),
        settings::get_setting(&state.db, "model_health_probe_enabled"),
        settings::get_setting(&state.db, "model_health_probe_interval_s"),
        settings::get_setting(&state.db, "model_health_probe_failure_threshold"),
    );

    let probe_enabled = setting_flag_enabled(s_probe_enabled.ok().flatten(), true);
    let probe_interval = parse_u64_or(s_probe_interval.ok().flatten(), 300);
    provider_health_probe::spawn_health_probe(
        std::sync::Arc::new(state.orchestrator.clone()),
        state.db.clone(),
        probe_enabled,
        probe_interval,
    );

    let model_probe_enabled = setting_flag_enabled(s_model_enabled.ok().flatten(), true);
    let model_probe_interval = parse_u64_or(s_model_interval.ok().flatten(), 1800);
    let model_probe_threshold = s_model_threshold
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
}

/// Avvia i watchdog/observer infrastrutturali fire-and-forget: task_watchdog
/// (dipendenze Qdrant/embedder + task bloccati), services_watchdog (microservizi
/// Nexus), service_observer (app utente), process_resume, fanin_worker (ripresa run
/// padre su fan-in subagenti) e monitor_seed. Config DB-driven. Estratto da
/// `spawn_background_workers` (ordine invariato).
fn spawn_infra_watchdogs(state: &AppState) {
    task_watchdog::spawn_task_watchdog(
        state.db.clone(),
        std::sync::Arc::new(state.orchestrator.clone()),
        state.dependency_status.clone(),
        state.agent_channels.clone(),
    );
    services_watchdog::spawn_services_watchdog(state.db.clone());
    crate::project_workspace::service_observer::spawn_service_observer(state.clone());
    process_resume::spawn_process_resume_worker(state.clone());
    fanin_worker::spawn_fanin_worker(state.clone());
    crate::project_workspace::monitor_seed::spawn_monitor_seed_worker(state.clone());
    // One-shot: chiude le diagnosi 'diagnosing' orfane di run di rimedio morti
    // col processo precedente (la chiusura normale vive in un task in-memory).
    crate::project_workspace::resource_violation_remediation::spawn_stale_diagnosing_reaper(
        state.clone(),
    );
}

/// Avvia i worker catalogo/telemetria/retention: catalog_sync (ai_price_catalog dal
/// JSON LiteLLM, default 12h), provider_balance_sync (endpoint di saldo di
/// deepseek/openrouter/kimi, 15min),
/// routing_matrix_auto_promoter (ricostruisce la matrix dal catalog ogni 6h,
/// preserva manual_override), tool-result cache cleanup e db_retention (pota
/// checkpoint + telemetria). Config DB-driven. Estratto da `spawn_background_workers`.
async fn spawn_catalog_and_retention_workers(state: &AppState) {
    let (s_catalog_enabled, s_catalog_interval, s_ap_enabled, s_ap_interval) = tokio::join!(
        settings::get_setting(&state.db, "model_catalog_sync_enabled"),
        settings::get_setting(&state.db, "model_catalog_sync_interval_s"),
        settings::get_setting(&state.db, "routing_matrix_auto_promote_enabled"),
        settings::get_setting(&state.db, "routing_matrix_auto_promote_interval_s"),
    );

    let catalog_sync_enabled = setting_flag_enabled(s_catalog_enabled.ok().flatten(), true);
    let catalog_sync_interval = parse_u64_or(s_catalog_interval.ok().flatten(), 43200);
    catalog_sync_worker::spawn_catalog_sync_worker(
        state.db.clone(),
        catalog_sync_enabled,
        catalog_sync_interval,
    );

    provider_balance_sync::spawn_provider_balance_sync(state.db.clone(), true, 900);

    let ap_enabled = setting_flag_enabled(s_ap_enabled.ok().flatten(), true);
    let ap_interval = parse_u64_or(s_ap_interval.ok().flatten(), 21600);
    routing_matrix_auto_promoter::spawn_routing_matrix_auto_promoter(
        state.db.clone(),
        ap_enabled,
        ap_interval,
    );

    agent_tool_result_cache::start_cleanup_worker(state.db.clone());
    // Retention DB (regola H): pota checkpoint dei run terminali + TTL telemetria.
    db_retention::start_retention_worker(state.db.clone());
}

/// Avvia i server gRPC opzionali (ToolRunner + AgentRouter) nell'ordine originale.
/// Estratto da `spawn_background_workers` (comportamento invariato).
async fn spawn_grpc_servers(state: &AppState) {
    spawn_tool_runner_grpc(state).await;
    spawn_agent_router_grpc(state).await;
}

/// ToolRunner gRPC server (Fase 1, tool read_file/str_replace/...). Priorita'
/// abilitazione: env ENABLE_TOOL_RUNNER=1 > settings DB > default OFF. Indirizzo:
/// env TOOL_RUNNER_ADDR > DB > hardcoded. Estratto da `spawn_grpc_servers`.
async fn spawn_tool_runner_grpc(state: &AppState) {
    let env_tr = std::env::var("ENABLE_TOOL_RUNNER").ok().as_deref() == Some("1");
    let (s_tr_enabled, s_tr_addr) = tokio::join!(
        settings::get_setting(&state.db, "tool_runner_enabled"),
        settings::get_setting(&state.db, "tool_runner_addr"),
    );
    let tr_db_enabled = s_tr_enabled
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !(env_tr || tr_db_enabled) {
        tracing::info!("ToolRunner gRPC: disabilitato (tool_runner_enabled=false in DB)");
        return;
    }
    let tool_runner_addr_str = std::env::var("TOOL_RUNNER_ADDR")
        .ok()
        .or_else(|| s_tr_addr.ok().flatten().map(|v| v.trim().to_string()))
        .unwrap_or_else(|| "127.0.0.1:50071".to_string());
    let addr: SocketAddr = tool_runner_addr_str
        .parse()
        .expect("tool_runner_addr (DB o env TOOL_RUNNER_ADDR) non valido");
    let deps = tool_runner_server::ToolRunnerDeps {
        db: state.db.clone(),
        neural: state.orchestrator.neural.clone(),
        playwright_channels: state.playwright_channels.clone(),
        dependency_status: state.dependency_status.clone(),
        project_channels: state.project_channels.clone(),
        monitor_registry: state.monitor_registry.clone(),
        port_registry: state.port_registry.clone(),
    };
    if let Err(e) = tool_runner_server::spawn_tool_runner_server(deps, addr).await {
        tracing::error!("ToolRunner server: avvio fallito: {e}");
    }
}

/// AgentRouter gRPC server (Fase 5f, Q-Learning router). Priorita' abilitazione:
/// env ENABLE_AGENT_ROUTER=1 > settings DB > default OFF. Indirizzo: env
/// AGENT_ROUTER_ADDR > DB > hardcoded. Estratto da `spawn_grpc_servers`.
async fn spawn_agent_router_grpc(state: &AppState) {
    let env_ar = std::env::var("ENABLE_AGENT_ROUTER").ok().as_deref() == Some("1");
    let ar_db_enabled = settings::get_setting(&state.db, "agent_router_enabled")
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !(env_ar || ar_db_enabled) {
        tracing::info!("AgentRouter gRPC: disabilitato (agent_router_enabled=false in DB)");
        return;
    }
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
}

/// Parsa un `Option<String>` (valore setting) a `u64`, con default se assente/non valido.
fn parse_u64_or(opt: Option<String>, default: u64) -> u64 {
    opt.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Parsa un `Option<String>` (valore setting) a `usize`, con default se assente/non valido.
fn parse_usize_or(opt: Option<String>, default: usize) -> usize {
    opt.and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// Interpreta un `Option<String>` (valore setting) come booleano, col `default`
/// quando il valore e' assente o fuori dal vocabolario unico.
fn parse_bool_or(opt: Option<String>, default: bool) -> bool {
    opt.and_then(|v| nexus_auth::parse_setting_bool(&v))
        .unwrap_or(default)
}

/// Applica il single-instance guard (Unix: flock esclusivo per porta; su Windows
/// e' garantito dal Service Manager, quindi cfg-gated), binda il listener HTTP con
/// socket NON ereditabile (Windows), installa il graceful shutdown (SIGTERM/Ctrl-C
/// -> flush NexusBridge + safety-net force-exit 10s) e serve l'app axum. Estratta
/// da `main` (comportamento invariato): ritorna l'errore di bind/serve che `main`
/// propaga con `?`.
async fn serve_http(state: AppState, mcp_http_port: u16) -> anyhow::Result<()> {
    let app = routes::build_app_router(state, build_cors());

    // Single-instance guard (vedi `acquire_single_instance_lock`, solo Unix).
    #[cfg(unix)]
    acquire_single_instance_lock(mcp_http_port);

    let addr = SocketAddr::from(([0, 0, 0, 0], mcp_http_port));
    tracing::info!("mcp-core listening on {}", addr);

    // Bind via std::net per marcare il socket NON ereditabile su Windows PRIMA che
    // l'agente spawni figli (dev server): altrimenti un figlio eredita l'handle di
    // :4000 e, orfano dopo un crash/restart, blocca il re-bind (WSAEADDRINUSE,
    // os error 10048 -> crash loop WinSW). Punto unico: sandbox::make_socket_non_inheritable.
    let listener = {
        let std_listener = std::net::TcpListener::bind(addr)?;
        std_listener.set_nonblocking(true)?;
        #[cfg(windows)]
        crate::sandbox::make_socket_non_inheritable(&std_listener);
        tokio::net::TcpListener::from_std(std_listener)?
    };

    // `into_make_service_with_connect_info` porta l'indirizzo del chiamante fino
    // ai middleware: senza, `internal_only_middleware` non puo' distinguere una
    // chiamata locale da una che arriva dalla rete, e (per costruzione) rifiuta
    // tutto il blocco `/internal/*`.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(graceful_shutdown_signal())
    .await?;
    Ok(())
}

/// Layer CORS per il frontend (origin da `FRONTEND_URL`, default localhost:3000).
/// Estratto da `serve_http` (comportamento invariato).
fn build_cors() -> CorsLayer {
    let frontend_origin =
        std::env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    CorsLayer::new()
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
        .allow_credentials(true)
}

/// Single-instance guard (regola L): un flock esclusivo non-bloccante legato alla
/// porta garantisce UNA sola istanza di mcp-core; se un'altra e' gia' viva usciamo
/// SUBITO invece di coesistere sulla porta servendo richieste da codice vecchio.
/// Il File viene "dimenticato" cosi' il lock resta tenuto per tutta la vita del
/// processo (il kernel lo rilascia alla terminazione). Solo Unix: su Windows il
/// single-instance e' gia' garantito dal Service Manager (WinSW/SCM). Estratto da
/// `serve_http` (comportamento invariato).
#[cfg(unix)]
fn acquire_single_instance_lock(mcp_http_port: u16) {
    use std::os::unix::io::AsRawFd;
    let lock_path = std::env::var("NEXUS_MCP_CORE_LOCK")
        .unwrap_or_else(|_| format!("/tmp/nexus-mcp-core-{mcp_http_port}.lock"));
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(lock_file) => {
            let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                eprintln!(
                    "mcp-core: un'altra istanza e' gia' attiva (lock {lock_path} occupato). \
                     Esco per non coesistere sulla porta {mcp_http_port} (evita richieste \
                     servite a caso da codice vecchio). Ferma il processo vecchio e riprova."
                );
                std::process::exit(1);
            }
            std::mem::forget(lock_file); // tieni il lock per tutta la vita del processo
            tracing::info!("mcp-core: single-instance lock acquisito ({lock_path})");
        }
        Err(e) => {
            tracing::warn!(
                "mcp-core: impossibile aprire il lock file {lock_path}: {e} (proseguo senza guard)"
            );
        }
    }
}

/// Attende SIGTERM/Ctrl-C, poi arma un force-exit di sicurezza a 10s (limite ASSOLUTO
/// dello shutdown: se il flush o SSE/long-poll in-flight si bloccano il processo esce
/// comunque — la unit systemd ha TimeoutStopSec=15 come ulteriore rete) e fa il flush
/// best-effort del NexusBridge (Q-table + replication:pending) con timeout proprio di
/// 5s, cosi' un DB lento non tiene in ostaggio il processo. Estratto da `serve_http`
/// (comportamento invariato); su Unix diagnostica l'origine del SIGTERM.
async fn graceful_shutdown_signal() {
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
                tracing::warn!(
                    "SIGTERM ricevuto — avvio graceful shutdown.{}",
                    diagnose_signal_origin()
                );
            },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Ctrl-C ricevuto — avvio graceful shutdown");
    }

    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        tracing::warn!(
            "shutdown timeout 10s superato (flush appeso o SSE/long-poll in-flight), force-exit"
        );
        std::process::exit(0);
    });

    if let Some(bridge) = nexus_bridge::NexusBridge::global() {
        if tokio::time::timeout(std::time::Duration::from_secs(5), bridge.shutdown())
            .await
            .is_err()
        {
            tracing::warn!(
                "NexusBridge.shutdown() oltre 5s (DB lento o lock conteso): \
                 procedo con lo shutdown senza attendere il flush"
            );
        }
    }
}
/// Verifica la disponibilita' della sandbox Docker (imposta il flag globale),
/// costruisce la `AppState`, inizializza il registry globale dei pool per-progetto
/// (separazione DB) ed esegue il boot-recovery: reap dei run 'running' orfani (await,
/// deve concludere PRIMA del bind HTTP) + mark-only dei processi per-progetto stale
/// (tokio::spawn). Estratta da `main` (ordine e side-effect invariati). Consuma db,
/// redis, orchestrator, template_cache e il registro porte; ritorna la `AppState`.
async fn build_app_state(
    db: PgPool,
    redis: redis::aio::MultiplexedConnection,
    orchestrator: Orchestrator,
    template_cache: prompt_templates::TemplateCache,
    port_registry_cache: port_registry::PortRegistryCache,
) -> AppState {
    // Verifica disponibilità sandbox Docker all'avvio e imposta il flag globale.
    let sandbox_available = sandbox::is_sandbox_available().await;
    sandbox::set_sandbox_available(sandbox_available);
    let (stato_sb, effetto_sb) = if sandbox_available {
        ("attiva", "isolato in container nexus-sandbox")
    } else {
        ("non disponibile", "eseguito con env filtrato (fallback)")
    };
    tracing::info!(
        sandbox = sandbox_available,
        "Sandbox Docker {stato_sb}: ogni processo agente sarà {effetto_sb}"
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
        port_registry: port_registry_cache,
        dependency_status: std::sync::Arc::new(task_watchdog::DependencyStatus::new()),
        indexing_projects: Arc::new(DashSet::new()),
        watching_projects: Arc::new(DashSet::new()),
        project_channels: nexus_events::dispatcher::new_registry(),
        monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        boot_at: std::time::Instant::now(),
    };
    // Singleton globale per emit da contesti senza &ProjectChannels (NexusToolHandler).
    nexus_events::dispatcher::init_global(state.project_channels.clone());

    // Boot-recovery run/processi orfani (vedi `run_boot_recovery`). Il registro
    // dei pool per-progetto non ha piu' un'inizializzazione da attendere: vive in
    // `nexus_project_pools` e si popola alla prima risoluzione.
    run_boot_recovery(&state).await;

    state
}

/// Boot-recovery a `AppState` costruito: reap dei run 'running' orfani (mig 0392,
/// await: deve concludere PRIMA del bind HTTP — ogni 'running' e' orfano del processo
/// precedente e va marcato 'interrupted' per sbloccare il gate 409; esclude
/// 'awaiting_confirmation' resumibile) + mark-only dei processi per-progetto stale
/// (tokio::spawn, non blocca l'avvio; il re-attach dei processi vivi resta al
/// watchdog periodico). Il registry pool DEVE essere gia' inizializzato, altrimenti
/// `project_data_pool_from` ritorna un errore tipizzato (`ProjectDbError`) e il
/// progetto viene SALTATO con WARN per quel giro — niente piu' fallback silenzioso
/// al meta-DB, che lasciava i run per-progetto zombie (causa radice del bug
/// "chat cieca sul run dopo restart"). Estratta da `build_app_state`.
async fn run_boot_recovery(state: &AppState) {
    let _ = run_reaper::reap_orphaned_runs_at_boot(&state.db).await;
    let db_recover = state.db.clone();
    tokio::spawn(mark_stale_project_processes_failed(db_recover));
}

/// Inizializza observability (tracing), variabili d'ambiente infrastrutturali,
/// catalog tool globale, pool DB e NexusBridge. Ritorna il pool DB e la porta HTTP
/// risolta dal DB (regola G). Estratta da `main` (ordine e comportamento invariati);
/// se il DB e' irraggiungibile propaga l'errore che `main` gestisce con `?`.
/// Inizializza il logging. Senza la feature `tokio-console` e' la sola coppia
/// EnvFilter + fmt di sempre.
#[cfg(not(feature = "tokio-console"))]
fn init_tracing() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Variante con il server di diagnostica del runtime in ascolto (porta 6669).
///
/// Il filtro sta sul layer di FORMATTAZIONE, non sul registry: da globale
/// scarterebbe gli eventi `tokio=trace`/`runtime=trace` su cui si regge la
/// console, che resterebbe vuota senza dire perche' (l'errore classico di questa
/// integrazione). Cosi' i log restano quelli di sempre e la console vede tutto.
///
/// Build ed uso, da PowerShell nella radice del repo:
///
/// ```text
/// $env:RUSTFLAGS = "--cfg tokio_unstable"
/// $env:CARGO_TARGET_DIR = "target-console"   # non invalida la cache normale
/// cargo build -p mcp-core --features tokio-console
/// # avviare il binario prodotto al posto del servizio, poi:
/// tokio-console http://127.0.0.1:6669
/// ```
///
/// Il target dir separato non e' un vezzo: `--cfg tokio_unstable` cambia la
/// configurazione di compilazione di tokio, quindi ricompila l'intero albero e
/// senza la separazione butterebbe via anche la cache delle build normali.
#[cfg(feature = "tokio-console")]
fn init_tracing() {
    use tracing_subscriber::Layer;

    tracing_subscriber::registry()
        .with(console_subscriber::spawn())
        .with(
            tracing_subscriber::fmt::layer().with_filter(tracing_subscriber::EnvFilter::new(
                std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            )),
        )
        .init();

    tracing::info!(
        "tokio-console attivo su 127.0.0.1:6669 (build di diagnostica, non di esercizio)"
    );
}

async fn init_infrastructure() -> anyhow::Result<(PgPool, u16)> {
    dotenvy::dotenv().ok();

    init_tracing();

    // Prima misura utile del processo: quale artefatto sta girando. Legge (e
    // memoizza) l'mtime del proprio eseguibile ORA, mentre il file e' ancora
    // quello da cui siamo partiti; lo stesso valore finira' su `/health`.
    let stamp = nexus_types::build_info::running_binary();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        build_time = %stamp.wire_value(),
        build_time_source = ?stamp.source,
        "mcp-core: binario in esecuzione"
    );

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable".to_string()
    });

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

    Ok((db, mcp_http_port))
}

/// Inizializza Redis ed espone client Redis + pool DB a `provider_cooldown`, poi
/// ripristina i cooldown billing sopravvissuti al riavvio (da Redis e, complementare,
/// dal DB persistente `nexus_provider_health`). Ritorna la connessione Redis.
/// Estratta da `main` (ordine e comportamento invariati); propaga l'errore di init
/// Redis con `?`.
async fn init_redis_and_cooldowns(
    db: &sqlx::PgPool,
    redis_url: &str,
) -> anyhow::Result<redis::aio::MultiplexedConnection> {
    let redis = cache::init_redis(redis_url).await?;

    // Espone il client Redis a `provider_cooldown` per la persistenza dei
    // cooldown lunghi (billing/quota). Senza questo, un restart di mcp-core
    // perderebbe i cooldown in-memory e il LED tornerebbe verde anche se
    // il provider e' realmente giu' (caso utente "LED openai verde").
    crate::provider_cooldown::init_redis_client(redis.clone());

    // Espone il pool DB a `provider_cooldown` per la persistenza TTL del billing
    // cooldown su nexus_provider_health (fonte GIUSTA: ha scadenza, non disabilita
    // il catalog). Sostituisce l'ex propagazione is_enabled=false/is_active=false.
    crate::provider_cooldown::init_db_pool(db.clone());

    // Ripristina cooldown billing provider sopravvissuti al riavvio (persistiti su Redis).
    restore_billing_cooldowns_from_redis(redis.clone()).await;

    // Restore COMPLEMENTARE dal DB persistente (ADR 0020): se Redis e' stato
    // svuotato/riavviato, il blocco sopra non ripristina nulla e il gate parte
    // VUOTO -> il primo run dopo il restart "scopre" i provider esausti
    // chiamandoli (anthropic 400 / openai 429 ad ogni turno). nexus_provider_health
    // e' la fonte persistente piu' affidabile: riallinea il gate allo stato noto
    // cosi' il run li salta senza ri-testarli (il polling resta l'unico tester).
    crate::provider_cooldown::restore_billing_cooldowns_from_db(db).await;

    Ok(redis)
}

/// L'avvio di mcp-core, dentro un runtime tokio gia' costruito dal chiamante.
///
/// Sta nella LIB e non nel binario perche' un `bin` non e' linkabile: finche'
/// queste 215k righe erano un binario puro, nessun crate poteva usarle e
/// nessun test poteva esercitarle dall'esterno — i test in `tests/` parlavano
/// col servizio via HTTP perche' non avevano alternativa. Il binario resta un
/// guscio che costruisce il runtime e chiama qui (vedi `src/main.rs`).
pub async fn run() -> anyhow::Result<()> {
    // Orchestratore d'avvio: ogni fase e' un helper di modulo coeso (init infra,
    // riconciliazione processi, redis+cooldown, cache routing, orchestrator,
    // AppState+boot-recovery, worker background, HTTP). L'ordine di inizializzazione
    // e' load-bearing e resta identico al pre-refactor.
    let (db, mcp_http_port) = init_infrastructure().await?;

    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    // Riconciliazione META dei processi 'running'/'starting' stale (vedi
    // `reconcile_stale_processes`). Il reap dei run orfani e' in `build_app_state`
    // (regola H).
    reconcile_stale_processes(&db).await;

    // Redis + esposizione a provider_cooldown + restore cooldown billing.
    let redis = init_redis_and_cooldowns(&db, &redis_url).await?;

    // Client zero-sized: delega all'embedder ONNX in-process e al gateway. Non
    // apre canali, quindi non c'e' nulla da connettere ne' da ritentare.
    let neural_client = NeuralCoreClient::new();
    let template_cache = prompt_templates::TemplateCache::new();

    // Cache di routing/porte + singleton build graph (vedi `init_routing_and_port_caches`).
    let caches = init_routing_and_port_caches(&db).await;

    // GC periodico delle porte dynamic orfane (vedi `spawn_port_gc`).
    spawn_port_gc(&db).await;

    // ADR 0028: cintura race-window del systemd --user manager (solo Linux/systemd).
    #[cfg(not(windows))]
    crate::project_workspace::user_manager::ensure_user_manager(&db).await;

    // Orchestrator (gateway PATH A/B) consumando le cache; ritorna anche il registro
    // porte che sopravvive nella AppState (vedi `build_orchestrator`).
    let (orchestrator, port_registry_cache) =
        build_orchestrator(&db, neural_client, template_cache.clone(), caches).await;

    // Sandbox + AppState + boot-recovery (vedi `build_app_state`).
    let state = build_app_state(db, redis, orchestrator, template_cache, port_registry_cache).await;

    // Worker fire-and-forget e loop periodici, nell'ordine esatto (vedi
    // `spawn_background_workers`).
    spawn_background_workers(&state).await;

    // CORS + router + single-instance guard + bind + graceful shutdown + serve
    // (vedi `serve_http`, comportamento invariato).
    serve_http(state, mcp_http_port).await
}

/// Probe TCP rapido (500ms) al ToolRunner gRPC: senza questo l'AI non puo'
/// invocare tool (read_file, str_replace, ...) e fallirebbe silenziosamente.
/// Indirizzo: env var (override emergenza) > DB (canonico) > hardcoded.
/// Estratta da `health` (comportamento invariato).
async fn probe_tool_runner_grpc(state: &AppState) -> bool {
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
}

/// Somma i conteggi dashboard (run ultimi 30 giorni, job attivi) su tutti i DB
/// per-progetto e restituisce lo stato del job `shadow_db_validation` piu' recente
/// cross-progetto. Estratta da `dashboard`. Best-effort: un progetto col DB non
/// disponibile viene saltato con WARN (niente fallback al meta, mig 0527).
async fn aggregate_project_dashboard_stats(
    db: &PgPool,
) -> (i64, i64, Option<(chrono::DateTime<chrono::Utc>, String)>) {
    let mut total_runs: i64 = 0;
    let mut active_jobs: i64 = 0;
    let mut latest_shadow: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
    for project_id in project_db_routes::list_all_project_ids(db).await {
        let pool = match project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "dashboard stats: DB progetto non disponibile, progetto saltato per questo giro");
                continue;
            }
        };

        total_runs += sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM orchestrator_runs WHERE created_at > NOW() - INTERVAL '30 days'",
        )
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        active_jobs += sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('queued', 'running')",
        )
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        if let Ok(Some(row)) = sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, String)>(
            "SELECT created_at, status FROM jobs WHERE job_type = 'shadow_db_validation' ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        {
            if latest_shadow.as_ref().is_none_or(|(ts, _)| row.0 > *ts) {
                latest_shadow = Some(row);
            }
        }
    }
    (total_runs, active_jobs, latest_shadow)
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
    let tools_grpc_ok = probe_tool_runner_grpc(&state).await;

    // NB: il vecchio probe `brain_rest` (TCP :8001) e' stato rimosso. Il brain
    // Python e' stato eliminato e i suoi endpoint vivono ora in mcp-core (coperti
    // da neural_core); sondare la 8001 dava sempre False, degradando l'health a
    // vita. Anche la UI (status-bar / ide-shell) ha gia' rimosso quel LED.
    let status = if db_ok && redis_ok && tools_grpc_ok {
        "ok"
    } else {
        "degraded"
    };

    // `build_time`/`build_time_source` non si passano da qui: li popola
    // `HealthSummary::new` dal binario in esecuzione (vedi domain.rs).
    Json(HealthSummary::new(
        "mcp-core",
        env!("CARGO_PKG_VERSION"),
        status,
        Utc::now(),
        domain::ComponentHealth {
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
        },
    ))
}

/// Somma token e costo consumati negli ultimi 30 giorni dai record finalizzati di
/// `ai_usage_ledger` (tabella globale sul meta). Estratta da `dashboard`
/// (comportamento invariato): ritorna `None` se la query fallisce o non ha righe.
async fn fetch_token_stats(db: &PgPool) -> Option<domain::TokenStats> {
    sqlx::query_as::<_, domain::TokenStats>(
        r#"
        SELECT
            COALESCE(SUM(total_tokens), 0) AS total_consumed,
            COALESCE(SUM(total_cost), 0) AS total_cost
        FROM ai_usage_ledger
        WHERE created_at > NOW() - INTERVAL '30 days'
          AND status = 'finalized'
        "#,
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

async fn dashboard(State(state): State<AppState>) -> Json<serde_json::Value> {
    let token_stats = fetch_token_stats(&state.db).await;

    // Conteggio quality finding APERTI dalla fonte unica `project_quality_findings`
    // (regola L). La vecchia tabella `quality_findings` (mig 0001) non e' mai stata
    // popolata ed e' stata droppata (mig 0487); per giunta la query precedente
    // filtrava su `status = 'open'` — colonna mai esistita su quella tabella —
    // quindi falliva sempre e `unwrap_or(0)` mascherava l'errore mostrando un
    // cronico 0 in dashboard. "Aperto" = non risolto e non falso positivo.
    let quality_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_quality_findings \
         WHERE fixed_at IS NULL AND (is_false_positive = FALSE OR is_false_positive IS NULL) \
         AND (is_auto_suppressed = FALSE OR is_auto_suppressed IS NULL)",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // orchestrator_runs e jobs sono MIGRATE al DB per-progetto (separazione DB):
    // a flag ON le copie meta sono vuote. Aggregazione cross-progetto delegata a
    // `aggregate_project_dashboard_stats` (behavior-preserving). ai_usage_ledger e
    // project_quality_findings restano sul meta (tabelle globali, non toccate).
    let (total_runs, active_jobs, latest_shadow) =
        aggregate_project_dashboard_stats(&state.db).await;
    let shadow_db_status = latest_shadow
        .map(|(_, status)| status)
        .unwrap_or_else(|| "no_runs".to_string());

    let stats = token_stats.unwrap_or_default();

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
