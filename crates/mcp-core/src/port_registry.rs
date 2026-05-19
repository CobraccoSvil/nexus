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
