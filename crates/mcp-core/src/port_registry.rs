//! Registro centralizzato delle porte TCP allocate ai progetti.
//!
//! Pattern: cache DB con refresh background 60s (come `RoutingMatrixCache`).
//! La differenza principale: NON panica se la tabella e' vuota (nessun
//! progetto potrebbe avere allocazioni al primo avvio).
//!
//! Ogni porta allocata e' unica a livello DB (vincolo UNIQUE su `port`).
//! Il registro viene consultato da `find_free_port()` per evitare conflitti
//! e dagli endpoint API per assegnazioni manuali.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Singola allocazione porta.
#[derive(Debug, Clone)]
pub struct PortAllocation {
    pub id: Uuid,
    pub project_id: Uuid,
    pub port: u16,
    pub label: String,
    pub allocation_mode: String,
    pub run_config_id: Option<Uuid>,
    pub service_unit: Option<String>,
}

/// Snapshot immutabile di tutte le allocazioni porte.
#[derive(Debug, Clone)]
pub struct PortRegistry {
    /// port -> allocazione
    pub by_port: HashMap<u16, PortAllocation>,
    /// project_id -> lista porte
    pub by_project: HashMap<Uuid, Vec<u16>>,
    /// Timestamp di caricamento (per debug/UI)
    pub loaded_at: Instant,
}

impl PortRegistry {
    /// Controlla se una porta e' disponibile (non allocata).
    pub fn is_port_available(&self, port: u16) -> bool {
        !self.by_port.contains_key(&port)
    }

    /// Porte allocate a un progetto specifico.
    pub fn ports_for_project(&self, project_id: &Uuid) -> Vec<PortAllocation> {
        self.by_project
            .get(project_id)
            .map(|ports| {
                ports
                    .iter()
                    .filter_map(|p| self.by_port.get(p).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Insieme di tutte le porte allocate (per `find_free_port`).
    pub fn all_allocated_ports(&self) -> Vec<u16> {
        self.by_port.keys().copied().collect()
    }
}

/// Carica il registro dal DB.
async fn fetch_from_db(db: &PgPool) -> Result<PortRegistry, String> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, i32, String, String, Option<Uuid>, Option<String>)>(
        r#"SELECT id, project_id, port, label, allocation_mode, run_config_id, service_unit
           FROM nexus_port_allocations
           ORDER BY project_id, port"#,
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("query nexus_port_allocations fallita: {e}"))?;

    let mut by_port = HashMap::new();
    let mut by_project: HashMap<Uuid, Vec<u16>> = HashMap::new();

    for (id, project_id, port_i32, label, mode, rc_id, svc_unit) in rows {
        let port = port_i32 as u16;
        let alloc = PortAllocation {
            id,
            project_id,
            port,
            label,
            allocation_mode: mode,
            run_config_id: rc_id,
            service_unit: svc_unit,
        };
        by_port.insert(port, alloc);
        by_project.entry(project_id).or_default().push(port);
    }

    Ok(PortRegistry {
        by_port,
        by_project,
        loaded_at: Instant::now(),
    })
}

/// Cache con refresh background. Stessa semantica di `RoutingMatrixCache`
/// ma non panica se la tabella e' vuota.
#[derive(Clone)]
pub struct PortRegistryCache {
    inner: Arc<RwLock<Arc<PortRegistry>>>,
    db: PgPool,
}

impl std::fmt::Debug for PortRegistryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortRegistryCache").finish_non_exhaustive()
    }
}

impl PortRegistryCache {
    /// Inizializza la cache. Retry 5x5s per dare tempo a Postgres di salire.
    /// A differenza di RoutingMatrixCache, NON panica se la tabella e' vuota
    /// (situazione normale al primo avvio senza allocazioni).
    /// Panica solo se la tabella non esiste (migrazione 0114 non applicata).
    pub async fn init(db: PgPool) -> Self {
        let mut last_err: Option<String> = None;
        let mut initial: Option<PortRegistry> = None;

        for attempt in 1..=5 {
            match fetch_from_db(&db).await {
                Ok(reg) => {
                    info!(
                        "port_registry: caricato da DB ({} allocazioni)",
                        reg.by_port.len()
                    );
                    initial = Some(reg);
                    last_err = None;
                    break;
                }
                Err(e) => {
                    warn!(
                        "port_registry: tentativo {}/5 di load DB fallito ({}). Retry in 5s...",
                        attempt, e
                    );
                    last_err = Some(e);
                    if attempt < 5 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }

        let registry = match initial {
            Some(r) => r,
            None => {
                panic!(
                    "port_registry: impossibile caricare dal DB dopo 5 tentativi. \
                     Errore: {}. \
                     Verifica: (a) Postgres raggiungibile, (b) migrazione 0114 applicata.",
                    last_err.unwrap_or_else(|| "sconosciuto".to_string())
                );
            }
        };

        let inner = Arc::new(RwLock::new(Arc::new(registry)));
        let cache = Self {
            inner: inner.clone(),
            db: db.clone(),
        };

        // Spawn refresh background
        let inner_bg = inner;
        let db_bg = db;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                match fetch_from_db(&db_bg).await {
                    Ok(new_reg) => {
                        let arc = Arc::new(new_reg);
                        let mut w = inner_bg.write().await;
                        *w = arc;
                        debug!("port_registry: refresh OK");
                    }
                    Err(e) => {
                        warn!("port_registry: refresh fallito ({}). Mantengo cache precedente.", e);
                    }
                }
            }
        });

        cache
    }

    /// Snapshot corrente del registro.
    pub async fn current(&self) -> Arc<PortRegistry> {
        let g = self.inner.read().await;
        Arc::clone(&g)
    }

    /// Controlla se una porta e' disponibile.
    pub async fn is_port_available(&self, port: u16) -> bool {
        self.current().await.is_port_available(port)
    }

    /// Porte allocate a un progetto.
    pub async fn ports_for_project(&self, project_id: &Uuid) -> Vec<PortAllocation> {
        self.current().await.ports_for_project(project_id)
    }

    /// Alloca una porta (write-through: INSERT DB + aggiorna cache inline).
    /// Ritorna errore se la porta e' gia' allocata (UNIQUE violation).
    pub async fn allocate(
        &self,
        project_id: Uuid,
        port: u16,
        label: &str,
        mode: &str,
        run_config_id: Option<Uuid>,
        service_unit: Option<&str>,
    ) -> Result<PortAllocation, String> {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO nexus_port_allocations (id, project_id, port, label, allocation_mode, run_config_id, service_unit)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(id)
        .bind(project_id)
        .bind(port as i32)
        .bind(label)
        .bind(mode)
        .bind(run_config_id)
        .bind(service_unit)
        .execute(&self.db)
        .await
        .map_err(|e| {
            if e.to_string().contains("uq_port") || e.to_string().contains("unique") {
                format!("Porta {} gia' allocata a un altro progetto", port)
            } else {
                format!("Errore DB durante allocazione porta {}: {}", port, e)
            }
        })?;

        let alloc = PortAllocation {
            id,
            project_id,
            port,
            label: label.to_string(),
            allocation_mode: mode.to_string(),
            run_config_id,
            service_unit: service_unit.map(|s| s.to_string()),
        };

        // Aggiorna cache inline (write-through)
        {
            let mut w = self.inner.write().await;
            let old = &**w;
            let mut new_by_port = old.by_port.clone();
            let mut new_by_project = old.by_project.clone();
            new_by_port.insert(port, alloc.clone());
            new_by_project.entry(project_id).or_default().push(port);
            *w = Arc::new(PortRegistry {
                by_port: new_by_port,
                by_project: new_by_project,
                loaded_at: Instant::now(),
            });
        }

        Ok(alloc)
    }

    /// Rilascia una porta (DELETE DB + aggiorna cache inline).
    pub async fn release(&self, port: u16) -> Result<(), String> {
        let result = sqlx::query("DELETE FROM nexus_port_allocations WHERE port = $1")
            .bind(port as i32)
            .execute(&self.db)
            .await
            .map_err(|e| format!("Errore DB durante rilascio porta {}: {}", port, e))?;

        if result.rows_affected() == 0 {
            return Err(format!("Porta {} non trovata nel registro", port));
        }

        // Aggiorna cache inline
        {
            let mut w = self.inner.write().await;
            let old = &**w;
            let mut new_by_port = old.by_port.clone();
            let mut new_by_project = old.by_project.clone();

            if let Some(alloc) = new_by_port.remove(&port) {
                if let Some(ports) = new_by_project.get_mut(&alloc.project_id) {
                    ports.retain(|p| *p != port);
                    if ports.is_empty() {
                        new_by_project.remove(&alloc.project_id);
                    }
                }
            }

            *w = Arc::new(PortRegistry {
                by_port: new_by_port,
                by_project: new_by_project,
                loaded_at: Instant::now(),
            });
        }

        Ok(())
    }

    /// Rilascia tutte le porte di un progetto.
    pub async fn release_all_for_project(&self, project_id: &Uuid) -> Result<u64, String> {
        let result = sqlx::query("DELETE FROM nexus_port_allocations WHERE project_id = $1")
            .bind(project_id)
            .execute(&self.db)
            .await
            .map_err(|e| format!("Errore DB durante rilascio porte progetto: {}", e))?;

        // Aggiorna cache inline
        {
            let mut w = self.inner.write().await;
            let old = &**w;
            let mut new_by_port = old.by_port.clone();
            let mut new_by_project = old.by_project.clone();

            if let Some(ports) = new_by_project.remove(project_id) {
                for p in ports {
                    new_by_port.remove(&p);
                }
            }

            *w = Arc::new(PortRegistry {
                by_port: new_by_port,
                by_project: new_by_project,
                loaded_at: Instant::now(),
            });
        }

        Ok(result.rows_affected())
    }

    /// Startup recovery: sincronizza il registro con i file .service esistenti.
    ///
    /// 1. Per ogni allocazione con `service_unit`, verifica che il file esista.
    ///    Se non esiste, rimuove dal DB.
    /// 2. Scansiona i file .service in `~/.config/systemd/user/`, estrae le porte.
    ///    Se una porta e' in un .service ma non nel DB, la registra come "auto".
    ///
    /// Richiede la lista dei progetti per associare le porte al progetto corretto.
    pub async fn startup_recovery(&self) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let svc_dir = format!("{}/.config/systemd/user", home);

        // 1. Rimuovi allocazioni orfane (il file .service non esiste piu')
        let registry = self.current().await;
        for (_, alloc) in &registry.by_port {
            if let Some(ref unit) = alloc.service_unit {
                let path = format!("{}/{}", svc_dir, unit);
                if !std::path::Path::new(&path).exists() {
                    info!(
                        "port_registry recovery: rimozione porta {} (unit {} non esiste)",
                        alloc.port, unit
                    );
                    if let Err(e) = self.release(alloc.port).await {
                        warn!("port_registry recovery: rilascio porta {} fallito: {}", alloc.port, e);
                    }
                }
            }
        }
        drop(registry);

        // 2. Scansiona i .service, registra porte mancanti.
        // Mappa slug -> project_id per associare le porte ai progetti
        let project_map: HashMap<String, Uuid> = match sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, LOWER(REPLACE(REPLACE(name, ' ', '-'), '_', '-')) FROM projects"
        )
        .fetch_all(&self.db)
        .await
        {
            Ok(rows) => rows.into_iter().map(|(id, slug)| (slug, id)).collect(),
            Err(e) => {
                warn!("port_registry recovery: query projects fallita: {}", e);
                return;
            }
        };

        // Lettura filesystem sincrona: spawn_blocking per non bloccare tokio
        let svc_dir_owned = svc_dir.clone();
        let project_map_clone = project_map.clone();
        let discovered: Vec<(String, Uuid, Vec<u16>)> = tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let entries = match std::fs::read_dir(&svc_dir_owned) {
                Ok(e) => e,
                Err(_) => return results,
            };
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".service") { continue; }
                let mut matched_project: Option<Uuid> = None;
                for (slug, pid) in &project_map_clone {
                    let prefix = format!("{}-", slug);
                    if fname.starts_with(&prefix) {
                        matched_project = Some(*pid);
                        break;
                    }
                }
                let project_id = match matched_project {
                    Some(pid) => pid,
                    None => continue,
                };
                let path = entry.path();
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let ports = extract_ports_from_unit_content(&content);
                if !ports.is_empty() {
                    results.push((fname, project_id, ports));
                }
            }
            results
        }).await.unwrap_or_default();

        for (unit_name, project_id, ports) in discovered {
            for port in ports {
                if self.is_port_available(port).await {
                    info!(
                        "port_registry recovery: registrazione porta {} da {} per progetto {}",
                        port, unit_name, project_id
                    );
                    if let Err(e) = self.allocate(
                        project_id, port, "", "auto", None, Some(&unit_name),
                    ).await {
                        warn!("port_registry recovery: allocazione porta {} fallita: {}", port, e);
                    }
                }
            }
        }
    }
}

/// Rilascia le allocazioni porta "dynamic"/"auto" (non systemd) ORFANE: oltre la
/// grace period e SENZA alcun listener TCP. Sono i residui dei tentativi falliti
/// degli agenti (es. `pnpm dev` su porte diverse) che il recovery systemd non
/// pulisce. Ritorna il numero di allocazioni rilasciate.
pub async fn cleanup_orphaned_ports(db: &PgPool, grace_secs: i64) -> u64 {
    let grace = grace_secs.max(60);
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT project_id, port FROM nexus_port_allocations \
         WHERE service_unit IS NULL AND created_at < NOW() - make_interval(secs => $1)",
    )
    .bind(grace as f64)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut released = 0u64;
    for (project_id, port) in rows {
        let p = port as u16;
        // Se qualcuno ascolta sulla porta, e' in uso: non la tocchiamo.
        if crate::project_workspace::port_recovery::tcp_probe(p, 200).await {
            continue;
        }
        let n = sqlx::query(
            "DELETE FROM nexus_port_allocations \
             WHERE project_id = $1 AND port = $2 AND service_unit IS NULL",
        )
        .bind(project_id)
        .bind(port)
        .execute(db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
        released += n;
    }
    if released > 0 {
        info!("port_gc: rilasciate {released} allocazioni orfane (nessun listener)");
    }
    released
}

/// Radice dei progetti utente sotto cui un dev-server e' considerato candidato.
const USER_PROJECTS_ROOT: &str = "/home/administrator/projects/";
/// Radice del meta-progetto Nexus: i processi con cwd qui sono infrastruttura e
/// NON vanno mai terminati (regola E di CLAUDE.md).
const NEXUS_ROOT: &str = "/home/administrator/ideai";

/// True se la command line (token NUL-separati di /proc/{pid}/cmdline) appartiene
/// a un dev-server di un'app utente (Vite, Next, `pnpm dev`, ecc.). Esclude
/// esplicitamente build/install e i processi di Nexus stesso.
fn is_dev_server_cmdline(cmdline: &str) -> bool {
    let cl = cmdline.to_lowercase();
    // Esclusioni: build/install e tooling che non e' un long-running dev-server.
    const EXCLUDE: &[&str] = &[
        "npm install",
        "pnpm install",
        "yarn install",
        "esbuild --service",
        "mcp-core",
        "nexus-orchestrator",
        "brain",
        "next build",
        "vite build",
    ];
    if EXCLUDE.iter().any(|kw| cl.contains(kw)) {
        return false;
    }
    const INCLUDE: &[&str] = &[
        "vite.js",
        "vite",
        "next dev",
        "next-server",
        "pnpm dev",
        "npm run dev",
        "npm exec dev",
    ];
    INCLUDE.iter().any(|kw| cl.contains(kw))
}

/// Legge il campo 22 (`starttime`, clock ticks dall'avvio del sistema) da
/// /proc/{pid}/stat. None se non leggibile/parsabile. Serve per individuare il
/// processo piu' recente fra duplicati dello stesso progetto.
fn read_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (campo 2) puo' contenere spazi/parentesi: si parte da dopo l'ultima
    // ')'. Dopo di essa, `starttime` e' il 20° campo (campo 22 globale, 3..=21
    // sono state..itrealvalue).
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // Indici 0-based in `after`: 0=state(3) ... 19=starttime(22).
    fields.get(19).and_then(|s| s.parse::<u64>().ok())
}

/// Termina i dev-server duplicati per progetto avviati fuori dal registry (es.
/// `pnpm dev`/`vite` rilanciati a mano: Vite auto-incrementa la porta lasciando
/// le istanze precedenti vive e non tracciate in `nexus_port_allocations`).
///
/// Scansiona /proc, raggruppa i candidati per cwd risolto (= project_root sotto
/// `/home/administrator/projects/`) e in ogni gruppo con piu' di un processo tiene
/// solo il piu' recente (start time piu' alto), terminando gli altri via
/// `kill_process_tree`. Ritorna quanti processi sono stati terminati. Gated da
/// `agent.port_gc.dedupe_dev_servers` (regola G).
pub async fn cleanup_duplicate_dev_servers(db: &PgPool) -> u64 {
    let enabled = crate::settings::get_setting(db, "agent.port_gc.dedupe_dev_servers")
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    if !enabled {
        return 0;
    }

    let own_pid = std::process::id();
    // cwd risolto -> lista (pid, start_time).
    let mut groups: HashMap<String, Vec<(u32, u64)>> = HashMap::new();

    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let pid: u32 = match entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == own_pid {
            continue;
        }

        // cmdline NUL-separated -> spazi per il matching dei pattern.
        let cmdline_raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline_raw.is_empty() {
            continue; // kernel thread o processo sparito
        }
        let cmdline: String = cmdline_raw
            .split(|b| *b == 0)
            .map(|seg| String::from_utf8_lossy(seg))
            .collect::<Vec<_>>()
            .join(" ");
        if !is_dev_server_cmdline(&cmdline) {
            continue;
        }

        // cwd reale: scarta se fuori dai progetti utente o dentro Nexus (regola E).
        let cwd = match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let cwd_str = cwd.to_string_lossy().to_string();
        if cwd_str.starts_with(NEXUS_ROOT) || !cwd_str.starts_with(USER_PROJECTS_ROOT) {
            continue;
        }

        let start = read_start_time(pid).unwrap_or(0);
        groups.entry(cwd_str).or_default().push((pid, start));
    }

    let mut killed = 0u64;
    for (cwd, mut procs) in groups {
        if procs.len() < 2 {
            continue;
        }
        // Ordina per start time decrescente: il primo e' il piu' recente, da tenere.
        procs.sort_by(|a, b| b.1.cmp(&a.1));
        let keep = procs[0].0;
        for (pid, _) in procs.iter().skip(1) {
            info!(
                "cleanup_duplicate_dev_servers: terminato dev-server duplicato pid={} cwd={} (tengo pid piu' recente {})",
                pid, cwd, keep
            );
            crate::project_workspace::port_recovery::kill_process_tree(*pid).await;
            killed += 1;
        }
    }

    if killed > 0 {
        warn!("cleanup_duplicate_dev_servers: terminati {killed} dev-server duplicati");
    }
    killed
}

/// Worker periodico di garbage-collection delle porte orfane. `interval_secs` e
/// `grace_secs` sono passati dal chiamante (DB-driven).
pub async fn port_gc_loop(db: PgPool, interval_secs: u64, grace_secs: i64) {
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs.max(30)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        cleanup_orphaned_ports(&db, grace_secs).await;
        cleanup_duplicate_dev_servers(&db).await;
    }
}

/// Estrae le porte da un contenuto di file .service (replica semplificata
/// di `extract_ports_from_unit` in services.rs per evitare dipendenze circolari).
fn extract_ports_from_unit_content(content: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Environment=") {
            for segment in rest.split_whitespace() {
                if let Some(val) = segment.split('=').nth(1) {
                    if let Ok(p) = val.parse::<u16>() {
                        if p > 0 { ports.push(p); }
                    } else {
                        // URL con porta (es. http://+:5215)
                        for part in val.split(';') {
                            if let Some(colon_pos) = part.rfind(':') {
                                let after = &part[colon_pos + 1..];
                                let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                                if let Ok(p) = num_str.parse::<u16>() {
                                    if p > 0 { ports.push(p); }
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            for (i, tok) in tokens.iter().enumerate() {
                if (*tok == "--port" || *tok == "-p" || *tok == "--server.port")
                    && i + 1 < tokens.len()
                {
                    if let Ok(p) = tokens[i + 1].parse::<u16>() {
                        if p > 0 { ports.push(p); }
                    }
                }
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}
