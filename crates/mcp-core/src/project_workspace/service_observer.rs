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
    /// Secondi di attesa dopo l'avvio prima del readiness check TCP (mig 0384).
    readiness_grace_seconds: i64,
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
    /// Firma dell'ultimo problema (crash/unhealthy) gestito (anti-ripetizione).
    last_crash_sig: Option<String>,
    /// Timestamp (unix) dell'ultima scansione log incrementale.
    last_log_scan_ts: i64,
    /// Valore di `ActiveEnterTimestamp` visto al ciclo precedente: se cambia, e'
    /// un nuovo avvio ("run") -> si resetta il grace di readiness e l'anti-spam.
    prev_active_enter: Option<String>,
    /// Istante in cui e' stato osservato l'avvio corrente (per il grace period
    /// readiness, in-memory: niente parsing del timestamp systemd).
    run_seen_at: Option<Instant>,
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
    let utime = fields
        .get(11)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let stime = fields
        .get(12)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

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
/// "failed", "inactive", "activating").
///
/// CAUSA RADICE (caso reale: backend morto per ts-node mancante): si enumera dai
/// FILE unit (`list-unit-files`), NON da `list-units`. Un servizio `Type=simple`
/// che muore con exit 0 viene SCARICATO (unloaded) da systemd e sparisce da
/// `list-units --all`, restando invisibile all'observer: non se ne scansionano
/// mai i log ne' lo stato. I file `{slug}-*.service` invece ci sono sempre; per
/// ciascuno interroghiamo lo stato corrente per nome con `is-active` (che
/// funziona anche su unit scaricati).
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

/// `ActiveEnterTimestamp` dell'unita' (stringa systemd, es. "Sun 2026-06-09
/// 19:25:05 CEST"). Confrontata col valore del ciclo precedente per rilevare un
/// nuovo avvio ("run"): NON la parsiamo, basta sapere se e' cambiata.
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
async fn ports_for_unit(db: &PgPool, project_id: Uuid, unit: &str) -> Vec<u16> {
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
    for p in unit_env_ports(unit).await {
        ports.insert(p);
    }
    ports.into_iter().collect()
}

/// Estrae le porte dalle variabili `Environment` del unit il cui NOME contiene
/// "PORT" (es. `PORT`, `PORT_BACKEND`). `systemctl show --property=Environment`
/// restituisce le coppie KEY=VALUE separate da spazio.
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

/// Chiude (resolved) le diagnosi 'crash' aperte di un'unita' tornata sana. Punto
/// unico del ciclo di vita dei crash strutturali: quando il servizio e' di nuovo
/// healthy (porta in ascolto / non failed) il problema sparisce dal pannello,
/// simmetrico a `resolve_stale_anomalies` per le anomalie.
async fn resolve_open_crashes(db: &PgPool, project_id: Uuid, unit: &str) {
    let _ = sqlx::query(
        "UPDATE service_diagnoses SET status = 'resolved', resolved_at = NOW() \
         WHERE project_id = $1 AND unit = $2 AND signal_kind = 'crash' \
           AND status IN ('open', 'diagnosing')",
    )
    .bind(project_id)
    .bind(unit)
    .execute(db)
    .await;
}

/// Scansione incrementale dei log dell'unita' dal timestamp `since_unix`.
/// Ritorna (n_righe_error, testo_completo). Usa journalctl one-shot (no follow).
/// Il testo serve alla diagnosi LLM (service_log_diagnose) quando la detection
/// strutturale segnala un servizio non funzionante: niente piu' pattern fissi.
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
    (error_lines, text)
}

/// Log dell'INTERO run corrente dell'unita' (dal suo avvio), filtrati per
/// `InvocationID` di systemd. Cattura sempre lo startup completo a prescindere
/// da quanti minuti fa il servizio e' partito, ed e' indispensabile per la
/// diagnosi di `port_not_listening`: la riga chiave ("listening on <porta>")
/// e' nello startup, non negli ultimi secondi della finestra error-rate.
/// L'InvocationID e' l'identificatore stabile del run corrente: niente parsing
/// di timestamp/fuso orario. Stringa vuota -> il chiamante usa il suo fallback.
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
        out.push(AnomalySignal {
            metric: "down".into(),
            value: 1.0,
            threshold: 0.0,
            severity: "critical".into(),
        });
    }
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

async fn run_cycle(
    state: &AppState,
    cfg: &ObserverConfig,
    states: &mut HashMap<String, UnitState>,
) {
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
            let (error_lines, log_text) = scan_new_logs(&unit, since).await;
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
                evaluate_anomalies(&active, cpu_pct, rss, restart_delta, error_per_min, cfg);
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
            let active_metrics: Vec<String> = st.active_anomalies.iter().cloned().collect();
            resolve_stale_anomalies(&state.db, project_id, &unit, &active_metrics).await;

            // ── Detection STRUTTURALE di servizio non funzionante (cap 1) ───
            // Niente piu' pattern testuali per-linguaggio: si rileva in modo
            // OGGETTIVO che il servizio e' giu' (stato failed / porta non in
            // ascolto dopo l'avvio / restart-loop), poi un LLM classifica i log
            // (cross-tecnologia). Vedi service_log_diagnose + mig 0384.

            // Nuovo "run"? (ActiveEnterTimestamp cambiato) -> reset grace + anti-spam.
            let active_enter = unit_active_enter(&unit).await;
            if active_enter != st.prev_active_enter {
                st.prev_active_enter = active_enter;
                st.run_seen_at = Some(Instant::now());
                st.last_crash_sig = None;
                // Nuovo avvio del servizio (es. il debugger ha applicato un fix e
                // l'auto-remediation ha riavviato): i crash dei run PRECEDENTI sono
                // obsoleti -> risolvili, cosi' il pannello mostra solo il problema
                // del run corrente. Se il nuovo run e' ancora unhealthy, la
                // detection sotto crea un nuovo crash aggiornato.
                resolve_open_crashes(&state.db, project_id, &unit).await;
            }
            let grace_ok = st
                .run_seen_at
                .map(|t| t.elapsed().as_secs() as i64 >= cfg.readiness_grace_seconds)
                .unwrap_or(false);

            // Readiness TCP: servizio attivo da > grace con porte allocate ma
            // NESSUNA in ascolto -> e' giu' (cattura i supervisori che restano
            // vivi con l'app crashata, qualunque tecnologia). Servizio senza porte
            // (worker): readiness non applicabile -> solo failed/restart-loop.
            let ports = ports_for_unit(&state.db, project_id, &unit).await;
            let mut readiness_failed = false;
            if grace_ok && active == "active" && !ports.is_empty() {
                let mut any_up = false;
                for p in &ports {
                    if crate::project_workspace::port_recovery::tcp_probe(*p, 400).await {
                        any_up = true;
                        break;
                    }
                }
                readiness_failed = !any_up;
            }

            let reason: Option<&str> = if active == "failed" {
                Some("service_failed")
            } else if readiness_failed {
                Some("port_not_listening")
            } else if restart_delta > cfg.restart_rate_max {
                Some("restart_loop")
            } else {
                None
            };

            if let Some(reason) = reason {
                // Firma per anti-spam: unit + run corrente + natura del problema.
                let sig = sig_hash(&format!(
                    "{unit}:{}:{reason}",
                    st.prev_active_enter.as_deref().unwrap_or("")
                ));
                if st.last_crash_sig.as_deref() != Some(sig.as_str()) {
                    st.last_crash_sig = Some(sig.clone());

                    // Register-then-refine: registra SUBITO il problema (sincrono,
                    // veloce, col log grezzo) cosi' compare in Problemi e il persist
                    // NON dipende dall'LLM; poi la diagnosi LLM raffina il detail in
                    // BACKGROUND (con timeout), senza bloccare il ciclo observer.
                    // Per la diagnosi usa il log dell'INTERO run corrente (startup
                    // incluso) anziche' la sola finestra error-rate: lo startup
                    // contiene il segnale chiave (es. la porta reale in ascolto).
                    // Fallback alla finestra se l'InvocationID non e' disponibile.
                    let run_log = scan_run_logs(&unit).await;
                    let source: &str = if run_log.trim().is_empty() {
                        &log_text
                    } else {
                        &run_log
                    };
                    let mut clean = strip_ansi(source);
                    // Gestione porte STRUTTURALE (no liste/regex): se il servizio e'
                    // su ma non ascolta sulle porte ALLOCATE (fonte unica), passa il
                    // fatto alla diagnosi. L'LLM, leggendo i log, rileva se ascolta
                    // su una porta hardcoded diversa e suggerisce process.env.PORT.
                    if reason == "port_not_listening" && !ports.is_empty() {
                        let plist = ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(", ");
                        clean = format!(
                            "[Nexus] Il servizio risulta avviato ma non ascolta sulle porte ALLOCATE \
                             attese ({plist}). Se dai log ascolta su una porta diversa, la causa \
                             probabile e' una porta HARDCODED nel codice invece della porta allocata: \
                             il fix corretto e' leggere la porta da process.env.PORT (porta allocata da \
                             Nexus), non un valore fisso.\n\nLog del servizio:\n{clean}"
                        );
                    }
                    let tail: Vec<&str> = clean.lines().rev().take(15).collect();
                    let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                    let tail: String = tail.chars().take(600).collect();
                    let initial_detail =
                        format!("Servizio non operativo ({reason}). Ultime righe di log:\n{tail}");

                    nexus_events::dispatcher::emit_global(
                        project_id,
                        ProjectEvent::ServiceCrashDetected {
                            unit: unit.clone(),
                            error_kind: reason.to_string(),
                            last_log: initial_detail.clone(),
                        },
                    );
                    let diag_id = persist_diagnosis(
                        &state.db,
                        project_id,
                        &unit,
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
                        state.clone(),
                        project_id,
                        unit.clone(),
                        clean,
                        sig,
                        diag_id,
                        cfg.auto_diagnose_enabled,
                        cfg.diagnose_cooldown_seconds,
                        cfg.diagnose_max_per_hour,
                    );
                }
            } else if grace_ok {
                // Servizio sano dopo il grace: chiude eventuali crash aperti
                // (ciclo di vita: quando viene riparato, il problema sparisce).
                resolve_open_crashes(&state.db, project_id, &unit).await;
                st.last_crash_sig = None;
            }
        }

        // ── Sweep: anomalie di unit non piu' osservati (rinominati/rimossi) ─
        // Le richiude qui perche' il resolve per-unit non le raggiunge mai.
        // Guard !is_empty(): non azzerare tutto su un errore di systemctl.
        if !observed_units.is_empty() {
            resolve_anomalies_for_absent_units(&state.db, project_id, &observed_units).await;
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
            readiness_grace_seconds: 12,
        }
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
}
