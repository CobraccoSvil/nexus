//! Service Observer — osservabilita' runtime delle APP UTENTE (mig 0355/0356).
//!
//! Worker periodico, fratello di `services_watchdog` ma con scope DISGIUNTO:
//! qui si monitorano i servizi `{slug}-*.service` dei progetti utente (unit
//! systemd `--user` su Linux, processi gestiti `agent_processes` kind='service'
//! su Windows), NON i microservizi Nexus (che restano al watchdog). L'observer
//! NON riavvia: osserva e diagnostica. Copre in un solo ciclo:
//!   - cap 4: metriche OS per processo -> evento ServiceMetrics
//!   - cap 3: anomaly detection (cpu/rss/restart/error-rate) -> ServiceAnomaly
//!   - cap 1: crash detection nei log -> ServiceCrashDetected (+ auto-debug,
//!            vedi `remediation`, gated da agent.observer.auto_diagnose_enabled)
//!
//! Regole: G (tutte le soglie in settings, lette a ogni ciclo), E (solo unit
//! `{slug}-` e PID del progetto), L (metriche via `process_util` cross-platform,
//! readiness via `port_recovery::listening_ports`, enumerazione Windows via
//! `list_services_windows`; un solo loop per 3 capacita'), M (lo STATO del
//! servizio si legge da segnali
//! strutturati -- process_alive + exit_code/status di `agent_processes` su
//! Windows, `is-active` su Linux -- mai dal parsing della prosa dei log: i log
//! servono solo a error_rate e alla diagnosi).
//!
//! Anti-spam: anomalie e crash sono emessi sulla TRANSIZIONE (quando compaiono),
//! non a ogni ciclo finche' persistono (stato in-memory per unit).
//!
//! Sorgenti dati astratte per OS (`#[cfg(unix)]` / `#[cfg(windows)]`):
//!   - Unix: systemd `--user` (enum/stato/restart/env) + `/proc` (metriche via
//!     process_util) + journalctl (log). Comportamento invariato.
//!   - Windows: i servizi di progetto girano come `agent_processes` (kind=
//!     'service', tabella per-progetto): enumerazione via `list_services_windows`
//!     (services.rs, riuso), stato da `process_alive(pid)` + `status`/`exit_code`,
//!     metriche via Win32 (process_util), log da `output`/`error_output`. Il
//!     conteggio restart nativo non esiste su Windows -> quella specifica anomaly
//!     e' disabilitata (documentato), senza rompere il ciclo.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use nexus_events::event::ProjectEvent;
use sqlx::PgPool;
use tokio::time::sleep;
use uuid::Uuid;

use crate::AppState;

/// Attesa iniziale: lascia stabilizzare l'avvio dei servizi.
const STARTUP_DELAY_S: u64 = 25;
/// Cap di progetti analizzati per ciclo (anti-overhead su molte istanze).
const MAX_PROJECTS_PER_CYCLE: usize = 50;

/// Config dell'observer, riletta dal DB a ogni ciclo (regola G).
#[derive(Debug, Clone)]
struct ObserverConfig {
    enabled: bool,
    interval_s: u64,
    metrics_enabled: bool,
    error_rate_max_per_min: f64,
    restart_rate_max: u32,
    cpu_pct_threshold: f64,
    rss_bytes_threshold: u64,
    auto_diagnose_enabled: bool,
    diagnose_cooldown_seconds: i64,
    diagnose_max_per_hour: i64,
    /// Secondi di attesa dopo l'avvio prima del readiness check TCP (mig 0384).
    readiness_grace_seconds: i64,
    /// Anomalie il cui `updated_at` e' piu' vecchio di questo (l'observer non le
    /// ha piu' confermate via UPSERT per molti cicli: servizio sparito dal bus
    /// --user, rinominato o non piu' anomalo) vengono chiuse come stantie.
    /// Indipendente da systemctl (vedi limite WSL: bus --user cieco). 0 = off.
    anomaly_stale_resolve_seconds: i64,
}

/// Stato runtime per unit, tra i cicli.
#[derive(Debug, Default, Clone)]
struct UnitState {
    /// Tempo CPU cumulativo (secondi) del campione precedente, per il delta CPU.
    /// Normalizzato dalla fonte (process_util), cosi' il calcolo CPU% e' identico
    /// su Unix e Windows.
    prev_cpu_seconds: Option<f64>,
    /// Istante del campione precedente (per il denominatore CPU%).
    prev_sample: Option<Instant>,
    /// NRestarts visto al ciclo precedente.
    prev_restarts: Option<u32>,
    /// Anomalie attualmente attive (metric), per emettere solo sulla transizione.
    active_anomalies: HashSet<String>,
    /// Firma dell'ultimo problema (crash/unhealthy) gestito (anti-ripetizione).
    last_crash_sig: Option<String>,
    /// Timestamp (unix) dell'ultima scansione log incrementale.
    last_log_scan_ts: i64,
    /// (Solo Windows) Lunghezza del buffer log gia' scansionato al ciclo
    /// precedente: l'error-rate conta SOLO le righe-error NUOVE (la coda oltre
    /// questo offset), replicando l'incrementalita' della finestra journalctl.
    /// Il buffer agent_processes e' cumulativo (append con cap 50KB): se si
    /// accorcia (troncamento/nuovo avvio) l'offset si resetta.
    #[cfg_attr(unix, allow(dead_code))]
    prev_log_len: usize,
    /// Valore di `ActiveEnterTimestamp` visto al ciclo precedente: se cambia, e'
    /// un nuovo avvio ("run") -> si resetta il grace di readiness e l'anti-spam.
    prev_active_enter: Option<String>,
    /// Istante in cui e' stato osservato l'avvio corrente (per il grace period
    /// readiness, in-memory: niente parsing del timestamp systemd).
    run_seen_at: Option<Instant>,
    /// Ultimo `active_state` emesso via ServiceStatusChanged (dedup SSE).
    prev_reported_active: Option<String>,
}

// ── Config ───────────────────────────────────────────────────────────────────

async fn load_config(db: &PgPool) -> ObserverConfig {
    async fn s(db: &PgPool, k: &str) -> Option<String> {
        crate::settings::get_setting(db, k).await.ok().flatten()
    }
    fn b(v: Option<String>, def: bool) -> bool {
        match v {
            Some(x) => !matches!(
                x.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ),
            None => def,
        }
    }
    fn f(v: Option<String>, def: f64) -> f64 {
        v.and_then(|x| x.trim().parse::<f64>().ok()).unwrap_or(def)
    }
    fn u(v: Option<String>, def: u32) -> u32 {
        v.and_then(|x| x.trim().parse::<u32>().ok()).unwrap_or(def)
    }
    fn i(v: Option<String>, def: i64) -> i64 {
        v.and_then(|x| x.trim().parse::<i64>().ok()).unwrap_or(def)
    }
    ObserverConfig {
        enabled: b(s(db, "agent.observer.enabled").await, true),
        interval_s: i(s(db, "agent.observer.interval_seconds").await, 15).max(5) as u64,
        metrics_enabled: b(s(db, "agent.observer.metrics_enabled").await, true),
        error_rate_max_per_min: f(s(db, "agent.observer.error_rate_max_per_min").await, 10.0),
        restart_rate_max: u(s(db, "agent.observer.restart_rate_max").await, 3),
        cpu_pct_threshold: f(s(db, "agent.observer.cpu_pct_threshold").await, 90.0),
        rss_bytes_threshold: i(
            s(db, "agent.observer.rss_bytes_threshold").await,
            1_073_741_824,
        )
        .max(0) as u64,
        auto_diagnose_enabled: b(s(db, "agent.observer.auto_diagnose_enabled").await, false),
        diagnose_cooldown_seconds: i(s(db, "agent.observer.diagnose_cooldown_seconds").await, 600),
        diagnose_max_per_hour: i(s(db, "agent.observer.diagnose_max_per_hour").await, 5),
        readiness_grace_seconds: i(s(db, "agent.observer.readiness_grace_seconds").await, 12)
            .max(0),
        anomaly_stale_resolve_seconds: i(
            s(db, "agent.observer.anomaly_stale_resolve_seconds").await,
            300,
        )
        .max(0),
    }
}

// ── Enumerazione servizi utente (scope progetto, regola E) ────────────────────

/// Progetti con slug non vuoto (al piu' MAX_PROJECTS_PER_CYCLE), con anche il
/// `name` (serve al ramo Windows per derivare lo slug di servizio con la stessa
/// formula del pannello, vedi `collect_units` Windows). Il ramo Unix usa lo
/// `slug` (invariato) e ignora il name.
async fn projects_with_slug(db: &PgPool) -> Vec<(Uuid, String, String)> {
    sqlx::query_as::<_, (Uuid, Option<String>, Option<String>)>(
        "SELECT id, slug, name FROM projects WHERE slug IS NOT NULL AND slug <> '' LIMIT $1",
    )
    .bind(MAX_PROJECTS_PER_CYCLE as i64)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(id, slug, name)| slug.map(|s| (id, s, name.unwrap_or_default())))
    .collect()
}

/// Snapshot per-unit dei dati sorgente di un ciclo, aggregati per OS.
///
/// Il `run_cycle` e' cross-platform e legge SOLO da qui: i dettagli OS
/// (systemctl+/proc+journalctl vs agent_processes+Win32) sono confinati nel
/// collector (`collect_units`). `pid`/`active_state` vengono da segnali
/// strutturati (regola M): MainPID+is-active su Linux, pid+status/exit_code di
/// agent_processes su Windows.
struct UnitSample {
    /// Nome unit canonico `{slug}-{short}.service` (chiave stabile per lo stato).
    unit: String,
    /// Stato mappato: "active" | "failed" | "inactive" | "activating" | ...
    active_state: String,
    /// PID del processo servizio (None se non attivo).
    pid: Option<u32>,
    /// Conteggio restart cumulativo (systemd NRestarts). `None` = non disponibile
    /// (Windows: agent_processes non traccia i restart -> anomaly restart off).
    restarts: Option<u32>,
    /// Marcatore opaco dell'avvio corrente ("run"): se cambia tra due cicli e' un
    /// nuovo avvio (reset grace/anti-spam). NON viene parsato, solo confrontato.
    /// Unix: `ActiveEnterTimestamp`. Windows: PID+started_at della riga
    /// agent_processes. `None` = servizio non attivo.
    active_enter: Option<String>,
    /// Testo dei log del RUN corrente, per la diagnosi (startup incluso). Vuoto
    /// -> il chiamante usa la finestra error-rate come fallback.
    run_log: String,
    /// Buffer di log applicativi per lo scan error-rate del ciclo (stdout+stderr).
    /// Unix: finestra journalctl incrementale (calcolata nel loop). Windows:
    /// buffer cumulativo di agent_processes (l'incrementale lo fa il loop via
    /// offset in UnitState). Vuoto su Unix (il loop usa scan_new_logs).
    log_buffer: String,
}

/// Enumera i servizi del progetto e ne aggrega i dati sorgente, per OS.
///
/// - Unix: systemd `--user` (file unit + is-active + MainPID + NRestarts +
///   InvocationID) e journalctl per il run-log. Comportamento invariato.
/// - Windows: `agent_processes` (kind='service'), enumerati con il PUNTO UNICO
///   `list_services_windows` (services.rs, dedup label/fantasma). Stato da
///   `process_alive(pid)` + `status`/`exit_code`; run-log da `output`/
///   `error_output`. Nessun systemctl/journalctl/proc.
#[cfg(unix)]
async fn collect_units(
    _state: &AppState,
    _project_id: Uuid,
    slug: &str,
    _name: &str,
) -> Vec<UnitSample> {
    let mut out = Vec::new();
    for (unit, active_state) in list_user_services(slug).await {
        let pid = unit_main_pid(&unit).await;
        let restarts = unit_restarts(&unit).await;
        let active_enter = unit_active_enter(&unit).await;
        let run_log = scan_run_logs(&unit).await;
        out.push(UnitSample {
            unit,
            active_state,
            pid,
            restarts,
            active_enter,
            run_log,
            // Su Unix i log incrementali arrivano da scan_new_logs nel loop (con
            // finestra temporale journalctl): il buffer aggregato resta vuoto.
            log_buffer: String::new(),
        });
    }
    out
}

// La verifica di identita' del PID (anti-riciclo: creation-time vs started_at)
// e' il PUNTO UNICO `process_util::pid_identity_confirmed` (regola L), condiviso
// con il port_enforcer (attribuzione PID->progetto in detect_all_port_bindings).

/// Windows: enumera i servizi del progetto da `agent_processes` (kind='service')
/// e ne aggrega i dati sorgente. Regola L: la visibilita'/dedup delle label
/// (voci fantasma nascoste, similarita') e' delegata al PUNTO UNICO
/// `visible_windows_services` (services.rs), la stessa logica del pannello
/// Servizi; lo slug e l'unit dai punti unici `project_service_slug`/
/// `service_unit_name`. Regola M: lo stato deriva da `process_alive(pid)` +
/// VALIDAZIONE IDENTITA' del PID + `status`/`exit_code`, non dal parsing dei log.
///
/// Anti-riciclo PID (regola M/H): `agent_processes.pid` NON viene mai azzerato
/// allo stop/crash, e Windows ricicla i PID in modo aggressivo, quindi un pid
/// persistito puo' gia' appartenere a un processo ESTRANEO. `process_alive`/
/// `read_process_metrics` da soli aprono QUALSIASI processo con quel PID: si
/// scambierebbe un crash per "attivo" e si campionerebbero metriche altrui. Per
/// questo il pid e' considerato vivo SOLO se il suo creation-time reale
/// (`process_start_unix`) combacia con lo `started_at` della riga entro
/// `PID_IDENTITY_TOLERANCE_S`. Su Unix non serve: MainPID viene da systemd fresco
/// a ogni ciclo (ramo invariato).
#[cfg(windows)]
async fn collect_units(
    state: &AppState,
    project_id: Uuid,
    _slug: &str,
    name: &str,
) -> Vec<UnitSample> {
    // Slug/unit dai PUNTI UNICI condivisi col pannello (regola L): la formula e'
    // name.to_lowercase().replace([' ','_'],"-"), NON projects.slug (slugify +
    // suffisso -N), altrimenti l'unit divergerebbe da list_services_windows e da
    // nexus_port_allocations.service_unit (diagnosi orfane, readiness saltata).
    let slug = super::services::project_service_slug(name);
    let (rows, visible) = windows_visible_service_rows(state, project_id).await;

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for row in rows {
        // Prima occorrenza per label = riga piu' recente (rows ordinata DESC).
        if !seen.insert(row.0.clone()) || !visible.contains(&row.0) {
            continue;
        }
        out.push(windows_service_sample(&slug, row));
    }
    out
}

/// Carica le righe service di `agent_processes` (ordinate label ASC, created_at
/// DESC) e l'insieme delle label visibili dal PUNTO UNICO `visible_windows_services`
/// (services.rs, dedup label/fantasma). Separazione DB per-progetto: stessa fonte
/// di list_services_windows. Estratto da `collect_units`.
#[cfg(windows)]
async fn windows_visible_service_rows(
    state: &AppState,
    project_id: Uuid,
) -> (Vec<WindowsServiceRow>, std::collections::HashSet<String>) {
    use chrono::{DateTime, Utc};
    // DB progetto non disponibile -> nessuna riga osservabile per questo ciclo
    // (WARN + skip, l'observer riprova al giro successivo).
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "windows_visible_service_rows: DB progetto non disponibile, salto il ciclo"
                );
                return (Vec::new(), std::collections::HashSet::new());
            }
        };
    let rows: Vec<WindowsServiceRow> = sqlx::query_as(
        "SELECT label, status, created_at, pid, exit_code, output, error_output, started_at \
         FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service' \
         ORDER BY label, created_at DESC",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();

    // Dedup/visibilita' dal punto unico: proiezione (label, status, created_at).
    let visible_proj: Vec<(String, String, DateTime<Utc>)> = rows
        .iter()
        .map(|(label, status, created_at, ..)| (label.clone(), status.clone(), *created_at))
        .collect();
    let visible: std::collections::HashSet<String> =
        super::services::visible_windows_services(&visible_proj)
            .into_iter()
            .map(|(label, _running)| label)
            .collect();
    (rows, visible)
}

/// Riga service di `agent_processes` (proiezione usata dal collector Windows).
#[cfg(windows)]
type WindowsServiceRow = (
    String,                                // label
    String,                                // status
    chrono::DateTime<chrono::Utc>,         // created_at
    Option<i32>,                           // pid
    Option<i32>,                           // exit_code
    String,                                // output (stdout)
    String,                                // error_output (stderr)
    Option<chrono::DateTime<chrono::Utc>>, // started_at
);

/// Deriva `(active_state, effective_pid)` di una riga service Windows. Stato
/// strutturato (regola M): la liveness reale via process_alive + VALIDAZIONE
/// IDENTITA' del PID (anti-riciclo: creation-time vs started_at; dato mancante =>
/// non vivo, fail-safe) hanno la precedenza sul campo `status` (che puo' restare
/// 'running' dopo un crash o un riciclo del PID).
#[cfg(windows)]
fn windows_pid_state(
    status: &str,
    pid: Option<i32>,
    exit_code: Option<i32>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> (String, Option<u32>) {
    let pid_u = pid.and_then(|p| u32::try_from(p).ok()).filter(|&p| p > 0);
    let alive = match pid_u {
        Some(p) if crate::process_util::process_alive(p) => {
            crate::process_util::pid_identity_confirmed(p, started_at.map(|t| t.timestamp()))
        }
        _ => false,
    };
    // Vivo E identita' confermata => 'active'; altrimenti morto: exit_code!=0 o
    // status='failed' => 'failed', sennò 'inactive'.
    let active_state = if alive {
        "active".to_string()
    } else if status == "failed" || exit_code.is_some_and(|c| c != 0) {
        "failed".to_string()
    } else {
        "inactive".to_string()
    };
    (active_state, if alive { pid_u } else { None })
}

/// Costruisce un `UnitSample` da una riga service Windows. Estratto da
/// `collect_units` (comportamento invariato).
#[cfg(windows)]
fn windows_service_sample(slug: &str, row: WindowsServiceRow) -> UnitSample {
    let (label, status, _created_at, pid, exit_code, output, error_output, started_at) = row;
    let unit = super::services::service_unit_name(slug, &label);
    let (active_state, effective_pid) = windows_pid_state(&status, pid, exit_code, started_at);

    // Marcatore di run: pid + started_at. Cambia a ogni nuovo avvio (pid nuovo,
    // started_at pure) -> reset grace/anti-spam. Non viene parsato.
    let active_enter =
        effective_pid.map(|p| format!("{p}:{}", started_at.map(|t| t.timestamp()).unwrap_or(0)));

    // Log del run corrente = stdout + stderr accumulati (cap 50KB alla fonte,
    // spawn_agent_process). Servono a error-rate e alla diagnosi LLM.
    let mut run_log = String::with_capacity(output.len() + error_output.len() + 1);
    run_log.push_str(&output);
    if !error_output.is_empty() {
        if !run_log.is_empty() {
            run_log.push('\n');
        }
        run_log.push_str(&error_output);
    }

    UnitSample {
        unit,
        active_state,
        pid: effective_pid,
        // agent_processes non traccia i restart: anomaly 'restart' disabilitata su
        // Windows (regola M/H: nessun dato strutturato -> None, non stime).
        restarts: None,
        active_enter,
        log_buffer: run_log.clone(),
        run_log,
    }
}

/// Unit systemd `{slug}-*.service` con il loro stato active (es. "active",
/// "failed", "inactive", "activating").
///
/// CAUSA RADICE (caso reale: backend morto per ts-node mancante): si enumera dai
/// FILE unit (`list-unit-files`), NON da `list-units`. Un servizio `Type=simple`
/// che muore con exit 0 viene SCARICATO (unloaded) da systemd e sparisce da
/// `list-units --all`, restando invisibile all'observer: non se ne scansionano
/// mai i log ne' lo stato. I file `{slug}-*.service` invece ci sono sempre; per
/// ciascuno interroghiamo lo stato corrente per nome con `is-active` (che
/// funziona anche su unit scaricati).
#[cfg(unix)]
async fn list_user_services(slug: &str) -> Vec<(String, String)> {
    let out = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await;
    let out = match out {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let prefix = format!("{slug}-");
    let mut units = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // Formato list-unit-files: "UNIT_FILE STATE [PRESET]".
        let unit = match line.split_whitespace().next() {
            Some(u) => u,
            None => continue,
        };
        if unit.ends_with(".service") && unit.starts_with(&prefix) {
            let active = unit_active_state(unit).await;
            units.push((unit.to_string(), active));
        }
    }
    units
}

/// `ActiveState` corrente di un'unita' per nome (active|inactive|failed|...).
/// Usa `is-active`, che risponde anche se l'unit e' stato scaricato dalla
/// memoria (a differenza di `list-units`). Exit code != 0 e' normale per
/// inactive/failed: lo stato e' comunque stampato su stdout.
#[cfg(unix)]
async fn unit_active_state(unit: &str) -> String {
    match tokio::process::Command::new("systemctl")
        .args(["--user", "is-active", unit])
        .output()
        .await
    {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                "unknown".to_string()
            } else {
                s
            }
        }
        Err(_) => "unknown".to_string(),
    }
}

/// MainPID di un'unita' systemd --user (0/None se non attiva).
#[cfg(unix)]
async fn unit_main_pid(unit: &str) -> Option<u32> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=MainPID"])
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("MainPID="))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&p| p > 0)
}

/// NRestarts di un'unita'.
#[cfg(unix)]
async fn unit_restarts(unit: &str) -> Option<u32> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=NRestarts"])
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("NRestarts="))
        .and_then(|v| v.trim().parse::<u32>().ok())
}

/// `ActiveEnterTimestamp` dell'unita' (stringa systemd, es. "Sun 2026-06-09
/// 19:25:05 CEST"). Confrontata col valore del ciclo precedente per rilevare un
/// nuovo avvio ("run"): NON la parsiamo, basta sapere se e' cambiata.
#[cfg(unix)]
async fn unit_active_enter(unit: &str) -> Option<String> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=ActiveEnterTimestamp"])
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("ActiveEnterTimestamp="))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "n/a")
}

/// Porte "attese" di un servizio, per il readiness check TCP (cross-tecnologia):
/// un servizio che non ascolta su NESSUNA delle sue porte e' giu', qualunque sia
/// il linguaggio. Unione di due fonti, per robustezza (un unit puo' dichiarare la
/// porta solo in un modo):
///   1. `nexus_port_allocations` (porte allocate dal port_registry);
///   2. variabili `Environment` del unit il cui nome contiene PORT (es. PORT,
///      PORT_BACKEND) — copre gli unit creati fuori dal flusso port_registry.
///
/// Vuoto = servizio senza porta nota (worker): readiness non applicabile.
///
/// La fonte primaria `nexus_port_allocations` (meta-DB) e' cross-platform: e' la
/// stessa tabella che `request_port` popola con `service_unit = {slug}-{short}
/// .service`, formato identico all'unit costruito qui su entrambi gli OS. Su
/// Unix si aggiungono le porte dalle `Environment=` dell'unit (per gli unit
/// creati fuori dal flusso port_registry); su Windows quella seconda fonte non
/// esiste (i servizi sono agent_processes, senza Environment= systemd) e ci si
/// affida alle sole allocazioni registrate.
pub(super) async fn ports_for_unit(db: &PgPool, project_id: Uuid, unit: &str) -> Vec<u16> {
    let mut ports: HashSet<u16> = HashSet::new();
    if let Ok(rows) = sqlx::query_scalar::<_, i32>(
        "SELECT port FROM nexus_port_allocations WHERE project_id = $1 AND service_unit = $2",
    )
    .bind(project_id)
    .bind(unit)
    .fetch_all(db)
    .await
    {
        for p in rows {
            if let Ok(p) = u16::try_from(p) {
                ports.insert(p);
            }
        }
    }
    #[cfg(unix)]
    for p in unit_env_ports(unit).await {
        ports.insert(p);
    }
    ports.into_iter().collect()
}

/// Estrae le porte dalle variabili `Environment` del unit il cui NOME contiene
/// "PORT" (es. `PORT`, `PORT_BACKEND`). `systemctl show --property=Environment`
/// restituisce le coppie KEY=VALUE separate da spazio.
#[cfg(unix)]
async fn unit_env_ports(unit: &str) -> Vec<u16> {
    let out = match tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=Environment"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Environment="))
        .unwrap_or("");
    let mut ports = Vec::new();
    for tok in val.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            if k.to_ascii_uppercase().contains("PORT") {
                if let Ok(p) = v.trim().parse::<u16>() {
                    if p > 0 {
                        ports.push(p);
                    }
                }
            }
        }
    }
    ports
}

/// Chiude (resolved) le diagnosi 'crash' APERTE di un'unita' tornata sana. Punto
/// unico del ciclo di vita dei crash strutturali: quando il servizio e' di nuovo
/// healthy (porta in ascolto / non failed) il problema sparisce dal pannello,
/// simmetrico a `resolve_stale_anomalies` per le anomalie.
///
/// SOLO `open`, mai `diagnosing`. Una diagnosi in `diagnosing` ha un rimedio in
/// corso, e la sua chiusura appartiene al contratto di quel rimedio
/// (`service_recovery`). Prima il predicato includeva `diagnosing`, e questa
/// funzione veniva invocata da `apply_run_reset` al CAMBIO DEL MARCATORE
/// D'AVVIO: bastava che il processo rinascesse — cioe' il riavvio che la
/// remediation stessa aveva appena ordinato — perche' la diagnosi diventasse
/// `resolved`. Il caso reale (gestione-spese, 28/07): il frontend riparte alle
/// 21:29, la diagnosi si chiude, il processo muore subito dopo e il guasto
/// prosegue senza piu' nessuna riga aperta a testimoniarlo. La nascita di un
/// processo non e' una guarigione.
async fn resolve_open_crashes(db: &PgPool, project_id: Uuid, unit: &str) -> Vec<Uuid> {
    sqlx::query_scalar(
        "UPDATE service_diagnoses SET status = 'resolved', resolved_at = NOW() \
         WHERE project_id = $1 AND unit = $2 AND signal_kind = 'crash' \
           AND status = 'open' \
         RETURNING id",
    )
    .bind(project_id)
    .bind(unit)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// PUNTO UNICO (regola L) del criterio "riga-error" per l'error-rate: conta le
/// righe che contengono error/exception/panic/fatal (case-insensitive). Usata
/// dal ramo Unix (journalctl) e dal ramo Windows (buffer agent_processes), cosi'
/// la soglia error_rate_max_per_min ha lo stesso significato su entrambi gli OS.
fn count_error_lines(text: &str) -> u64 {
    let mut error_lines = 0u64;
    for line in text.lines() {
        let low = line.to_lowercase();
        if low.contains("error")
            || low.contains("exception")
            || low.contains("panic")
            || low.contains("fatal")
        {
            error_lines += 1;
        }
    }
    error_lines
}

/// Scansione incrementale dei log dell'unita' dal timestamp `since_unix`.
/// Ritorna (n_righe_error, testo_completo). Usa journalctl one-shot (no follow).
/// Il testo serve alla diagnosi LLM (service_log_diagnose) quando la detection
/// strutturale segnala un servizio non funzionante: niente piu' pattern fissi.
#[cfg(unix)]
async fn scan_new_logs(unit: &str, since_unix: i64) -> (u64, String) {
    let since = chrono::DateTime::from_timestamp(since_unix.max(0), 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-5 min".to_string());
    let out = tokio::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            unit,
            "--since",
            &since,
            "--no-pager",
            "-o",
            "cat",
        ])
        .output()
        .await;
    let text = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return (0, String::new()),
    };
    let error_lines = count_error_lines(&text);
    (error_lines, text)
}

/// Log dell'INTERO run corrente dell'unita' (dal suo avvio), filtrati per
/// `InvocationID` di systemd. Cattura sempre lo startup completo a prescindere
/// da quanti minuti fa il servizio e' partito, ed e' indispensabile per la
/// diagnosi di `port_not_listening`: la riga chiave ("listening on <porta>")
/// e' nello startup, non negli ultimi secondi della finestra error-rate.
/// L'InvocationID e' l'identificatore stabile del run corrente: niente parsing
/// di timestamp/fuso orario. Stringa vuota -> il chiamante usa il suo fallback.
#[cfg(unix)]
async fn scan_run_logs(unit: &str) -> String {
    let inv = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=InvocationID"])
        .output()
        .await
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("InvocationID="))
                .map(|v| v.trim().to_string())
        })
        .filter(|v| !v.is_empty());
    let inv = match inv {
        Some(v) => v,
        None => return String::new(),
    };
    let out = tokio::process::Command::new("journalctl")
        .args([
            "--user",
            &format!("_SYSTEMD_INVOCATION_ID={inv}"),
            "--no-pager",
            "-o",
            "cat",
        ])
        .output()
        .await;
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => String::new(),
    }
}

/// Rimuove le sequenze di escape ANSI (CSI: `ESC [` ... lettera finale `@`-`~`)
/// da un testo. Serve a ripulire le righe di log colorate (nodemon, vite, ...)
/// prima di mostrarle nel pannello Problemi o di passarle al Debugger.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1B}' {
            // Control Sequence Introducer: ESC '[' ... terminatore in '@'..='~'.
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            // Altri ESC isolati: scartati (anche il '[' senza terminatore).
        } else {
            out.push(c);
        }
    }
    out
}

/// Anomalia rilevata dal detector.
#[derive(Debug, Clone, PartialEq)]
struct AnomalySignal {
    metric: String,
    value: f64,
    threshold: f64,
    severity: String,
}

impl AnomalySignal {
    /// Costruttore compatto per ridurre la ripetizione nei push di
    /// `evaluate_anomalies`.
    fn new(metric: &str, value: f64, threshold: f64, severity: &str) -> Self {
        Self {
            metric: metric.into(),
            value,
            threshold,
            severity: severity.into(),
        }
    }
}

/// Valuta le metriche correnti contro le soglie. Funzione pura.
fn evaluate_anomalies(
    active_state: &str,
    cpu_pct: Option<f64>,
    rss_bytes: u64,
    restart_delta: u32,
    error_per_min: f64,
    cfg: &ObserverConfig,
) -> Vec<AnomalySignal> {
    let mut out = Vec::new();
    // B2: un servizio gestito in stato 'failed' e' sempre un problema -> cattura i
    // crash che escono con exit code != 0. (Il caso exit-0/dead, dove systemd vede
    // Result=success, NON e' 'failed': lo copre il crash-detector sui log, cap 1.)
    // Trattato come anomalia 'down' cosi' il ciclo di vita (apertura sulla
    // transizione + auto-resolve quando il servizio torna attivo) e' gia' gestito
    // da resolve_stale_anomalies, senza logica nuova (regola L).
    if active_state == "failed" {
        out.push(AnomalySignal::new("down", 1.0, 0.0, "critical"));
    }
    if let Some(cpu) = cpu_pct {
        if cpu > cfg.cpu_pct_threshold {
            out.push(AnomalySignal::new(
                "cpu",
                cpu,
                cfg.cpu_pct_threshold,
                "warning",
            ));
        }
    }
    if cfg.rss_bytes_threshold > 0 && rss_bytes > cfg.rss_bytes_threshold {
        out.push(AnomalySignal::new(
            "rss",
            rss_bytes as f64,
            cfg.rss_bytes_threshold as f64,
            "warning",
        ));
    }
    if restart_delta > cfg.restart_rate_max {
        out.push(AnomalySignal::new(
            "restart",
            restart_delta as f64,
            cfg.restart_rate_max as f64,
            "critical",
        ));
    }
    if error_per_min > cfg.error_rate_max_per_min {
        out.push(AnomalySignal::new(
            "error_rate",
            error_per_min,
            cfg.error_rate_max_per_min,
            "warning",
        ));
    }
    out
}

// ── Persistenza diagnosi (service_diagnoses) ──────────────────────────────────

/// Punto unico di apertura/aggiornamento di una diagnosi (regola L).
///
/// Per `signal_kind='anomaly'` esegue un UPSERT sull'indice univoco parziale
/// `uniq_service_diagnoses_active_anomaly` (mig 0491): se esiste gia' una
/// anomalia ATTIVA ('open'/'diagnosing') per la chiave
/// (project_id, unit, COALESCE(metric,'')) la riga viene AGGIORNATA
/// (value/threshold/detail freschi, `updated_at=NOW()`, `occurrences += 1`)
/// invece di inserirne una nuova. Questo elimina la duplicazione causata dalla
/// guardia in-memory `active_anomalies`, che si azzera a ogni restart di mcp-core
/// lasciando l'anomalia 'open' nel DB e facendola ri-aprire come "nuova".
///
/// Per `signal_kind='crash'` esegue un UPSERT analogo sull'indice univoco parziale
/// `uniq_service_diagnoses_active_crash` (mig 0562): un solo crash ATTIVO per la
/// chiave (project_id, unit, signal_kind, COALESCE(error_signature_hash,'')). La
/// guardia anti-spam `st.last_crash_sig` e' in-memory e si azzera a ogni restart di
/// mcp-core; senza il vincolo un crash rimasto 'open' nel DB veniva re-inserito come
/// "nuovo", accumulando righe duplicate nel pannello Problemi.
///
/// Per gli altri `signal_kind` non vincolati (es. `build_error`) resta un INSERT
/// puro: ogni evento e' distinto e il suo ciclo di vita lo governa il Debugger.
async fn persist_diagnosis(
    db: &PgPool,
    project_id: Uuid,
    unit: &str,
    signal_kind: &str,
    metric: Option<&str>,
    value: Option<f64>,
    threshold: Option<f64>,
    error_signature_hash: Option<&str>,
    detail: Option<&str>,
) -> Option<Uuid> {
    let row = DiagnosisRow {
        project_id,
        unit,
        signal_kind,
        metric,
        value,
        threshold,
        error_signature_hash,
        detail,
    };
    match signal_kind {
        "anomaly" => upsert_anomaly_diagnosis(db, &row).await,
        "crash" => upsert_crash_diagnosis(db, &row).await,
        _ => insert_event_diagnosis(db, &row).await,
    }
}

/// Campi di una riga `service_diagnoses` da persistere, raggruppati per delegare
/// ai due rami SQL senza superare il limite clippy::too_many_arguments.
struct DiagnosisRow<'a> {
    project_id: Uuid,
    unit: &'a str,
    signal_kind: &'a str,
    metric: Option<&'a str>,
    value: Option<f64>,
    threshold: Option<f64>,
    error_signature_hash: Option<&'a str>,
    detail: Option<&'a str>,
}

/// UPSERT per `signal_kind='anomaly'`: una sola anomalia attiva per
/// (project_id, unit, metric). Il target ON CONFLICT replica esattamente
/// colonne+predicato dell'indice parziale uniq_service_diagnoses_active_anomaly.
async fn upsert_anomaly_diagnosis(db: &PgPool, row: &DiagnosisRow<'_>) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO service_diagnoses
           (project_id, unit, signal_kind, metric, value, threshold,
            error_signature_hash, status, detail)
           VALUES ($1,$2,'anomaly',$3,$4,$5,$6,'open',$7)
           ON CONFLICT (project_id, unit, COALESCE(metric, ''))
             WHERE signal_kind = 'anomaly' AND status IN ('open', 'diagnosing')
           DO UPDATE SET
             value       = EXCLUDED.value,
             threshold   = EXCLUDED.threshold,
             detail      = EXCLUDED.detail,
             updated_at  = NOW(),
             occurrences = service_diagnoses.occurrences + 1
           RETURNING id"#,
    )
    .bind(row.project_id)
    .bind(row.unit)
    .bind(row.metric)
    .bind(row.value)
    .bind(row.threshold)
    .bind(row.error_signature_hash)
    .bind(row.detail)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// UPSERT per `signal_kind='crash'`: un solo crash attivo per
/// (project_id, unit, error_signature_hash). Il target ON CONFLICT replica
/// esattamente colonne+predicato dell'indice parziale
/// uniq_service_diagnoses_active_crash (mig 0562). La guardia anti-spam
/// `st.last_crash_sig` e' in-memory e si azzera a ogni restart di mcp-core: senza
/// questo vincolo un crash rimasto 'open' nel DB veniva re-inserito come "nuovo",
/// accumulando righe duplicate nel pannello Problemi.
async fn upsert_crash_diagnosis(db: &PgPool, row: &DiagnosisRow<'_>) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO service_diagnoses
           (project_id, unit, signal_kind, metric, value, threshold,
            error_signature_hash, status, detail)
           VALUES ($1,$2,'crash',$3,$4,$5,$6,'open',$7)
           ON CONFLICT (project_id, unit, signal_kind, COALESCE(error_signature_hash, ''))
             WHERE signal_kind = 'crash' AND status IN ('open', 'diagnosing')
           DO UPDATE SET
             detail      = EXCLUDED.detail,
             updated_at  = NOW(),
             occurrences = service_diagnoses.occurrences + 1
           RETURNING id"#,
    )
    .bind(row.project_id)
    .bind(row.unit)
    .bind(row.metric)
    .bind(row.value)
    .bind(row.threshold)
    .bind(row.error_signature_hash)
    .bind(row.detail)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// INSERT puro per i `signal_kind` non vincolati (es. `build_error`): ogni evento
/// e' distinto (firma errore propria) e il suo ciclo di vita lo governa il Debugger,
/// quindi non rientra in un vincolo univoco parziale. `anomaly` e `crash` hanno
/// invece un proprio UPSERT dedicato (mig 0491 / 0562).
async fn insert_event_diagnosis(db: &PgPool, row: &DiagnosisRow<'_>) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO service_diagnoses
           (project_id, unit, signal_kind, metric, value, threshold,
            error_signature_hash, status, detail)
           VALUES ($1,$2,$3,$4,$5,$6,$7,'open',$8)
           RETURNING id"#,
    )
    .bind(row.project_id)
    .bind(row.unit)
    .bind(row.signal_kind)
    .bind(row.metric)
    .bind(row.value)
    .bind(row.threshold)
    .bind(row.error_signature_hash)
    .bind(row.detail)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Chiude (status='resolved') le anomalie aperte di un'unita' la cui metrica
/// non supera piu' la soglia. `active_metrics` = metriche attualmente in anomalia
/// per quell'unita'; tutte le diagnosi 'anomaly' aperte con metrica NON presente
/// in quella lista vengono risolte. Se `active_metrics` e' vuoto (servizio sano)
/// chiude tutte le anomalie aperte dell'unita'.
///
/// Simmetrico a `persist_diagnosis`: e' il punto unico che chiude il ciclo di vita
/// delle anomalie, niente worker di cleanup separato (regola L). Si basa sullo
/// stato DB corrente, quindi richiude anche le anomalie "fantasma" rimaste 'open'
/// da prima di un restart (lo stato in-memory `active_anomalies` si perde, il DB no).
/// I signal_kind 'crash'/'build_error' NON sono toccati: non sono stati continui
/// basati su soglia, il loro ciclo di vita lo governa il Debugger.
async fn resolve_stale_anomalies(
    db: &PgPool,
    project_id: Uuid,
    unit: &str,
    active_metrics: &[String],
) -> Vec<Uuid> {
    sqlx::query_scalar(
        r#"UPDATE service_diagnoses
           SET status = 'resolved', resolved_at = NOW()
           WHERE project_id = $1
             AND unit = $2
             AND signal_kind = 'anomaly'
             AND status IN ('open', 'diagnosing')
             AND metric <> ALL($3)
           RETURNING id"#,
    )
    .bind(project_id)
    .bind(unit)
    .bind(active_metrics)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Chiude le diagnosi CONTINUE (anomaly) e STRUTTURALI (crash) di un progetto
/// le cui `unit` NON sono piu' tra i servizi osservati (rinominate/rimosse: lo
/// unit file non esiste piu', quindi `list_user_services` non le elenca nemmeno
/// con `--all`). Queste righe non verrebbero MAI richiuse dal resolve per-unit,
/// perche' `run_cycle` non visita piu' quegli unit: restano 'open' a vita nel
/// pannello Problemi (i veri "fantasma").
///
/// I crash sono inclusi dal fix "unit orfane": `resolve_open_crashes` matcha per
/// unit ESATTA, quindi un crash su un'unit rinominata (es. il vecchio schema
/// label `{slug}-{slug}-frontend-dev.service` -> `frontend`) non puo' essere
/// richiuso da nessuno — nemmeno dal Debugger, che lavora sulla stessa unit.
/// Un crash la cui unit non esiste piu' non e' actionable: e' solo rumore.
/// Le policy_violation NON sono toccate: la loro `unit` e' fittizia
/// (`runtime:port` o un file path) e mai presente tra i servizi osservati;
/// il loro ciclo di vita e' del resource_linter (statiche) e del port_enforcer
/// (runtime).
///
/// Il chiamante DEVE garantire `observed_units` non vuoto: con lista vuota
/// `unit <> ALL('{}')` matcherebbe tutto e azzererebbe ogni diagnosi del
/// progetto su un errore transitorio di systemctl.
async fn resolve_diagnoses_for_absent_units(
    db: &PgPool,
    project_id: Uuid,
    observed_units: &[String],
) -> Vec<Uuid> {
    sqlx::query_scalar(
        r#"UPDATE service_diagnoses
           SET status = 'resolved', resolved_at = NOW()
           WHERE project_id = $1
             AND signal_kind IN ('anomaly', 'crash')
             AND status IN ('open', 'diagnosing')
             AND unit <> ALL($2)
           RETURNING id"#,
    )
    .bind(project_id)
    .bind(observed_units)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Chiude le anomalie il cui `updated_at` e' piu' vecchio di `max_age_seconds`:
/// l'observer non le ha piu' confermate (UPSERT a ogni tick) per molti cicli,
/// quindi il servizio non e' piu' osservato (sparito dal bus --user, rinominato,
/// fermo) o non e' piu' anomalo. Diversamente da `resolve_diagnoses_for_absent_units`
/// NON dipende da `list_user_services` (bus --user cieco in WSL): usa solo il
/// tempo. Gira solo mentre l'observer e' attivo, quindi le anomalie REALI hanno
/// `updated_at` fresco (UPSERT ogni `interval_s`) e non vengono mai toccate.
/// 0 = disabilitato.
async fn resolve_stale_anomalies_by_age(db: &PgPool, max_age_seconds: i64) -> Vec<(Uuid, Uuid)> {
    if max_age_seconds <= 0 {
        return Vec::new();
    }
    sqlx::query_as::<_, (Uuid, Uuid)>(
        r#"UPDATE service_diagnoses
           SET status = 'resolved', resolved_at = NOW()
           WHERE signal_kind = 'anomaly'
             AND status IN ('open', 'diagnosing')
             AND updated_at < NOW() - ($1::bigint * interval '1 second')
           RETURNING project_id, id"#,
    )
    .bind(max_age_seconds)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

fn notify_problems_panel_refresh(project_id: Uuid, resolved_ids: Vec<Uuid>) {
    if resolved_ids.is_empty() {
        return;
    }
    crate::project_workspace::logs::emit_problems_panel_refresh(project_id, resolved_ids);
}

fn notify_problems_panel_refresh_batch(rows: &[(Uuid, Uuid)]) {
    // Delega al punto unico condiviso col port_enforcer (regola L).
    crate::project_workspace::logs::emit_problems_panel_refresh_batch(rows);
}

fn sig_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

// ── Fasi del ciclo, estratte da run_cycle (regola L: helper coesi) ────────────

/// Contesto condiviso di un ciclo, raggruppato per non superare il limite
/// clippy::too_many_arguments negli helper di fase. Riferimenti presi in prestito
/// dal `run_cycle`: nessuna copia di stato.
struct CycleCtx<'a> {
    state: &'a AppState,
    cfg: &'a ObserverConfig,
    project_id: Uuid,
}

/// Campiona le metriche OS del processo servizio (cap 4) e ne emette l'evento.
///
/// `pid` e' gia' il PID VALIDATO dal collector (MainPID fresco su Unix; su Windows
/// solo se vivo e con identita' confermata). Aggiorna i campioni CPU in `st` e
/// ritorna `(cpu_pct, rss_bytes)` per la valutazione anomalie. Estratto da
/// `run_cycle` (comportamento invariato).
fn sample_process_metrics(
    ctx: &CycleCtx<'_>,
    st: &mut UnitState,
    unit: &str,
    pid: Option<u32>,
) -> (Option<f64>, u64) {
    // Le metriche OS arrivano dal PUNTO UNICO cross-platform
    // process_util::read_process_metrics. cpu_seconds e' cumulativo e gia'
    // normalizzato: il delta / dt da la % identica sui due OS. Il PID e' gia'
    // quello giusto (vedi collector), non serve ri-validare.
    let sample = pid.and_then(crate::process_util::read_process_metrics);
    let Some(m) = sample else {
        // Servizio non attivo o PID sparito tra enum e campionamento: azzera i
        // campioni CPU.
        st.prev_cpu_seconds = None;
        st.prev_sample = None;
        return (None, 0);
    };
    let pid = pid.expect("pid presente: sample deriva da Some(pid)");
    let now_inst = Instant::now();
    let mut cpu_pct: Option<f64> = None;
    if let (Some(prev_secs), Some(prev_inst)) = (st.prev_cpu_seconds, st.prev_sample) {
        let dt = now_inst.duration_since(prev_inst).as_secs_f64();
        if dt > 0.0 {
            cpu_pct = Some(((m.cpu_seconds - prev_secs).max(0.0) / dt) * 100.0);
        }
    }
    st.prev_cpu_seconds = Some(m.cpu_seconds);
    st.prev_sample = Some(now_inst);

    if ctx.cfg.metrics_enabled {
        emit_service_metrics(ctx, unit, pid, cpu_pct, &m);
    }
    (cpu_pct, m.rss_bytes)
}

/// Emette l'evento `ServiceMetrics` per un campione. Estratto da
/// `sample_process_metrics` (comportamento invariato).
fn emit_service_metrics(
    ctx: &CycleCtx<'_>,
    unit: &str,
    pid: u32,
    cpu_pct: Option<f64>,
    m: &crate::process_util::ProcessMetrics,
) {
    nexus_events::dispatcher::emit_global(
        ctx.project_id,
        ProjectEvent::ServiceMetrics {
            unit: unit.to_string(),
            pid: Some(pid as i32),
            cpu_pct: cpu_pct.unwrap_or(0.0) as f32,
            rss_bytes: m.rss_bytes,
            io_read_bytes: m.io_read_bytes,
            io_write_bytes: m.io_write_bytes,
            latency_ms: None,
        },
    );
}

/// Conta le righe-error NUOVE del ciclo e ritorna `(righe_error, testo_log)`.
///
/// Unix: finestra journalctl incrementale (`scan_new_logs`), `log_buffer` inutile.
/// Windows: buffer cumulativo di agent_processes; conta solo la coda oltre
/// `prev_log_len` (baseline al primo ciclo), replicando l'incrementalita' della
/// finestra journalctl. Estratto da `run_cycle` (comportamento invariato).
#[cfg_attr(unix, allow(unused_variables))]
async fn scan_error_rate(
    st: &mut UnitState,
    unit: &str,
    since: i64,
    log_buffer: &str,
) -> (u64, String) {
    #[cfg(unix)]
    {
        let _ = (st, log_buffer); // su Unix il buffer aggregato non serve
        scan_new_logs(unit, since).await
    }
    #[cfg(windows)]
    {
        let _ = (unit, since);
        scan_error_rate_windows(st, log_buffer)
    }
}

/// Ramo Windows di [`scan_error_rate`]: error-rate incrementale sul buffer
/// cumulativo di agent_processes. PRIMO CICLO (last_log_scan_ts==0): stabilisce
/// solo la BASELINE senza contare lo storico (fino a 50KB) che gonfierebbe
/// error_per_min con un'anomalia 'error_rate' spuria; dal secondo ciclo conta la
/// sola coda nuova. Char-boundary safe sull'offset in byte.
#[cfg(windows)]
fn scan_error_rate_windows(st: &mut UnitState, log_buffer: &str) -> (u64, String) {
    if st.last_log_scan_ts == 0 {
        st.prev_log_len = log_buffer.len();
        return (0u64, log_buffer.to_string());
    }
    // Se il buffer si e' accorciato (troncamento/nuovo avvio) resetta l'offset.
    let mut start = if log_buffer.len() >= st.prev_log_len {
        st.prev_log_len
    } else {
        0
    };
    // L'offset e' in byte del ciclo precedente: potrebbe cadere in mezzo a un
    // carattere multibyte del buffer corrente. Avanza al primo char boundary
    // valido per evitare un panic di slicing.
    while start < log_buffer.len() && !log_buffer.is_char_boundary(start) {
        start += 1;
    }
    let n = count_error_lines(&log_buffer[start..]);
    st.prev_log_len = log_buffer.len();
    (n, log_buffer.to_string())
}

/// Apre/aggiorna le anomalie di un sample e chiude quelle rientrate (cap 3).
///
/// Emette l'evento SSE solo sulla TRANSIZIONE (guardia in-memory), ma persiste a
/// ogni tick via UPSERT (mig 0491) e auto-risolve le anomalie non piu' attive.
/// Aggiorna `st.active_anomalies`. Estratto da `run_cycle` (comportamento
/// invariato).
async fn handle_anomalies(
    ctx: &CycleCtx<'_>,
    st: &mut UnitState,
    unit: &str,
    active: &str,
    signals: &[AnomalySignal],
) {
    let current: HashSet<String> = signals.iter().map(|s| s.metric.clone()).collect();
    for sig in signals {
        // L'evento SSE scatta SOLO sulla transizione healthy->anomaly (la guardia
        // in-memory evita lo spam di notifiche entro lo stesso processo). La
        // PERSISTENZA invece avviene a ogni tick: persist_diagnosis fa UPSERT
        // (mig 0491), quindi aggiorna la riga canonica (value/updated_at/
        // occurrences) senza creare duplicati. Cosi' dopo un restart di mcp-core,
        // dove `active_anomalies` si svuota, l'anomalia ancora attiva viene riusata
        // anziche' re-inserita.
        if !st.active_anomalies.contains(&sig.metric) {
            nexus_events::dispatcher::emit_global(
                ctx.project_id,
                ProjectEvent::ServiceAnomaly {
                    unit: unit.to_string(),
                    metric: sig.metric.clone(),
                    value: sig.value,
                    threshold: sig.threshold,
                    severity: sig.severity.clone(),
                },
            );
        }
        persist_diagnosis(
            &ctx.state.db,
            ctx.project_id,
            unit,
            "anomaly",
            Some(&sig.metric),
            Some(sig.value),
            Some(sig.threshold),
            None,
            Some(&format!("active={active}")),
        )
        .await;
    }
    st.active_anomalies = current;

    // Auto-resolve simmetrico all'apertura: chiude le diagnosi 'anomaly' aperte la
    // cui metrica non e' piu' attiva (anche le 'fantasma' storiche, perche' si basa
    // sul DB, non sullo stato in-memory). Senza questo le righe restavano 'open' a
    // vita nel pannello Problemi a servizio sano.
    let active_metrics: Vec<String> = st.active_anomalies.iter().cloned().collect();
    let resolved =
        resolve_stale_anomalies(&ctx.state.db, ctx.project_id, unit, &active_metrics).await;
    notify_problems_panel_refresh(ctx.project_id, resolved);
}

/// Ingredienti per la registrazione di un problema strutturale, gia' calcolati.
struct StructuralProblem {
    reason: &'static str,
    sig: String,
    /// Testo log ripulito da passare alla diagnosi LLM (con eventuale hint cause).
    clean_log: String,
    /// Detail iniziale mostrato nel pannello Problemi (sincrono, no LLM).
    initial_detail: String,
}

/// Costruisce log ripulito + detail per un problema strutturale rilevato. Punto
/// unico della formattazione (regola L): hint cause per port_not_listening,
/// strip ANSI, coda log. Estratto da `run_cycle` (comportamento invariato).
fn build_structural_problem(
    reason: &'static str,
    sig: String,
    run_log: &str,
    log_text: &str,
    ports: &[u16],
) -> StructuralProblem {
    // Per la diagnosi usa il log dell'INTERO run corrente (startup incluso, gia'
    // in `run_log` dal collector) anziche' la sola finestra error-rate: lo startup
    // contiene il segnale chiave (es. la porta reale in ascolto). Fallback alla
    // finestra se il run-log non e' disponibile.
    let source: &str = if run_log.trim().is_empty() {
        log_text
    } else {
        run_log
    };
    let mut clean = strip_ansi(source);
    // Gestione porte STRUTTURALE (no liste/regex): se il servizio e' su ma non
    // ascolta sulle porte ALLOCATE (fonte unica), passa il fatto alla diagnosi.
    // Hint sulle cause per port_not_listening (segnala, non prescrive): va SIA nel
    // log passato alla diagnosi LLM SIA nel detail del problema (che l'agente
    // error-fix riceve dal pannello).
    let cause_hint: Option<String> = port_not_listening_hint(reason, ports);
    if let Some(ref hint) = cause_hint {
        clean = format!("{hint}\n\nLog del servizio:\n{clean}");
    }
    let tail: Vec<&str> = clean.lines().rev().take(15).collect();
    let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    let tail: String = tail.chars().take(600).collect();
    let initial_detail = match &cause_hint {
        Some(hint) => {
            format!("Servizio non operativo ({reason}). {hint}\n\nUltime righe di log:\n{tail}")
        }
        None => {
            format!("Servizio non operativo ({reason}). Ultime righe di log:\n{tail}")
        }
    };
    StructuralProblem {
        reason,
        sig,
        clean_log: clean,
        initial_detail,
    }
}

/// Hint diagnostico per `port_not_listening` (None per gli altri reason o senza
/// porte). Estratto per tenere `build_structural_problem` coeso e sotto soglia.
fn port_not_listening_hint(reason: &str, ports: &[u16]) -> Option<String> {
    if reason != "port_not_listening" || ports.is_empty() {
        return None;
    }
    let plist = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "[Nexus] Il servizio risulta avviato ma non ascolta sulle porte \
         ALLOCATE attese ({plist}). Per la causa, distingui verificando lo \
         stato reale (quali processi sono in ascolto su quella porta e quali \
         processi di questo servizio sono in esecuzione): (a) il codice \
         ascolta su una porta HARDCODED diversa da quella allocata (dai log \
         vedi una porta diversa); (b) la porta allocata e' gia' occupata da \
         un'altra istanza dello STESSO servizio non terminata (processi \
         orfani -> EADDRINUSE), per cui il nuovo avvio non riesce ad \
         ascoltare. Verifica quale delle due prima di concludere."
    ))
}

/// Registra un problema strutturale (evento + persistenza) e lancia la diagnosi
/// LLM in background. Register-then-refine: il persist e' sincrono e non dipende
/// dall'LLM; la diagnosi raffina il detail dopo, senza bloccare il ciclo. Estratto
/// da `run_cycle` (comportamento invariato).
async fn register_structural_problem(ctx: &CycleCtx<'_>, unit: &str, problem: StructuralProblem) {
    let StructuralProblem {
        reason,
        sig,
        clean_log,
        initial_detail,
    } = problem;

    nexus_events::dispatcher::emit_global(
        ctx.project_id,
        ProjectEvent::ServiceCrashDetected {
            unit: unit.to_string(),
            error_kind: reason.to_string(),
            last_log: initial_detail.clone(),
        },
    );
    let diag_id = persist_diagnosis(
        &ctx.state.db,
        ctx.project_id,
        unit,
        "crash",
        Some(reason),
        None,
        None,
        Some(&sig),
        Some(&initial_detail),
    )
    .await;

    // Diagnosi LLM + auto-debug in BACKGROUND (non blocca il ciclo).
    crate::project_workspace::service_log_diagnose::spawn_diagnosis(
        ctx.state.clone(),
        ctx.project_id,
        unit.to_string(),
        clean_log,
        sig,
        diag_id,
        ctx.cfg.auto_diagnose_enabled,
        ctx.cfg.diagnose_cooldown_seconds,
        ctx.cfg.diagnose_max_per_hour,
    );
}

/// Gestisce il cambio di "run" (marcatore d'avvio) e ritorna se il grace di
/// readiness e' scaduto. Su nuovo run resetta grace + anti-spam e risolve i crash
/// dei run PRECEDENTI (obsoleti dopo un fix/riavvio), cosi' il pannello mostra solo
/// il problema del run corrente. Estratto da `detect_structural_failure`.
async fn apply_run_reset(
    ctx: &CycleCtx<'_>,
    st: &mut UnitState,
    unit: &str,
    active_enter: Option<String>,
) -> bool {
    if active_enter != st.prev_active_enter {
        st.prev_active_enter = active_enter;
        st.run_seen_at = Some(Instant::now());
        st.last_crash_sig = None;
        let resolved =
            resolve_open_crashes(&ctx.state.db, ctx.project_id, unit).await;
        notify_problems_panel_refresh(ctx.project_id, resolved);
    }
    st.run_seen_at
        .map(|t| t.elapsed().as_secs() as i64 >= ctx.cfg.readiness_grace_seconds)
        .unwrap_or(false)
}

/// Reason strutturale del servizio giu' (o None se sano). Readiness TCP: servizio
/// attivo da > grace con porte allocate ma NESSUNA in ascolto -> e' giu' (cattura i
/// supervisori vivi con l'app crashata, qualunque tecnologia). Servizio senza porte
/// (worker): readiness non applicabile -> solo failed/restart-loop. Estratto da
/// `detect_structural_failure`.
async fn structural_reason(
    ctx: &CycleCtx<'_>,
    active: &str,
    restart_delta: u32,
    grace_ok: bool,
    ports: &[u16],
) -> Option<&'static str> {
    let readiness_failed =
        readiness_fallita(grace_ok, active, ports, any_port_listening(ports).await);
    if active == "failed" {
        Some("service_failed")
    } else if readiness_failed {
        Some("port_not_listening")
    } else if restart_delta > ctx.cfg.restart_rate_max {
        Some("restart_loop")
    } else {
        None
    }
}

/// Detection STRUTTURALE di servizio non funzionante (cap 1): stato failed /
/// porta non in ascolto dopo l'avvio / restart-loop. Niente pattern testuali:
/// si rileva OGGETTIVAMENTE che il servizio e' giu', poi un LLM classifica i log.
/// Gestisce reset-run, grace readiness, probe TCP, anti-spam e auto-resolve.
/// Estratto da `run_cycle` (comportamento invariato).
async fn detect_structural_failure(
    ctx: &CycleCtx<'_>,
    st: &mut UnitState,
    unit: &str,
    active: &str,
    restart_delta: u32,
    active_enter: Option<String>,
    run_log: &str,
    log_text: &str,
) {
    let grace_ok = apply_run_reset(ctx, st, unit, active_enter).await;
    let ports = ports_for_unit(&ctx.state.db, ctx.project_id, unit).await;
    let reason = structural_reason(ctx, active, restart_delta, grace_ok, &ports).await;

    let Some(reason) = reason else {
        if grace_ok {
            // Servizio sano dopo il grace: chiude eventuali crash aperti (ciclo di
            // vita: quando viene riparato, il problema sparisce).
            let resolved =
                resolve_open_crashes(&ctx.state.db, ctx.project_id, unit).await;
            notify_problems_panel_refresh(ctx.project_id, resolved);
            st.last_crash_sig = None;
        }
        return;
    };

    // Firma per anti-spam: unit + run corrente + natura del problema.
    let sig = sig_hash(&format!(
        "{unit}:{}:{reason}",
        st.prev_active_enter.as_deref().unwrap_or("")
    ));
    if st.last_crash_sig.as_deref() == Some(sig.as_str()) {
        return;
    }
    st.last_crash_sig = Some(sig.clone());

    let problem = build_structural_problem(reason, sig, run_log, log_text, &ports);
    register_structural_problem(ctx, unit, problem).await;
}

/// Predicato PURO della readiness TCP: il servizio e' giu' SOLO se si e'
/// potuto osservare che nessuna delle porte attese e' in ascolto.
///
/// `ascolto == None` significa che il SO non ha risposto: da li' non si apre una
/// diagnosi di crash. Il difetto era `!any_port_listening(..)`, che faceva del
/// silenzio del SO una prova a carico — reason `port_not_listening`, diagnosi
/// aperta, remediation innescata, tutto su un servizio sano.
fn readiness_fallita(grace_ok: bool, active: &str, ports: &[u16], ascolto: Option<bool>) -> bool {
    grace_ok && active == "active" && !ports.is_empty() && ascolto == Some(false)
}

/// Almeno una delle porte attese e' in ascolto? `None` = non si e' potuto
/// chiedere (vedi [`ListenerScan`]). Estratto da `run_cycle` per early-return
/// leggibile.
///
/// Un solo interrogo del SO (`scan_listening_ports`, punto unico regola L) per
/// TUTTE le porte, invece di N connect mirati uno per porta: oltre a evitare
/// N scansioni ridondanti della tabella TCP, copre entrambe le famiglie di
/// indirizzo — un vecchio `tcp_probe` per porta (connect al solo
/// `127.0.0.1`) dichiarava mute le porte in LISTEN solo su `[::1]`.
///
/// [`ListenerScan`]: crate::project_workspace::port_recovery::ListenerScan
async fn any_port_listening(ports: &[u16]) -> Option<bool> {
    // Nessuna porta attesa: niente da osservare, e nessun motivo di interrogare
    // il SO. La risposta e' certa.
    if ports.is_empty() {
        return Some(false);
    }
    crate::project_workspace::port_recovery::scan_listening_ports()
        .await
        .qualcuno_ascolta(ports)
}

// ── Loop ──────────────────────────────────────────────────────────────────────

/// Avvia l'observer in background. Gating runtime via `agent.observer.enabled`.
///
/// Cross-platform: le sorgenti dati (enumerazione servizi, stato, metriche, log,
/// restart) sono astratte per OS in `collect_units` + i reader per-unit. Su Unix
/// usa systemd `--user` + `/proc` + journalctl; su Windows usa `agent_processes`
/// (kind='service') + le API Win32 (`process_util`). Il resto del ciclo
/// (anomaly/crash detection, persistenza `service_diagnoses`, auto-debug con
/// cooldown/cap/anti-loop) e' identico su entrambi gli OS.
pub fn spawn_service_observer(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        let mut states: HashMap<String, UnitState> = HashMap::new();
        loop {
            let cfg = load_config(&state.db).await;
            if !cfg.enabled {
                sleep(Duration::from_secs(cfg.interval_s)).await;
                continue;
            }
            run_cycle(&state, &cfg, &mut states).await;
            sleep(Duration::from_secs(cfg.interval_s)).await;
        }
    });
    tracing::info!("service_observer: avviato (config DB-driven, gating runtime)");
}

/// Delta di restart rispetto al ciclo precedente (cap 3). `restarts` e' None su
/// Windows (agent_processes non traccia i restart): il delta resta 0 -> l'anomaly
/// 'restart' e il reason 'restart_loop' sono di fatto disabilitati la', senza
/// codice extra. Aggiorna `st.prev_restarts` quando il dato e' disponibile.
fn compute_restart_delta(st: &mut UnitState, restarts: Option<u32>) -> u32 {
    let delta = match (st.prev_restarts, restarts) {
        (Some(prev), Some(cur)) => cur.saturating_sub(prev),
        _ => 0,
    };
    if restarts.is_some() {
        st.prev_restarts = restarts;
    }
    delta
}

/// Error-rate per minuto sulla finestra dall'ultimo scan + testo log per la
/// diagnosi (cap 3 + cap 1). Wrappa `scan_error_rate` con il calcolo della
/// finestra temporale e aggiorna `st.last_log_scan_ts`. Estratto da
/// `process_sample`.
async fn sample_error_rate(
    st: &mut UnitState,
    unit: &str,
    now_ts: i64,
    log_buffer: &str,
) -> (f64, String) {
    let since = if st.last_log_scan_ts > 0 {
        st.last_log_scan_ts
    } else {
        now_ts - 60
    };
    let window_min = ((now_ts - since).max(1) as f64) / 60.0;
    let (error_lines, log_text) = scan_error_rate(st, unit, since, log_buffer).await;
    st.last_log_scan_ts = now_ts;
    (error_lines as f64 / window_min, log_text)
}

/// Elabora un singolo sample (unit) di un progetto: metriche, restart-rate,
/// error-rate dai log, anomalie e detection strutturale. Orchestra gli helper di
/// fase; lo stato per-unit vive in `states`. Estratto da `run_cycle` per tenere
/// entrambe le funzioni coese e sotto le soglie di qualita' (comportamento
/// invariato).
async fn process_sample(
    ctx: &CycleCtx<'_>,
    states: &mut HashMap<String, UnitState>,
    now_ts: i64,
    sample: UnitSample,
) {
    let UnitSample {
        unit,
        active_state: active,
        pid,
        restarts,
        active_enter,
        run_log,
        log_buffer,
    } = sample;
    let key = format!("{}:{unit}", ctx.project_id);
    let st = states.entry(key).or_default();

    if st.prev_reported_active.as_deref() != Some(active.as_str()) {
        st.prev_reported_active = Some(active.clone());
        nexus_events::dispatcher::emit_global(
            ctx.project_id,
            nexus_events::event::ProjectEvent::ServiceStatusChanged {
                name: unit.clone(),
                status: active.clone(),
                port: None,
                pid: pid.map(|p| p as i32),
            },
        );
    }

    // ── Metriche (cap 4) ───────────────────────────────────────────────────
    let (cpu_pct, rss) = sample_process_metrics(ctx, st, &unit, pid);

    // ── Restart rate (cap 3) + error-rate dai log (cap 3 + cap 1) ──────────
    let restart_delta = compute_restart_delta(st, restarts);
    let (error_per_min, log_text) = sample_error_rate(st, &unit, now_ts, &log_buffer).await;

    // ── Anomalie: emetti solo sulla transizione + auto-resolve ─────────────
    let signals = evaluate_anomalies(&active, cpu_pct, rss, restart_delta, error_per_min, ctx.cfg);
    handle_anomalies(ctx, st, &unit, &active, &signals).await;

    // ── Detection STRUTTURALE di servizio non funzionante (cap 1) ──────────
    detect_structural_failure(
        ctx,
        st,
        &unit,
        &active,
        restart_delta,
        active_enter,
        &run_log,
        &log_text,
    )
    .await;
}

async fn run_cycle(
    state: &AppState,
    cfg: &ObserverConfig,
    states: &mut HashMap<String, UnitState>,
) {
    let now_ts = chrono::Utc::now().timestamp();
    let projects = projects_with_slug(&state.db).await;

    for (project_id, slug, name) in projects {
        let ctx = CycleCtx {
            state,
            cfg,
            project_id,
        };
        // Sorgenti dati aggregate per OS (Unix: systemd+/proc+journalctl; Windows:
        // agent_processes+Win32). Il ramo Unix usa `slug`, quello Windows deriva
        // lo slug di servizio da `name`. Il resto del ciclo e' identico.
        let services = collect_units(state, project_id, &slug, &name).await;
        let observed_units: Vec<String> = services.iter().map(|s| s.unit.clone()).collect();
        for sample in services {
            process_sample(&ctx, states, now_ts, sample).await;
        }

        // ── PRESA IN CARICO delle diagnosi aperte (regola L: il criterio e la
        // scrittura dell'esito vivono in `service_recovery`, qui c'e' solo il
        // battito che le interroga, coi parametri del trigger AI gia'
        // caricati sopra). Rilevare non ha mai riparato nulla: fino a qui
        // l'unica strada verso un rimedio era lo spawn dell'AI, interrogato
        // UNA volta al momento della rilevazione, e ogni rinvio lo consumava
        // per sempre — tre diagnosi aperte per sette ore con zero tentativi
        // (bacheca-attivita, 30-31/07/2026). Da qui la domanda si ripone a ogni
        // ciclo: si ritenta lo STESSO trigger (nessuna logica duplicata), e
        // solo una diagnosi rimasta bloccata a lungo senza che l'AI sia mai
        // partita ricade su un riavvio deterministico di ripiego.
        crate::project_workspace::service_recovery::process_open_service_crashes(
            state,
            project_id,
            cfg.auto_diagnose_enabled,
            cfg.diagnose_cooldown_seconds,
            cfg.diagnose_max_per_hour,
        )
        .await;

        // ── Sweep: anomalie e crash di unit non piu' osservati (rinominati/
        // rimossi). Le richiude qui perche' il resolve per-unit non le
        // raggiunge mai. Guard !is_empty(): non azzerare tutto su un errore
        // di systemctl.
        if !observed_units.is_empty() {
            let resolved = resolve_diagnoses_for_absent_units(
                &state.db,
                project_id,
                &observed_units,
            )
            .await;
            notify_problems_panel_refresh(project_id, resolved);
        }
    }

    // Sweep globale per staleness: chiude le anomalie fantasma il cui updated_at
    // e' troppo vecchio (servizio non piu' osservato per molti cicli: bus --user
    // cieco in WSL, unit rimosso, o non piu' anomalo). Non dipende da systemctl,
    // a differenza del sweep per-progetto qui sopra: usa solo il tempo.
    let stale_resolved =
        resolve_stale_anomalies_by_age(&state.db, cfg.anomaly_stale_resolve_seconds).await;
    notify_problems_panel_refresh_batch(&stale_resolved);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ObserverConfig {
        ObserverConfig {
            enabled: true,
            interval_s: 15,
            metrics_enabled: true,
            error_rate_max_per_min: 10.0,
            restart_rate_max: 3,
            cpu_pct_threshold: 90.0,
            rss_bytes_threshold: 1_000_000_000,
            auto_diagnose_enabled: false,
            diagnose_cooldown_seconds: 600,
            diagnose_max_per_hour: 5,
            readiness_grace_seconds: 12,
            anomaly_stale_resolve_seconds: 300,
        }
    }

    /// IL DIFETTO: una tabella dei listener non leggibile (`None`) apriva una
    /// diagnosi di crash `port_not_listening` e innescava la remediation su un
    /// servizio sano. Solo l'assenza OSSERVATA di listener e' un servizio giu'.
    #[test]
    fn senza_risposta_dal_so_il_servizio_non_e_dichiarato_giu() {
        assert!(
            !readiness_fallita(true, "active", &[24805], None),
            "il silenzio del SO non e' la prova che nessuno ascolti"
        );
        assert!(
            readiness_fallita(true, "active", &[24805], Some(false)),
            "nessun listener OSSERVATO: il servizio e' giu'"
        );
        assert!(!readiness_fallita(true, "active", &[24805], Some(true)));
    }

    /// Le altre condizioni restano necessarie: prima del grace, con un servizio
    /// non attivo o senza porte attese, non si parla di readiness.
    #[test]
    fn la_readiness_richiede_grace_servizio_attivo_e_porte() {
        assert!(!readiness_fallita(false, "active", &[24805], Some(false)));
        assert!(!readiness_fallita(
            true,
            "activating",
            &[24805],
            Some(false)
        ));
        assert!(!readiness_fallita(true, "active", &[], Some(false)));
    }

    #[test]
    fn no_anomaly_when_under_thresholds() {
        let s = evaluate_anomalies("active", Some(10.0), 100_000, 0, 1.0, &cfg());
        assert!(s.is_empty());
    }

    #[test]
    fn cpu_over_threshold_flags() {
        let s = evaluate_anomalies("active", Some(95.0), 0, 0, 0.0, &cfg());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric, "cpu");
    }

    #[test]
    fn restart_spike_is_critical() {
        let s = evaluate_anomalies("active", None, 0, 5, 0.0, &cfg());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric, "restart");
        assert_eq!(s[0].severity, "critical");
    }

    #[test]
    fn multiple_anomalies() {
        let s = evaluate_anomalies("active", Some(99.0), 2_000_000_000, 10, 50.0, &cfg());
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn failed_state_flags_down_critical() {
        // B2: un servizio in stato 'failed' e' segnalato come anomalia 'down'
        // critica anche senza altre metriche sopra soglia.
        let s = evaluate_anomalies("failed", None, 0, 0, 0.0, &cfg());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric, "down");
        assert_eq!(s[0].severity, "critical");
    }

    #[test]
    fn active_service_not_flagged_down() {
        let s = evaluate_anomalies("active", Some(10.0), 0, 0, 0.0, &cfg());
        assert!(s.iter().all(|x| x.metric != "down"));
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(
            strip_ansi("\u{1B}[31mrosso\u{1B}[0m normale"),
            "rosso normale"
        );
        assert_eq!(strip_ansi("nessun codice"), "nessun codice");
    }

    #[test]
    fn count_error_lines_criterio_condiviso() {
        // Il criterio error-rate (error/exception/panic/fatal, case-insensitive)
        // e' identico su Unix (journalctl) e Windows (buffer agent_processes).
        let log = "avvio ok\n\
                   ERROR: connessione rifiutata\n\
                   info: caricamento moduli\n\
                   Unhandled Exception at main\n\
                   PANIC: index out of bounds\n\
                   riga normale\n\
                   FATAL could not bind";
        assert_eq!(count_error_lines(log), 4);
        assert_eq!(count_error_lines("tutto tranquillo\nnessun problema"), 0);
        assert_eq!(count_error_lines(""), 0);
    }

    // Schema minimale di service_diagnoses + indice univoco parziale (mig 0491)
    // per testare la deduplica dell'UPSERT senza dipendere dalla suite migrazioni.
    async fn create_service_diagnoses(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE service_diagnoses ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 project_id UUID NOT NULL, \
                 unit TEXT NOT NULL, \
                 ts TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 signal_kind TEXT NOT NULL, \
                 metric TEXT, \
                 value DOUBLE PRECISION, \
                 threshold DOUBLE PRECISION, \
                 error_signature_hash TEXT, \
                 status TEXT NOT NULL DEFAULT 'open', \
                 detail TEXT, \
                 occurrences INTEGER NOT NULL DEFAULT 1, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 resolved_at TIMESTAMPTZ \
             )",
        )
        .execute(pool)
        .await
        .expect("create service_diagnoses");

        sqlx::query(
            "CREATE UNIQUE INDEX uniq_service_diagnoses_active_anomaly \
                 ON service_diagnoses (project_id, unit, COALESCE(metric, '')) \
                 WHERE signal_kind = 'anomaly' AND status IN ('open', 'diagnosing')",
        )
        .execute(pool)
        .await
        .expect("create unique partial index");

        // Indice crash (mig 0562): abilita l'UPSERT del ramo crash.
        sqlx::query(
            "CREATE UNIQUE INDEX uniq_service_diagnoses_active_crash \
                 ON service_diagnoses (project_id, unit, signal_kind, COALESCE(error_signature_hash, '')) \
                 WHERE signal_kind = 'crash' AND status IN ('open', 'diagnosing')",
        )
        .execute(pool)
        .await
        .expect("create crash unique partial index");
    }

    async fn count_open_anomalies(
        pool: &PgPool,
        project_id: Uuid,
        unit: &str,
        metric: &str,
    ) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM service_diagnoses \
             WHERE project_id=$1 AND unit=$2 AND metric=$3 \
               AND signal_kind='anomaly' AND status='open'",
        )
        .bind(project_id)
        .bind(unit)
        .bind(metric)
        .fetch_one(pool)
        .await
        .expect("count")
    }

    // Apre/aggiorna un'anomalia 'error_rate' col `value` dato (soglia fissa 10.0):
    // incapsula la chiamata ripetuta di persist_diagnosis nel test dell'UPSERT.
    async fn open_error_rate(pool: &PgPool, project_id: Uuid, unit: &str, value: f64) -> Uuid {
        persist_diagnosis(
            pool,
            project_id,
            unit,
            "anomaly",
            Some("error_rate"),
            Some(value),
            Some(10.0),
            None,
            Some("active=active"),
        )
        .await
        .expect("persist error_rate")
    }

    #[sqlx::test]
    async fn anomaly_upsert_non_duplica_e_incrementa_occurrences(pool: PgPool) {
        create_service_diagnoses(&pool).await;
        let project_id = Uuid::new_v4();
        let unit = "beauty-book-frontend.service";

        // Primo tick: apre l'anomalia. Tick successivi (simulano anche un restart
        // che svuota active_anomalies): AGGIORNANO la stessa riga, non ne inseriscono.
        let id1 = open_error_rate(&pool, project_id, unit, 60.0).await;
        let id2 = open_error_rate(&pool, project_id, unit, 177.0).await;
        let id3 = open_error_rate(&pool, project_id, unit, 833.0).await;

        assert_eq!(id1, id2, "stesso id: riga riusata, non duplicata");
        assert_eq!(id1, id3, "stesso id sul terzo tick");
        assert_eq!(
            count_open_anomalies(&pool, project_id, unit, "error_rate").await,
            1,
            "una sola anomalia open per (unit, metric)"
        );

        let (value, occ): (f64, i32) =
            sqlx::query_as("SELECT value, occurrences FROM service_diagnoses WHERE id=$1")
                .bind(id1)
                .fetch_one(&pool)
                .await
                .expect("riga canonica");
        assert_eq!(value, 833.0, "value aggiornato all'ultimo tick");
        assert_eq!(occ, 3, "occurrences incrementato a ogni tick");
    }

    #[sqlx::test]
    async fn anomaly_metrica_diversa_e_riga_separata(pool: PgPool) {
        create_service_diagnoses(&pool).await;
        let project_id = Uuid::new_v4();
        let unit = "beauty-book-frontend.service";

        // Metrica diversa sulla stessa unit -> riga distinta (chiave diversa).
        open_error_rate(&pool, project_id, unit, 60.0).await;
        persist_diagnosis(
            &pool,
            project_id,
            unit,
            "anomaly",
            Some("cpu"),
            Some(95.0),
            Some(90.0),
            None,
            Some("active=active"),
        )
        .await
        .expect("anomalia cpu distinta");
        assert_eq!(
            count_open_anomalies(&pool, project_id, unit, "cpu").await,
            1,
            "metrica diversa = anomalia separata"
        );
    }

    #[sqlx::test]
    async fn anomaly_riapre_dopo_resolve(pool: PgPool) {
        create_service_diagnoses(&pool).await;
        let project_id = Uuid::new_v4();
        let unit = "beauty-book-frontend.service";

        // Dopo resolve, un nuovo tick puo' riaprire (l'indice copre solo gli attivi).
        let id1 = open_error_rate(&pool, project_id, unit, 60.0).await;
        sqlx::query(
            "UPDATE service_diagnoses SET status='resolved', resolved_at=NOW() WHERE id=$1",
        )
        .bind(id1)
        .execute(&pool)
        .await
        .expect("resolve manuale");
        let id_riaperta = open_error_rate(&pool, project_id, unit, 500.0).await;
        assert_ne!(
            id_riaperta, id1,
            "nuova riga dopo resolve (storico preservato)"
        );
        assert_eq!(
            count_open_anomalies(&pool, project_id, unit, "error_rate").await,
            1,
            "sempre una sola anomalia open dopo riapertura"
        );
    }

    async fn count_open_crashes(pool: &PgPool, project_id: Uuid, unit: &str, sig: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM service_diagnoses \
             WHERE project_id=$1 AND unit=$2 AND error_signature_hash=$3 \
               AND signal_kind='crash' AND status='open'",
        )
        .bind(project_id)
        .bind(unit)
        .bind(sig)
        .fetch_one(pool)
        .await
        .expect("count")
    }

    // Apre/aggiorna un crash con la firma data (error_signature_hash): incapsula la
    // chiamata ripetuta di persist_diagnosis nel test dell'UPSERT crash.
    async fn open_crash(pool: &PgPool, project_id: Uuid, unit: &str, sig: &str) -> Uuid {
        persist_diagnosis(
            pool,
            project_id,
            unit,
            "crash",
            Some("service_failed"),
            None,
            None,
            Some(sig),
            Some("Servizio non operativo (service_failed)."),
        )
        .await
        .expect("persist crash")
    }

    #[sqlx::test]
    async fn crash_upsert_non_duplica_e_incrementa_occurrences(pool: PgPool) {
        create_service_diagnoses(&pool).await;
        let project_id = Uuid::new_v4();
        let unit = "beauty-book-frontend.service";

        // Primo ciclo: apre il crash. Cicli successivi (simulano un restart di
        // mcp-core che azzera st.last_crash_sig): AGGIORNANO la stessa riga, non ne
        // inseriscono. E' la causa radice dei crash duplicati nel pannello Problemi.
        let id1 = open_crash(&pool, project_id, unit, "sig-abc").await;
        let id2 = open_crash(&pool, project_id, unit, "sig-abc").await;

        assert_eq!(id1, id2, "stesso id: riga riusata, non duplicata");
        assert_eq!(
            count_open_crashes(&pool, project_id, unit, "sig-abc").await,
            1,
            "un solo crash open per (unit, firma)"
        );

        let occ: i32 = sqlx::query_scalar("SELECT occurrences FROM service_diagnoses WHERE id=$1")
            .bind(id1)
            .fetch_one(&pool)
            .await
            .expect("riga canonica");
        assert_eq!(occ, 2, "occurrences incrementato al secondo ciclo");
    }

    #[sqlx::test]
    async fn crash_firma_diversa_e_riga_separata(pool: PgPool) {
        create_service_diagnoses(&pool).await;
        let project_id = Uuid::new_v4();
        let unit = "beauty-book-frontend.service";

        // Firma errore diversa sulla stessa unit -> riga distinta (chiave diversa).
        open_crash(&pool, project_id, unit, "sig-uno").await;
        open_crash(&pool, project_id, unit, "sig-due").await;
        assert_eq!(
            count_open_crashes(&pool, project_id, unit, "sig-uno").await,
            1,
            "prima firma: una riga"
        );
        assert_eq!(
            count_open_crashes(&pool, project_id, unit, "sig-due").await,
            1,
            "firma diversa = crash separato"
        );
    }
}
