//! Registro centralizzato delle porte TCP allocate ai progetti.
//!
//! Pattern: cache DB con refresh background 60s (come `RoutingMatrixCache`).
//! La differenza principale: NON panica se la tabella e' vuota (nessun
//! progetto potrebbe avere allocazioni al primo avvio).
//!
//! Ogni porta allocata e' unica a livello DB (vincolo UNIQUE su `port`).
//! Il registro viene consultato da `find_free_project_port()` per evitare
//! conflitti e dagli endpoint API per assegnazioni manuali.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

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

    /// Insieme di tutte le porte allocate (per `find_free_project_port`).
    pub fn all_allocated_ports(&self) -> Vec<u16> {
        self.by_port.keys().copied().collect()
    }
}

/// Carica il registro dal DB.
async fn fetch_from_db(db: &PgPool) -> Result<PortRegistry, String> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            i32,
            String,
            String,
            Option<Uuid>,
            Option<String>,
        ),
    >(
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
    /// Cache vuota per i test unit: nessun accesso al DB finche' non parte
    /// un refresh (che nei test non viene mai avviato). Il pool puo' essere
    /// un `connect_lazy` mai contattato.
    #[cfg(test)]
    pub(crate) fn empty_for_tests(db: PgPool) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(PortRegistry {
                by_port: HashMap::new(),
                by_project: HashMap::new(),
            }))),
            db,
        }
    }

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
                        warn!(
                            "port_registry: refresh fallito ({}). Mantengo cache precedente.",
                            e
                        );
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
            });
        }

        Ok(())
    }

    /// Startup recovery: registra le porte dichiarate dai file `.service` su
    /// disco e assenti dal registro. SOLO aggiunte: qui non si rilascia nulla.
    ///
    /// Il rilascio di un'allocazione ha un punto unico (regola L),
    /// `cleanup_orphaned_ports`, e un criterio osservabile (regola M): nessun
    /// listener sulla porta, oltre la grace period, senza una riserva che la
    /// giustifichi, e mai per una `manual`.
    ///
    /// Qui viveva un SECONDO criterio, che diceva il contrario: rilasciava ogni
    /// allocazione il cui `service_unit` non avesse un file
    /// `~/.config/systemd/user/<unit>`. Su Windows quel file non esiste per
    /// costruzione — i servizi di progetto sono processi gestiti, non unit
    /// systemd — quindi ogni riavvio di mcp-core svuotava il registro di TUTTE
    /// le righe con `service_unit`: proprio quelle dei servizi gestiti, che
    /// `web_service_port_env` annota via `link_allocation_to_service_unit` per
    /// PROTEGGERLE dal GC. Protette a regime, distrutte all'avvio. E il rilascio
    /// passava da `release`, che cancella per sola porta: nemmeno le `manual`
    /// erano al riparo.
    ///
    /// Misurato il 31/07/2026 su bacheca-attivita: `nexus_port_allocations` vuota
    /// per ogni progetto, backend in ascolto sulla 24826 dalle 22:21 del giorno
    /// prima con lo stesso PID, e l'audit fermo alla riallocazione che precede il
    /// riavvio dello stack. Senza allocazioni la readiness TCP non e' applicabile
    /// (`service_observer::structural_reason` richiede `!ports.is_empty()`) e i
    /// servizi vivi venivano giudicati dai soli PID vecchi, cioe' guasti.
    ///
    /// Un file unit non e' mai stato la domanda giusta: dice come il servizio
    /// andrebbe avviato, non se qualcuno stia ascoltando adesso.
    pub async fn startup_recovery(&self) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let svc_dir = format!("{}/.config/systemd/user", home);

        // Scansiona i .service, registra porte mancanti.
        // Mappa slug -> project_id per associare le porte ai progetti
        let project_map: HashMap<String, Uuid> = match sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, LOWER(REPLACE(REPLACE(name, ' ', '-'), '_', '-')) FROM projects",
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
                if !fname.ends_with(".service") {
                    continue;
                }
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
        })
        .await
        .unwrap_or_default();

        for (unit_name, project_id, ports) in discovered {
            for port in ports {
                if self.is_port_available(port).await {
                    info!(
                        "port_registry recovery: registrazione porta {} da {} per progetto {}",
                        port, unit_name, project_id
                    );
                    if let Err(e) = self
                        .allocate(project_id, port, "", "auto", None, Some(&unit_name))
                        .await
                    {
                        warn!(
                            "port_registry recovery: allocazione porta {} fallita: {}",
                            port, e
                        );
                    }
                }
            }
        }
    }
}

/// Vero se il file unit `~/.config/systemd/user/<unit>` esiste E dichiara ANCORA
/// `port` tra le sue porte (Environment=PORT / ExecStart `--port`). Identifica le
/// RISERVE dei servizi configurati ma temporaneamente fermi, da NON rilasciare
/// come orfane. Punto unico (regola L) del parsing porte: riusa
/// `extract_ports_from_unit_content`. False se il file non esiste piu' (servizio
/// rimosso) o non dichiara piu' quella porta (mapping stale dopo riconfig).
#[cfg_attr(windows, allow(dead_code))]
async fn service_unit_reserves_port(unit: &str, port: u16) -> bool {
    let unit = unit.trim();
    if unit.is_empty() {
        return false;
    }
    // Windows nativo: NIENTE systemd (i servizi di progetto sono processi gestiti).
    // Il path `~/.config/systemd/user/<unit>` non esiste -> la lettura fallirebbe
    // sempre -> il GC rilasciava la porta di un servizio managed fermo (drift 31792
    // ->31798 / pannello Porte svuotato, bug Beaty-Book). Qui la PRESENZA di una
    // `service_unit` non vuota e' gia' il segnale che un servizio managed RISERVA la
    // porta: la preserviamo (piu' conservativo = porta stabile ai riavvii). `cfg!`
    // runtime (non attributo) cosi' entrambi i rami compilano su ogni piattaforma
    // (nessun dead_code / unused_async).
    if cfg!(windows) {
        let _ = port;
        return true;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
    let path = format!("{home}/.config/systemd/user/{unit}");
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => extract_ports_from_unit_content(&content).contains(&port),
        Err(_) => false,
    }
}

/// Segnale strutturato (regola M) che il servizio dietro un'allocazione ESISTE
/// ancora su Windows: una delle `labels` di servizio del progetto (agent_processes
/// kind='service') ricostruisce, via il PUNTO UNICO `service_unit_name(slug,label)`
/// (regola L), esattamente `unit`. Puro e testabile su ogni piattaforma.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_unit_backed_by_label(slug: &str, unit: &str, labels: &[String]) -> bool {
    labels
        .iter()
        .any(|l| crate::project_workspace::services::service_unit_name(slug, l) == unit)
}

/// Windows: l'allocazione con `service_unit = unit` riserva ancora la porta? Vero
/// SOLO se il servizio e' ancora installato, cioe' esiste una riga agent_processes
/// (kind='service') del progetto che ricostruisce quell'unit. Uninstall e
/// clear-finished cancellano quelle righe -> ritorna false -> l'allocazione orfana
/// viene rilasciata dal GC. Sostituisce il `return true` cieco che tratteneva per
/// sempre le porte fantasma dei servizi rimossi (bug pannello Porte segnalato).
#[cfg(windows)]
async fn windows_service_unit_still_exists(db: &PgPool, project_id: Uuid, unit: &str) -> bool {
    // Postura DB-down (regole G + M): un errore TRANSITORIO del DB (acquire timeout
    // su pool saturo, riconnessione, provisioning) NON deve diventare "servizio
    // disinstallato" -> porterebbe al DELETE distruttivo della riserva e al drift
    // porta 31792->31798, la regressione che questo fix deve EVITARE. Rilasciamo
    // SOLO su esito Ok deterministico (progetto o label realmente assenti =
    // uninstall/clear-finished); qualunque Err = fail-closed = preserva la riserva.
    let name = match sqlx::query_scalar::<_, String>("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(n)) => n,
        Ok(None) => return false, // progetto realmente rimosso: riserva orfana
        Err(_) => return true,    // META transitorio: preserva la riserva
    };
    let slug = crate::project_workspace::services::project_service_slug(&name);
    // agent_processes e' tabella migrata: instrada sul pool del progetto. Pool
    // PROGETTO non disponibile = transitorio: fail-closed, preserva la riserva
    // (stessa postura del ramo Err della query sotto).
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "windows_service_unit_still_exists: DB progetto non disponibile, preservo la riserva"
            );
            return true;
        }
    };
    match sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT label FROM agent_processes WHERE project_id = $1 AND kind = 'service'",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    {
        Ok(labels) => windows_unit_backed_by_label(&slug, unit, &labels),
        Err(_) => true, // pool PROGETTO transitorio: preserva la riserva
    }
}

/// Rilascia le allocazioni porta auto-gestite ORFANE: oltre la grace period e
/// SENZA alcun listener TCP. Sono i residui dei tentativi falliti degli agenti
/// (es. `pnpm dev` su porte diverse) E i mapping stale lasciati da un servizio
/// riconfigurato su una porta diversa (l'allocazione conserva un `service_unit`
/// ma la porta non e' piu' quella dichiarata dall'unit -> porta fantasma nel
/// pannello Run&Debug).
///
/// Criterio (regola H, DUE cause radice conciliate):
///  - Porta con listener TCP -> in uso (servizio vivo), mai toccata.
///  - Porta SENZA listener il cui `service_unit` punta a un file .service
///    ESISTENTE che dichiara ANCORA quella porta -> NON orfana: e' la RISERVA di
///    un servizio configurato ma fermo, preservata come le `manual`. Senza questo
///    il pannello "Porte allocate" si svuotava di continuo per i servizi gestiti
///    spenti (es. frontend.service in WSL/detached) e il link al servizio spariva.
///  - Tutto il resto oltre la grace e senza listener (no `service_unit`, file unit
///    mancante = servizio rimosso, o porta non piu' dichiarata dall'unit = mapping
///    stale dopo riconfig) viene rilasciato.
///
/// Le `manual` (riserve esplicite dell'utente) restano sempre. Ritorna il numero
/// rilasciate.
pub async fn cleanup_orphaned_ports(db: &PgPool, grace_secs: i64) -> u64 {
    let grace = grace_secs.max(60);
    let rows: Vec<(Uuid, i32, Option<String>)> = sqlx::query_as(
        "SELECT project_id, port, service_unit FROM nexus_port_allocations \
         WHERE allocation_mode <> 'manual' AND created_at < NOW() - make_interval(secs => $1)",
    )
    .bind(grace as f64)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut released = 0u64;
    for (project_id, port, service_unit) in rows {
        let p = port as u16;
        // Le protezioni qui sotto (listener vivo, riserva di un servizio fermo)
        // dicono "questa allocazione e' viva, non toccarla". Hanno senso solo per
        // una riga che il progetto ha DIRITTO di avere: applicarle a un artefatto
        // significa proteggerlo proprio mentre fa danno.
        //
        // Il criterio era il range GLOBALE (20000-39999), e prendeva dentro anche
        // le porte del bucket di un ALTRO progetto: una 5434 (Postgres, dove il
        // servizio si CONNETTE) cadeva subito, ma una 20001 registrata al progetto
        // sbagliato restava protetta finche' qualcuno ci ascoltava. Ora il criterio
        // e' l'AUTORIZZAZIONE (punto unico, regola L): nel proprio bucket, oppure
        // allocata a mano. Le `manual` non arrivano nemmeno qui, escluse dalla
        // query. La chiusura alla fonte e' in `register_detected_port`.
        if nexus_tool_kit::ports::port_authorized_for_project(db, &project_id, p).await {
            // Se qualcuno ascolta sulla porta, e' in uso: non la tocchiamo.
            if crate::project_workspace::port_recovery::port_listening(p).await {
                continue;
            }
            // Riserva di un servizio configurato ma FERMO -> non e' orfana, va
            // preservata (come le `manual`). Criterio platform-specifico:
            //  - POSIX: il file unit esiste ancora e dichiara ANCORA questa porta.
            //  - Windows: NON esiste alcun file unit. Segnale strutturato onesto
            //    (regola M): esiste ancora una riga agent_processes (servizio
            //    installato) che ricostruisce quell'unit? uninstall / clear-finished
            //    cancellano quelle righe -> assenza = servizio rimosso = allocazione
            //    ORFANA da rilasciare. Su errore DB TRANSITORIO si PRESERVA
            //    (windows_service_unit_still_exists e' fail-closed).
            if let Some(ref unit) = service_unit {
                let reserved = {
                    #[cfg(windows)]
                    {
                        windows_service_unit_still_exists(db, project_id, unit).await
                    }
                    #[cfg(not(windows))]
                    {
                        service_unit_reserves_port(unit, p).await
                    }
                };
                if reserved {
                    continue;
                }
            }
        }
        let n = sqlx::query(
            "DELETE FROM nexus_port_allocations \
             WHERE project_id = $1 AND port = $2 AND allocation_mode <> 'manual'",
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

/// True se la command line (token NUL-separati di /proc/{pid}/cmdline) appartiene
/// a un dev-server di un'app utente (Vite, Next, `pnpm dev`, ecc.). Esclude
/// esplicitamente build/install e i processi di Nexus stesso.
///
/// Solo Unix: usata esclusivamente dalla scansione `/proc` della dedup dev-server,
/// no-op su Windows.
#[cfg(unix)]
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
///
/// Solo Unix: `/proc` non esiste su Windows. La dedup dev-server che la usa
/// (`cleanup_duplicate_dev_servers`) e' no-op su Windows (vedi variante dedicata).
#[cfg(unix)]
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

/// Legge il PPID (campo 4 globale di /proc/{pid}/stat) del processo. None se non
/// leggibile/parsabile. Serve a riconoscere le catene padre-figlio fra dev-server
/// della stessa cwd: un albero `pnpm dev -> node vite -> esbuild` ha UNA sola
/// radice e NON va trattato come N duplicati (incidente 2026-06-06: il kill di
/// gruppo di un anello della catena abbatteva l'intero process group, mcp-core
/// incluso). Solo Unix (dipende da `/proc`).
#[cfg(unix)]
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?; // campo 3 (state)
    fields.next()?.parse::<u32>().ok() // campo 4 (ppid)
}

/// Punto unico (regola L) della logica di dedup dei dev-server di una stessa cwd.
/// Signature di confronto di un dev-server: la cmdline con le sequenze di 4+
/// cifre (le porte dev tipiche -- 3000/5173/21954...) rimosse. Cosi' due istanze
/// dello STESSO server su porte diverse (Vite che auto-incrementa) collassano
/// sulla stessa signature ed e' giusto deduplicarle, mentre servizi DIVERSI dello
/// stesso progetto (es. `pnpm run dev:frontend` vs `pnpm run dev:backend`)
/// restano distinti. Versioni e numeri brevi (1-3 cifre, es. `vite@5.4.21`,
/// `worker:2`) sono preservati per non collassare servizi distinti.
///
/// Funzione pura testabile su ogni OS. Su Windows non ha chiamanti non-test (la
/// dedup dev-server e' no-op), quindi si sopprime dead_code lasciando i test attivi.
#[cfg_attr(windows, allow(dead_code))]
fn dev_server_signature(cmdline: &str) -> String {
    static RE_PORTLIKE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\d{4,}").unwrap());
    RE_PORTLIKE.replace_all(cmdline, "").to_string()
}

/// Input: lista `(pid, start_time, ppid, signature)` dei candidati di un gruppo
/// cwd. Output: i PID delle RADICI da terminare.
///
/// Una "radice" e' un processo il cui padre NON e' un altro membro del gruppo: una
/// catena `pnpm dev -> node vite -> esbuild` nella stessa cwd e' UN solo albero con
/// UNA radice, quindi NON un duplicato (ritorna vuoto). Tra le radici indipendenti
/// sono VERI duplicati solo quelle con la STESSA `signature` di comando (stesso
/// dev-server rilanciato): si tiene la piu' recente (start time piu' alto) e si
/// terminano le altre. Radici con signature DIVERSE sono servizi distinti dello
/// stesso progetto (es. frontend e backend lanciati entrambi da `pnpm run dev:X`
/// nella root del progetto: stessa cwd, due radici, ma NON duplicati) e non vanno
/// toccate. Estratta per essere testabile senza /proc reale; fissa la regressione
/// 2026-06-06 (catena scambiata per duplicati) e 2026-06-25 (frontend+backend
/// dello stesso progetto scambiati per duplicati -> kill del backend).
///
/// Funzione pura testabile su ogni OS. Su Windows non ha chiamanti non-test (la
/// dedup dev-server e' no-op), quindi si sopprime dead_code lasciando i test attivi.
#[cfg_attr(windows, allow(dead_code))]
fn dev_server_roots_to_kill(procs: &[(u32, u64, u32, String)]) -> Vec<u32> {
    if procs.len() < 2 {
        return Vec::new();
    }
    let pids_in_group: HashSet<u32> = procs.iter().map(|(p, _, _, _)| *p).collect();
    // Radici indipendenti (padre fuori dal gruppo) raggruppate per signature.
    let mut by_sig: HashMap<&str, Vec<(u32, u64)>> = HashMap::new();
    for (pid, start, ppid, sig) in procs {
        if !pids_in_group.contains(ppid) {
            by_sig.entry(sig.as_str()).or_default().push((*pid, *start));
        }
    }
    let mut to_kill: Vec<u32> = Vec::new();
    for (_, mut roots) in by_sig {
        if roots.len() < 2 {
            continue; // un solo dev-server con questa signature: non e' un duplicato.
        }
        // Ordina per start time decrescente: la prima e' la piu' recente, da tenere.
        roots.sort_by_key(|r| std::cmp::Reverse(r.1));
        to_kill.extend(roots.iter().skip(1).map(|(p, _)| *p));
    }
    to_kill
}

/// Termina i dev-server duplicati per progetto avviati fuori dal registry (es.
/// `pnpm dev`/`vite` rilanciati a mano: Vite auto-incrementa la porta lasciando
/// le istanze precedenti vive e non tracciate in `nexus_port_allocations`).
///
/// Scansiona /proc, raggruppa i candidati per cwd risolto (= project_root sotto
/// `/home/administrator/projects/`) e, in ogni gruppo, individua le RADICI degli
/// alberi di processo (un dev-server e' tipicamente la catena `pnpm dev -> node
/// vite -> esbuild`: una sola radice). Se ci sono 2+ radici indipendenti sotto la
/// stessa cwd tiene la piu' recente (start time piu' alto) e termina le altre via
/// `kill_process_tree`. Deduplicare per radice (e non per singolo processo) evita
/// di scambiare gli anelli figli di una catena per duplicati e di abbattere il
/// process group del dev-server vivo (incidente 2026-06-06). Ritorna quanti
/// processi sono stati terminati. Gated da `agent.port_gc.dedupe_dev_servers`
/// (regola G).
///
/// Solo Unix: l'intera logica dipende da `/proc` (scan, cmdline, cwd, start_time,
/// ppid). Su Windows non e' determinabile in modo affidabile quale, tra due
/// dev-server della stessa cwd, sia il duplicato da terminare (mancano start_time
/// e ppid via `/proc`): la variante Windows e' un no-op sicuro (vedi sotto),
/// perche' non terminare e' preferibile a uccidere il processo sbagliato.
#[cfg(unix)]
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
    // cwd risolto -> lista (pid, start_time, ppid). Il ppid serve a distinguere
    // i veri duplicati (alberi indipendenti) dalle catene padre-figlio.
    let mut groups: HashMap<String, Vec<(u32, u64, u32, String)>> = HashMap::new();

    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return 0,
    };

    // Radici risolte a runtime (regola G: nessun path hardcoded; regola L: la
    // radice progetti utente arriva dal punto unico `load_projects_base_root`,
    // che legge la setting `projects_base_root`). Cosi' il dedup segue
    // `D:/IDEAI-projects` su Windows e `/home/administrator/projects` su WSL,
    // senza costanti compilate. Trailing slash per evitare match parziali
    // (`/.../projects` non deve combaciare con `/.../projects-altro`).
    let user_projects_root = match crate::projects::load_projects_base_root(db).await {
        Ok(p) => {
            let mut s = p.to_string_lossy().replace('\\', "/");
            if !s.ends_with('/') {
                s.push('/');
            }
            s
        }
        Err(_) => {
            warn!("cleanup_duplicate_dev_servers: projects_base_root non risolvibile — skip");
            return 0;
        }
    };
    // Radice del meta-progetto Nexus (regola E): i processi con cwd qui sono
    // infrastruttura e NON vanno mai terminati. Stessa fonte (`NEXUS_REPO_ROOT`)
    // usata da claude_agents.rs / services_watchdog.rs.
    let nexus_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string())
        .replace('\\', "/");

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
        if cwd_str.starts_with(nexus_root.as_str())
            || !cwd_str.starts_with(user_projects_root.as_str())
        {
            continue;
        }

        let start = read_start_time(pid).unwrap_or(0);
        let ppid = read_ppid(pid).unwrap_or(0);
        let sig = dev_server_signature(&cmdline);
        groups
            .entry(cwd_str)
            .or_default()
            .push((pid, start, ppid, sig));
    }

    let mut killed = 0u64;
    for (cwd, procs) in groups {
        // Dedup-per-radice nel punto unico testabile (regola L): ritorna i PID
        // delle radici da terminare (vuoto se la cwd ha un solo albero/dev-server,
        // es. la catena `pnpm dev -> node vite -> esbuild`). Evita di scambiare gli
        // anelli figli per duplicati e di abbattere il process group del dev-server
        // vivo, mcp-core incluso (incidente 2026-06-06).
        let to_kill = dev_server_roots_to_kill(&procs);
        for pid in &to_kill {
            // Anti-race: verifica che tra lo scan e la kill il PID non sia stato
            // riciclato (processo morto, kernel assegna lo stesso numero a un
            // nuovo processo non-dev-server, eventualmente mcp-core stesso).
            // Ri-leggiamo cmdline+cwd: se non matchano piu', skip.
            let cmdline_now = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            if cmdline_now.is_empty() {
                // Processo gia' morto: niente da fare.
                continue;
            }
            let cmdline_now_s: String = cmdline_now
                .split(|b| *b == 0)
                .map(|seg| String::from_utf8_lossy(seg))
                .collect::<Vec<_>>()
                .join(" ");
            if !is_dev_server_cmdline(&cmdline_now_s) {
                warn!(
                    "cleanup_duplicate_dev_servers: pid={} non e' piu' un dev-server (riciclato?) — skip",
                    pid
                );
                continue;
            }
            let cwd_now = match std::fs::read_link(format!("/proc/{pid}/cwd")) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            if cwd_now != cwd {
                warn!(
                    "cleanup_duplicate_dev_servers: pid={} cwd cambiato ({} -> {}) — skip",
                    pid, cwd, cwd_now
                );
                continue;
            }

            info!(
                "cleanup_duplicate_dev_servers: terminato dev-server duplicato (radice) pid={} cwd={}",
                pid, cwd
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

/// Variante Windows: NO-OP sicuro. La dedup dev-server si basa su `start_time` e
/// `ppid` letti da `/proc`, che su Windows non esistono; senza questi segnali non
/// si puo' stabilire in modo affidabile quale istanza sia il duplicato da
/// terminare. Degrado consapevole (regola H, no toppe): meglio non terminare
/// nulla che rischiare di uccidere il dev-server vivo o un processo riciclato.
/// Ritorna sempre 0 (nessun processo terminato). Chiamata da `port_gc_loop`.
#[cfg(windows)]
pub async fn cleanup_duplicate_dev_servers(_db: &PgPool) -> u64 {
    0
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
                        if p > 0 {
                            ports.push(p);
                        }
                    } else {
                        // URL con porta (es. http://+:5215)
                        for part in val.split(';') {
                            if let Some(colon_pos) = part.rfind(':') {
                                let after = &part[colon_pos + 1..];
                                let num_str: String =
                                    after.chars().take_while(|c| c.is_ascii_digit()).collect();
                                if let Ok(p) = num_str.parse::<u16>() {
                                    if p > 0 {
                                        ports.push(p);
                                    }
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
                        if p > 0 {
                            ports.push(p);
                        }
                    }
                }
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_orphaned_ports, dev_server_roots_to_kill, dev_server_signature,
        extract_ports_from_unit_content, windows_unit_backed_by_label, PortRegistryCache,
    };

    /// Il GC proteggeva l'artefatto proprio mentre faceva danno.
    ///
    /// Le protezioni (listener vivo, riserva di un servizio fermo) dicono
    /// "allocazione viva, non toccarla", e si applicavano a tutto cio' che stava
    /// nel range GLOBALE 20000-39999. Una porta del bucket di un ALTRO progetto ci
    /// sta dentro: bastava che il processo che l'aveva presa fosse ancora in
    /// ascolto perche' la riga restasse li' per sempre - e finche' restava, il
    /// linter e il port_scanner la leggevano come autorizzazione.
    ///
    /// Il test tiene un listener VERO su quella porta, perche' e' esattamente la
    /// condizione in cui prima la riga sopravviveva: senza listener sarebbe stata
    /// rilasciata anche dal codice vecchio, e il test non misurerebbe nulla
    /// (regola O).
    ///
    /// Mutazione che rende rosso: rimettere il criterio del range globale
    /// (`PROJECT_PORT_RANGE_START..=PROJECT_PORT_RANGE_END`) al posto
    /// dell'autorizzazione -> la riga 'auto' torna protetta dal probe e la prima
    /// asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_gc_libera_l_artefatto_in_ascolto_e_rispetta_le_manual(pool: sqlx::PgPool) {
        let (_user, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, bucket_end) =
            crate::project_workspace::services::project_bucket_range(&project_id);

        // Una porta del range progetti fuori dal bucket di QUESTO progetto, su cui
        // si possa davvero ascoltare. Si cerca la prima libera: il range ne ha
        // ~20000, quindi la ricerca non e' una scommessa.
        let mut occupata = None;
        for p in 20000u16..=39999 {
            if (bucket_start..=bucket_end).contains(&p) {
                continue;
            }
            if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", p)) {
                occupata = Some((p, l));
                break;
            }
        }
        let (porta_altrui, _listener) = occupata.expect("una porta libera fuori dal bucket");
        // La `manual` sta fuori bucket come l'altra: cio' che le distingue e' solo
        // il modo, cioe' che una persona l'ha decisa.
        let manuale = if porta_altrui == 20000 { 20001 } else { 20000 };
        assert!(!(bucket_start..=bucket_end).contains(&manuale));

        // Label distinte: `uq_port_alloc_project_label` (mig 0434) ammette una sola
        // riga per (progetto, label).
        for (port, label, mode) in [
            (porta_altrui as i32, "backend", "auto"),
            (manuale as i32, "riserva-utente", "manual"),
        ] {
            sqlx::query(
                "INSERT INTO nexus_port_allocations \
                   (project_id, port, label, allocation_mode, created_at) \
                 VALUES ($1, $2, $3, $4, NOW() - INTERVAL '1 hour')",
            )
            .bind(project_id)
            .bind(port)
            .bind(label)
            .bind(mode)
            .execute(&pool)
            .await
            .expect("seed allocazione");
        }

        cleanup_orphaned_ports(&pool, 60).await;

        let rimaste: Vec<(i32, String)> = sqlx::query_as(
            "SELECT port::int, allocation_mode FROM nexus_port_allocations \
             WHERE project_id = $1 ORDER BY port",
        )
        .bind(project_id)
        .fetch_all(&pool)
        .await
        .expect("rilettura allocazioni");

        assert!(
            !rimaste.iter().any(|(p, _)| *p == porta_altrui as i32),
            "la riga 'auto' sul bucket altrui e' un artefatto: il listener vivo non \
             la rende legittima, la rende dannosa. Rimaste: {rimaste:?}"
        );
        assert!(
            rimaste
                .iter()
                .any(|(p, m)| *p == manuale as i32 && m == "manual"),
            "una riserva decisa a mano non si tocca, dentro o fuori dal bucket. \
             Rimaste: {rimaste:?}"
        );
    }

    /// L'avvio distruggeva le allocazioni che il GC tiene in piedi.
    ///
    /// `startup_recovery` aveva un SECONDO criterio di rilascio, e diceva il
    /// contrario del primo: rilasciava ogni allocazione il cui `service_unit` non
    /// avesse un file `~/.config/systemd/user/<unit>`. Su Windows quel file non
    /// esiste per costruzione (i servizi di progetto sono processi gestiti, non
    /// unit systemd), quindi ogni riavvio di mcp-core svuotava il registro di
    /// tutte le righe con `service_unit` — cioe' proprio quelle dei servizi
    /// gestiti, che `web_service_port_env` annota via
    /// `link_allocation_to_service_unit` per PROTEGGERLE dal GC. Protette a
    /// regime, distrutte all'avvio (regola L: la stessa domanda con due
    /// risposte).
    ///
    /// Misurato il 31/07/2026 su bacheca-attivita: `nexus_port_allocations` vuota
    /// per tutti i progetti, backend in ascolto sulla 24826 dalle 22:21 del
    /// giorno prima con lo stesso PID (mai riavviato) e l'audit fermo alla
    /// riallocazione che precede il riavvio dello stack.
    ///
    /// Il test tiene un listener VERO e passa dai produttori reali (`allocate` +
    /// `link_allocation_to_service_unit`, poi una seconda `init` che rilegge dal
    /// DB come fa il riavvio): il difetto stava nella giunzione fra chi scrive
    /// `service_unit` e chi lo interpreta, e costruire la riga a mano l'avrebbe
    /// saltata (regola O).
    ///
    /// Mutazione che rende rosso: rimettere in `startup_recovery` il rilascio
    /// delle allocazioni il cui file unit non esiste.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_avvio_non_rilascia_l_allocazione_di_un_servizio_in_ascolto(pool: sqlx::PgPool) {
        let (_user, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, bucket_end) =
            crate::project_workspace::services::project_bucket_range(&project_id);

        // Porta del PROPRIO bucket su cui si possa davvero ascoltare: e' la
        // condizione dell'incidente (servizio vivo), non un'ipotesi. Senza
        // listener la riga sarebbe discutibile e il test non misurerebbe nulla.
        let mut scelta = None;
        for p in bucket_start..=bucket_end {
            if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", p)) {
                scelta = Some((p, l));
                break;
            }
        }
        let (porta, _listener) = scelta.expect("una porta libera nel bucket del progetto");

        // Il run precedente: la porta viene allocata e legata all'unit del
        // servizio, esattamente come a ogni avvio di un servizio gestito.
        let prima = PortRegistryCache::init(pool.clone()).await;
        prima
            .allocate(project_id, porta, "backend", "dynamic", None, None)
            .await
            .expect("allocazione del run precedente");
        let unit =
            crate::project_workspace::services::project_service_unit(&pool, project_id, "backend")
                .await
                .expect("unit del servizio");
        crate::project_workspace::allocate_port::link_allocation_to_service_unit(
            &pool, project_id, "backend", &unit,
        )
        .await;
        drop(prima);

        // Il riavvio di mcp-core: la cache si ricarica dal DB (ed e' li' che il
        // `service_unit` torna visibile) e parte il recovery.
        let dopo = PortRegistryCache::init(pool.clone()).await;
        dopo.startup_recovery().await;

        let rimaste: Vec<(i32, Option<String>)> = sqlx::query_as(
            "SELECT port::int, service_unit FROM nexus_port_allocations WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_all(&pool)
        .await
        .expect("rilettura allocazioni");

        assert_eq!(
            rimaste.len(),
            1,
            "un'allocazione con un processo IN ASCOLTO non e' un residuo: il criterio e' il \
             listener (regola M), non l'esistenza di un file unit systemd. Rimaste: {rimaste:?}"
        );
        assert_eq!(rimaste[0].0, porta as i32);
        assert_eq!(
            rimaste[0].1.as_deref(),
            Some(unit.as_str()),
            "il link all'unit deve sopravvivere al riavvio: e' cio' che preserva la porta \
             quando il servizio e' fermo"
        );
    }

    #[test]
    fn windows_unit_backed_by_label_distingue_installato_da_orfano() {
        // service_unit_name(slug, label) = "{slug}-{label}.service" (punto unico).
        let labels = vec!["frontend".to_string(), "backend-dev".to_string()];
        // servizio ancora installato: una label ricostruisce l'unit -> riserva legittima
        assert!(windows_unit_backed_by_label(
            "beauty-book",
            "beauty-book-frontend.service",
            &labels
        ));
        assert!(windows_unit_backed_by_label(
            "beauty-book",
            "beauty-book-backend-dev.service",
            &labels
        ));
        // servizio disinstallato (nessuna riga lo ricostruisce) -> allocazione ORFANA
        assert!(!windows_unit_backed_by_label(
            "beauty-book",
            "beauty-book-worker.service",
            &labels
        ));
        // slug di un altro progetto -> non e' una riserva di questo
        assert!(!windows_unit_backed_by_label(
            "other-proj",
            "beauty-book-frontend.service",
            &labels
        ));
        // nessun servizio installato -> mai una riserva
        assert!(!windows_unit_backed_by_label(
            "beauty-book",
            "beauty-book-frontend.service",
            &[]
        ));
    }

    #[test]
    fn extract_ports_riconosce_riserva_servizio_unit() {
        // File unit tipico di un servizio gestito (beauty-book-frontend.service):
        // la porta e' dichiarata in Environment=PORT e in ExecStart --port.
        // `cleanup_orphaned_ports` usa questo parsing (via service_unit_reserves_port)
        // per NON rilasciare la riserva di un servizio configurato ma fermo.
        let frontend = "[Service]\n\
                        Environment=PORT=35154\n\
                        ExecStart=npx vite --port 35154 --host 0.0.0.0\n";
        assert_eq!(extract_ports_from_unit_content(frontend), vec![35154]);

        // Backend: porta in Environment=PORT e dentro un URL (NEXTAUTH_URL=...:35176).
        let backend = "[Service]\n\
                       Environment=PORT=35176\n\
                       Environment=NEXTAUTH_URL=http://localhost:35176\n\
                       ExecStart=/usr/bin/npm run start\n";
        assert_eq!(extract_ports_from_unit_content(backend), vec![35176]);

        // Nessuna porta dichiarata -> nessuna riserva (l'allocazione e' orfana).
        let noport = "[Service]\nExecStart=/usr/bin/npm run start\n";
        assert!(extract_ports_from_unit_content(noport).is_empty());
    }

    // Helper: tuple (pid, start_time, ppid, signature).
    fn p(pid: u32, start: u64, ppid: u32, sig: &str) -> (u32, u64, u32, String) {
        (pid, start, ppid, sig.to_string())
    }

    #[test]
    fn catena_padre_figlio_non_e_duplicato() {
        // pnpm(100) -> vite(200) -> esbuild(300): una sola radice (100, padre fuori
        // gruppo). NON e' un duplicato, nessun kill. Fissa la regressione del
        // 2026-06-06 (catena scambiata per duplicati -> suicidio di mcp-core).
        let procs = vec![
            p(100, 10, 1, "vite"),
            p(200, 20, 100, "vite"),
            p(300, 30, 200, "esbuild"),
        ];
        assert!(dev_server_roots_to_kill(&procs).is_empty());
    }

    #[test]
    fn due_alberi_stessa_signature_sono_duplicati() {
        // Due catene indipendenti con la STESSA signature (stesso dev-server
        // rilanciato, es. Vite). Tiene la radice piu' recente (400), termina 100.
        let procs = vec![
            p(100, 10, 1, "vite"),
            p(200, 20, 100, "vite"),
            p(400, 40, 1, "vite"),
            p(500, 50, 400, "vite"),
        ];
        assert_eq!(dev_server_roots_to_kill(&procs), vec![100]);
    }

    #[test]
    fn frontend_e_backend_non_sono_duplicati() {
        // Due radici indipendenti nella stessa cwd ma con signature DIVERSE
        // (frontend vs backend lanciati da `pnpm run dev:X` nella root del progetto).
        // NON sono duplicati: nessun kill. Fissa la regressione 2026-06-25 (il
        // backend veniva ucciso come "duplicato" del frontend).
        let procs = vec![
            p(100, 10, 1, "node /usr/bin/pnpm run dev:backend"),
            p(400, 40, 1, "node /usr/bin/pnpm run dev:frontend"),
        ];
        assert!(dev_server_roots_to_kill(&procs).is_empty());
    }

    #[test]
    fn tre_radici_stessa_signature_tiene_la_piu_recente() {
        // Tre radici indipendenti stessa signature; tiene 20 (start 9), termina 10 e 30.
        let procs = vec![
            p(10, 5, 1, "vite"),
            p(20, 9, 1, "vite"),
            p(30, 7, 1, "vite"),
        ];
        let mut k = dev_server_roots_to_kill(&procs);
        k.sort_unstable();
        assert_eq!(k, vec![10, 30]);
    }

    #[test]
    fn singolo_processo_nessun_kill() {
        assert!(dev_server_roots_to_kill(&[p(1, 1, 0, "vite")]).is_empty());
    }

    #[test]
    fn gruppo_vuoto_nessun_kill() {
        assert!(dev_server_roots_to_kill(&[]).is_empty());
    }

    #[test]
    fn signature_normalizza_le_porte_ma_non_le_versioni() {
        // Stesso server su porte diverse -> stessa signature (deduplicabile).
        assert_eq!(
            dev_server_signature("vite --port 21954"),
            dev_server_signature("vite --port 21955")
        );
        // Servizi diversi -> signature diverse (NON deduplicabili).
        assert_ne!(
            dev_server_signature("pnpm run dev:backend"),
            dev_server_signature("pnpm run dev:frontend")
        );
        // Numeri brevi (versioni, worker:N) preservati: non collassano servizi distinti.
        assert_ne!(
            dev_server_signature("pnpm run worker:1"),
            dev_server_signature("pnpm run worker:2")
        );
    }
}
