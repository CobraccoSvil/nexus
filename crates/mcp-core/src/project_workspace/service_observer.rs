//! Service Observer — osservabilita' runtime delle APP UTENTE (mig 0355/0356).
//!
//! Worker periodico, fratello di `services_watchdog` ma con scope DISGIUNTO:
//! qui si monitorano i servizi systemd `{slug}-*.service` dei progetti utente,
//! NON i microservizi Nexus (che restano al watchdog). L'observer NON riavvia:
//! osserva e diagnostica. Copre in un solo ciclo:
//!   - cap 4: metriche OS per processo (/proc) -> evento ServiceMetrics
//!   - cap 3: anomaly detection (cpu/rss/restart/error-rate) -> ServiceAnomaly
//!   - cap 1: crash detection nei log -> ServiceCrashDetected (+ auto-debug,
//!            vedi `remediation`, gated da agent.observer.auto_diagnose_enabled)
//!
//! Regole: G (tutte le soglie in settings, lette a ogni ciclo), E (solo unit
//! `{slug}-` e PID del progetto, validati via /proc/<pid>/comm), L (riusa
//! tcp/`/proc` readers e build_diagnostics; un solo loop per 3 capacita').
//!
//! Anti-spam: anomalie e crash sono emessi sulla TRANSIZIONE (quando compaiono),
//! non a ogni ciclo finche' persistono (stato in-memory per unit).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use nexus_events::event::ProjectEvent;
use sqlx::PgPool;
use tokio::time::sleep;
use uuid::Uuid;

use crate::AppState;

/// Dimensione pagina (Linux x86_64) per convertire le pagine RSS in byte.
const PAGE_SIZE_BYTES: u64 = 4096;
/// USER_HZ standard Linux: i tick di /proc/<pid>/stat sono 1/100 di secondo.
const USER_HZ: f64 = 100.0;
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
}

/// Stato runtime per unit, tra i cicli.
#[derive(Debug, Default, Clone)]
struct UnitState {
    /// utime+stime (tick) del campione precedente, per il delta CPU.
    prev_cpu_ticks: Option<u64>,
    /// Istante del campione precedente (per il denominatore CPU%).
    prev_sample: Option<Instant>,
    /// NRestarts visto al ciclo precedente.
    prev_restarts: Option<u32>,
    /// Anomalie attualmente attive (metric), per emettere solo sulla transizione.
    active_anomalies: HashSet<String>,
    /// Firma dell'ultimo crash gestito (anti-ripetizione).
    last_crash_sig: Option<String>,
    /// Timestamp (unix) dell'ultima scansione log incrementale.
    last_log_scan_ts: i64,
}

/// Campione metriche grezze da /proc.
struct ProcSample {
    cpu_ticks: u64,
    rss_bytes: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
}

// ── Config ───────────────────────────────────────────────────────────────────

async fn load_config(db: &PgPool) -> ObserverConfig {
    async fn s(db: &PgPool, k: &str) -> Option<String> {
        crate::settings::get_setting(db, k).await.ok().flatten()
    }
    fn b(v: Option<String>, def: bool) -> bool {
        match v {
            Some(x) => !matches!(x.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"),
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
        rss_bytes_threshold: i(s(db, "agent.observer.rss_bytes_threshold").await, 1_073_741_824)
            .max(0) as u64,
        auto_diagnose_enabled: b(s(db, "agent.observer.auto_diagnose_enabled").await, false),
        diagnose_cooldown_seconds: i(s(db, "agent.observer.diagnose_cooldown_seconds").await, 600),
        diagnose_max_per_hour: i(s(db, "agent.observer.diagnose_max_per_hour").await, 5),
    }
}

// ── Lettura /proc (riusa il pattern di port_recovery) ─────────────────────────

/// Legge `comm` del processo (per validare PID non riciclato, regola E).
fn read_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Estrae metriche da /proc/<pid>/{stat,statm,io}. None se il PID non esiste.
fn read_proc_metrics(pid: u32) -> Option<ProcSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Il campo comm (2o) e' tra parentesi e puo' contenere spazi: si parte dopo ')'.
    let after = stat.rsplit_once(')').map(|(_, b)| b).unwrap_or(&stat);
    let fields: Vec<&str> = after.split_whitespace().collect();
    // Dopo ')' i campi ripartono da "state" (campo 3). utime=campo14 -> idx 11,
    // stime=campo15 -> idx 12.
    let utime = fields.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let stime = fields.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

    let rss_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| pages * PAGE_SIZE_BYTES)
        .unwrap_or(0);

    let (mut io_read, mut io_write) = (0u64, 0u64);
    if let Ok(io) = std::fs::read_to_string(format!("/proc/{pid}/io")) {
        for line in io.lines() {
            if let Some(v) = line.strip_prefix("read_bytes:") {
                io_read = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("write_bytes:") {
                io_write = v.trim().parse().unwrap_or(0);
            }
        }
    }

    Some(ProcSample {
        cpu_ticks: utime + stime,
        rss_bytes,
        io_read_bytes: io_read,
        io_write_bytes: io_write,
    })
}

// ── Enumerazione servizi utente (scope progetto, regola E) ────────────────────

/// Progetti con slog non vuoto (al piu' MAX_PROJECTS_PER_CYCLE).
async fn projects_with_slug(db: &PgPool) -> Vec<(Uuid, String)> {
    sqlx::query_as::<_, (Uuid, Option<String>)>(
        "SELECT id, slug FROM projects WHERE slug IS NOT NULL AND slug <> '' LIMIT $1",
    )
    .bind(MAX_PROJECTS_PER_CYCLE as i64)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(id, slug)| slug.map(|s| (id, s)))
    .collect()
}

/// Unit systemd `{slug}-*.service` con il loro stato active (es. "active",
/// "failed", "activating").
async fn list_user_services(slug: &str) -> Vec<(String, String)> {
    let out = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--type=service",
            "--all",
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
        let cols: Vec<&str> = line.split_whitespace().collect();
        // Formato: UNIT LOAD ACTIVE SUB DESCRIPTION...
        if let Some(unit) = cols.first() {
            if unit.ends_with(".service") && unit.starts_with(&prefix) {
                let active = cols.get(2).copied().unwrap_or("unknown").to_string();
                units.push((unit.to_string(), active));
            }
        }
    }
    units
}

/// MainPID di un'unita' systemd --user (0/None se non attiva).
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

/// Scansione incrementale dei log dell'unita' dal timestamp `since_unix`.
/// Ritorna (n_righe_error, eventuale_crash). Usa journalctl one-shot (no follow):
/// si integra nel loop unico evitando di gestire processi `-f` per servizio.
async fn scan_new_logs(unit: &str, since_unix: i64) -> (u64, Option<(String, String)>) {
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
        Err(_) => return (0, None),
    };
    let mut error_lines = 0u64;
    for line in text.lines() {
        let low = line.to_lowercase();
        if low.contains("error") || low.contains("exception") || low.contains("panic")
            || low.contains("fatal")
        {
            error_lines += 1;
        }
    }
    (error_lines, detect_crash(&text))
}

// ── Detector (logica pura, testabile) ─────────────────────────────────────────

/// Rileva un crash/eccezione runtime in un blocco di log. Ritorna (kind, riga).
fn detect_crash(text: &str) -> Option<(String, String)> {
    for (pattern, kind) in [
        ("panicked at", "rust_panic"),
        ("Traceback (most recent call last)", "python_traceback"),
        ("Unhandled exception", "dotnet_exception"),
        ("UnhandledPromiseRejection", "node_unhandled_rejection"),
        ("segmentation fault", "segfault"),
        ("Segmentation fault", "segfault"),
        ("FATAL ERROR", "fatal"),
    ] {
        if let Some(line) = text.lines().find(|l| l.contains(pattern)) {
            let trimmed = line.trim();
            let snippet: String = trimmed.chars().take(400).collect();
            return Some((kind.to_string(), snippet));
        }
    }
    None
}

/// Anomalia rilevata dal detector.
#[derive(Debug, Clone, PartialEq)]
struct AnomalySignal {
    metric: String,
    value: f64,
    threshold: f64,
    severity: String,
}

/// Valuta le metriche correnti contro le soglie. Funzione pura.
fn evaluate_anomalies(
    cpu_pct: Option<f64>,
    rss_bytes: u64,
    restart_delta: u32,
    error_per_min: f64,
    cfg: &ObserverConfig,
) -> Vec<AnomalySignal> {
    let mut out = Vec::new();
    if let Some(cpu) = cpu_pct {
        if cpu > cfg.cpu_pct_threshold {
            out.push(AnomalySignal {
                metric: "cpu".into(),
                value: cpu,
                threshold: cfg.cpu_pct_threshold,
                severity: "warning".into(),
            });
        }
    }
    if cfg.rss_bytes_threshold > 0 && rss_bytes > cfg.rss_bytes_threshold {
        out.push(AnomalySignal {
            metric: "rss".into(),
            value: rss_bytes as f64,
            threshold: cfg.rss_bytes_threshold as f64,
            severity: "warning".into(),
        });
    }
    if restart_delta > cfg.restart_rate_max {
        out.push(AnomalySignal {
            metric: "restart".into(),
            value: restart_delta as f64,
            threshold: cfg.restart_rate_max as f64,
            severity: "critical".into(),
        });
    }
    if error_per_min > cfg.error_rate_max_per_min {
        out.push(AnomalySignal {
            metric: "error_rate".into(),
            value: error_per_min,
            threshold: cfg.error_rate_max_per_min,
            severity: "warning".into(),
        });
    }
    out
}

// ── Persistenza diagnosi (service_diagnoses) ──────────────────────────────────

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
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO service_diagnoses
           (project_id, unit, signal_kind, metric, value, threshold,
            error_signature_hash, status, detail)
           VALUES ($1,$2,$3,$4,$5,$6,$7,'open',$8)
           RETURNING id"#,
    )
    .bind(project_id)
    .bind(unit)
    .bind(signal_kind)
    .bind(metric)
    .bind(value)
    .bind(threshold)
    .bind(error_signature_hash)
    .bind(detail)
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
) {
    let _ = sqlx::query(
        r#"UPDATE service_diagnoses
           SET status = 'resolved', resolved_at = NOW()
           WHERE project_id = $1
             AND unit = $2
             AND signal_kind = 'anomaly'
             AND status IN ('open', 'diagnosing')
             AND metric <> ALL($3)"#,
    )
    .bind(project_id)
    .bind(unit)
    .bind(active_metrics)
    .execute(db)
    .await;
}

/// Chiude le anomalie aperte di un progetto le cui `unit` NON sono piu' tra i
/// servizi osservati (rinominate/rimosse: lo unit file non esiste piu', quindi
/// `list_user_services` non le elenca nemmeno con `--all`). Queste righe non
/// verrebbero MAI richiuse dal resolve per-unit, perche' `run_cycle` non visita
/// piu' quegli unit: restano 'open' a vita nel pannello Problemi (i veri
/// "fantasma"). signal_kind='anomaly' soltanto; i crash li governa il Debugger.
///
/// Il chiamante DEVE garantire `observed_units` non vuoto: con lista vuota
/// `unit <> ALL('{}')` matcherebbe tutto e azzererebbe ogni anomalia del
/// progetto su un errore transitorio di systemctl.
async fn resolve_anomalies_for_absent_units(
    db: &PgPool,
    project_id: Uuid,
    observed_units: &[String],
) {
    let _ = sqlx::query(
        r#"UPDATE service_diagnoses
           SET status = 'resolved', resolved_at = NOW()
           WHERE project_id = $1
             AND signal_kind = 'anomaly'
             AND status IN ('open', 'diagnosing')
             AND unit <> ALL($2)"#,
    )
    .bind(project_id)
    .bind(observed_units)
    .execute(db)
    .await;
}

fn sig_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

// ── Loop ──────────────────────────────────────────────────────────────────────

/// Avvia l'observer in background. Gating runtime via `agent.observer.enabled`.
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

async fn run_cycle(state: &AppState, cfg: &ObserverConfig, states: &mut HashMap<String, UnitState>) {
    let now_ts = chrono::Utc::now().timestamp();
    let projects = projects_with_slug(&state.db).await;

    for (project_id, slug) in projects {
        let services = list_user_services(&slug).await;
        let observed_units: Vec<String> = services.iter().map(|(u, _)| u.clone()).collect();
        for (unit, active) in services {
            let key = format!("{project_id}:{unit}");
            let st = states.entry(key).or_default();

            // ── Metriche (cap 4) ───────────────────────────────────────────
            let pid = unit_main_pid(&unit).await;
            let mut cpu_pct: Option<f64> = None;
            if let Some(pid) = pid {
                // Validazione anti PID riciclato (regola E): il comm deve esistere.
                if read_comm(pid).is_some() {
                    if let Some(sample) = read_proc_metrics(pid) {
                        let now_inst = Instant::now();
                        if let (Some(prev_ticks), Some(prev_inst)) =
                            (st.prev_cpu_ticks, st.prev_sample)
                        {
                            let dt = now_inst.duration_since(prev_inst).as_secs_f64();
                            if dt > 0.0 {
                                let dticks = sample.cpu_ticks.saturating_sub(prev_ticks) as f64;
                                cpu_pct = Some((dticks / USER_HZ / dt) * 100.0);
                            }
                        }
                        st.prev_cpu_ticks = Some(sample.cpu_ticks);
                        st.prev_sample = Some(now_inst);

                        if cfg.metrics_enabled {
                            nexus_events::dispatcher::emit_global(
                                project_id,
                                ProjectEvent::ServiceMetrics {
                                    unit: unit.clone(),
                                    pid: Some(pid as i32),
                                    cpu_pct: cpu_pct.unwrap_or(0.0) as f32,
                                    rss_bytes: sample.rss_bytes,
                                    io_read_bytes: sample.io_read_bytes,
                                    io_write_bytes: sample.io_write_bytes,
                                    latency_ms: None,
                                },
                            );
                        }
                    }
                }
            } else {
                // Servizio non attivo: azzera i campioni CPU.
                st.prev_cpu_ticks = None;
                st.prev_sample = None;
            }

            // ── Restart rate (cap 3) ───────────────────────────────────────
            let restarts = unit_restarts(&unit).await;
            let restart_delta = match (st.prev_restarts, restarts) {
                (Some(prev), Some(cur)) => cur.saturating_sub(prev),
                _ => 0,
            };
            if restarts.is_some() {
                st.prev_restarts = restarts;
            }

            // ── Log scan: error-rate + crash (cap 3 + cap 1) ───────────────
            let since = if st.last_log_scan_ts > 0 {
                st.last_log_scan_ts
            } else {
                now_ts - 60
            };
            let window_min = ((now_ts - since).max(1) as f64) / 60.0;
            let (error_lines, crash) = scan_new_logs(&unit, since).await;
            st.last_log_scan_ts = now_ts;
            let error_per_min = error_lines as f64 / window_min;

            // rss per detector
            let rss = read_comm(pid.unwrap_or(0))
                .and(pid)
                .and_then(read_proc_metrics)
                .map(|s| s.rss_bytes)
                .unwrap_or(0);

            // ── Anomalie: emetti solo sulla transizione ────────────────────
            let signals =
                evaluate_anomalies(cpu_pct, rss, restart_delta, error_per_min, cfg);
            let current: HashSet<String> = signals.iter().map(|s| s.metric.clone()).collect();
            for sig in &signals {
                if !st.active_anomalies.contains(&sig.metric) {
                    nexus_events::dispatcher::emit_global(
                        project_id,
                        ProjectEvent::ServiceAnomaly {
                            unit: unit.clone(),
                            metric: sig.metric.clone(),
                            value: sig.value,
                            threshold: sig.threshold,
                            severity: sig.severity.clone(),
                        },
                    );
                    persist_diagnosis(
                        &state.db,
                        project_id,
                        &unit,
                        "anomaly",
                        Some(&sig.metric),
                        Some(sig.value),
                        Some(sig.threshold),
                        None,
                        Some(&format!("active={active}")),
                    )
                    .await;
                }
            }
            st.active_anomalies = current;

            // ── Auto-resolve: anomalie rientrate sotto soglia ──────────────
            // Simmetrico all'apertura sopra: chiude le diagnosi 'anomaly' aperte
            // la cui metrica non e' piu' attiva (anche le 'fantasma' storiche,
            // perche' si basa sul DB, non sullo stato in-memory). Senza questo le
            // righe restavano 'open' a vita nel pannello Problemi a servizio sano.
            let active_metrics: Vec<String> =
                st.active_anomalies.iter().cloned().collect();
            resolve_stale_anomalies(&state.db, project_id, &unit, &active_metrics).await;

            // ── Crash detection (cap 1): emetti su firma nuova ─────────────
            if let Some((kind, last_log)) = crash {
                let sig = sig_hash(&format!("{unit}:{last_log}"));
                if st.last_crash_sig.as_deref() != Some(sig.as_str()) {
                    st.last_crash_sig = Some(sig.clone());
                    nexus_events::dispatcher::emit_global(
                        project_id,
                        ProjectEvent::ServiceCrashDetected {
                            unit: unit.clone(),
                            error_kind: kind.clone(),
                            last_log: last_log.clone(),
                        },
                    );
                    let diag_id = persist_diagnosis(
                        &state.db,
                        project_id,
                        &unit,
                        "crash",
                        None,
                        None,
                        None,
                        Some(&sig),
                        Some(&format!("{kind}: {last_log}")),
                    )
                    .await;
                    // cap 1 — auto-debug (gated da auto_diagnose_enabled).
                    crate::project_workspace::service_observer_remediation::maybe_trigger_debugger(
                        state,
                        cfg.auto_diagnose_enabled,
                        cfg.diagnose_cooldown_seconds,
                        cfg.diagnose_max_per_hour,
                        project_id,
                        &unit,
                        &kind,
                        &last_log,
                        &sig,
                        diag_id,
                    )
                    .await;
                }
            }
        }

        // ── Sweep: anomalie di unit non piu' osservati (rinominati/rimossi) ─
        // Le richiude qui perche' il resolve per-unit non le raggiunge mai.
        // Guard !is_empty(): non azzerare tutto su un errore di systemctl.
        if !observed_units.is_empty() {
            resolve_anomalies_for_absent_units(
                &state.db,
                project_id,
                &observed_units,
            )
            .await;
        }
    }
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
        }
    }

    #[test]
    fn no_anomaly_when_under_thresholds() {
        let s = evaluate_anomalies(Some(10.0), 100_000, 0, 1.0, &cfg());
        assert!(s.is_empty());
    }

    #[test]
    fn cpu_over_threshold_flags() {
        let s = evaluate_anomalies(Some(95.0), 0, 0, 0.0, &cfg());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric, "cpu");
    }

    #[test]
    fn restart_spike_is_critical() {
        let s = evaluate_anomalies(None, 0, 5, 0.0, &cfg());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric, "restart");
        assert_eq!(s[0].severity, "critical");
    }

    #[test]
    fn multiple_anomalies() {
        let s = evaluate_anomalies(Some(99.0), 2_000_000_000, 10, 50.0, &cfg());
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn detect_rust_panic() {
        let log = "thread 'main' panicked at src/main.rs:10:5:\nindex out of bounds";
        let c = detect_crash(log);
        assert!(c.is_some());
        assert_eq!(c.unwrap().0, "rust_panic");
    }

    #[test]
    fn detect_python_traceback() {
        let log = "Traceback (most recent call last):\n  File x";
        assert_eq!(detect_crash(log).unwrap().0, "python_traceback");
    }

    #[test]
    fn no_crash_on_clean_log() {
        assert!(detect_crash("server started on :3000\nrequest handled").is_none());
    }
}
