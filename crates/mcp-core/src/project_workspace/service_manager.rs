//! Punto unico (regola L) del ciclo di vita dei "servizi di progetto" (dev
//! server, api, docker-compose registrati in un progetto Nexus).
//!
//! Il problema che chiude: la gestione dei servizi era split-brain fra Linux
//! (unit `systemd --user`) e Windows (righe in `agent_processes` + spawn/taskkill).
//! Il dispatch di piattaforma era sparso a macchia di leopardo con
//! `#[cfg(windows)]`/`#[cfg(not(windows))]` su molte funzioni; alcune l'avevano
//! dimenticato e su Windows facevano no-op o mentivano.
//!
//! Questo modulo introduce un `trait ServiceBackend` con vocabolario
//! platform-neutral e DUE implementazioni che AVVOLGONO (non riscrivono) i
//! primitivi gia' esistenti in `services.rs`, `wizard.rs`, `agent_processes.rs`.
//! La selezione del backend attivo avviene in un UNICO punto (`active()`), con un
//! type alias risolto a compile-time (composition over inheritance, niente
//! `dyn`/`Box`).
//!
//! WAVE 0 (fondazione): questo modulo compila e ha i suoi test, ma NESSUN call
//! site lo usa ancora. Il comportamento a runtime resta invariato. Le wave
//! successive migreranno gli handler HTTP, l'observer, il wizard, ecc.

use std::path::Path;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use crate::port_registry::PortRegistryCache;

// ─────────────────────────────────────────────────────────────────────────────
// Tipi di dominio platform-neutral (niente vocabolario systemd nei nomi pubblici)
// ─────────────────────────────────────────────────────────────────────────────

/// Stato normalizzato di un servizio di progetto, indipendente dalla piattaforma.
/// Il backend systemd lo popola tramite `normalize_from` senza esporre le stringhe
/// systemd; il backend Windows lo deriva dallo stato del processo gestito.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Running,
    Starting,
    Stopped,
    Failed,
    Unknown,
}

impl ServiceState {
    /// Mappa la coppia di stati testuali di systemd (`ActiveState`, `SubState`)
    /// allo stato normalizzato. Riferimento: `systemctl list-units` emette
    /// `active/inactive/failed/activating` in colonna ACTIVE e
    /// `running/exited/dead/auto-restart` in colonna SUB.
    ///
    /// Questa e' la sola conversione dal vocabolario systemd a quello neutro
    /// (regola L): il backend systemd la usa, i chiamanti non vedono mai le
    /// stringhe systemd. Regola M: qui NON si classifica prosa umana, si mappano
    /// gli enum testuali stabili dell'API systemd.
    pub fn normalize_from(active: &str, sub: &str) -> ServiceState {
        match (active, sub) {
            // Un servizio attivo e' Running solo se il sub e' `running`; con
            // `exited` (one-shot terminato con successo) lo consideriamo Stopped
            // perche' non c'e' piu' un processo vivo da fermare.
            ("active", "running") => ServiceState::Running,
            ("active", "exited") => ServiceState::Stopped,
            ("active", _) => ServiceState::Running,
            // `activating` = in avvio; `auto-restart` = crash-loop in ripartenza:
            // entrambi sono transitori verso l'attivo -> Starting.
            ("activating", _) => ServiceState::Starting,
            ("deactivating", _) => ServiceState::Stopped,
            ("failed", _) => ServiceState::Failed,
            ("inactive", _) => ServiceState::Stopped,
            _ => ServiceState::Unknown,
        }
    }
}

/// Una voce del pannello Servizi con stato normalizzato.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceEntry {
    /// Identificatore stabile del servizio: su Windows la label, su Linux il nome
    /// unit completo (`{slug}-{short}.service`).
    pub id: String,
    /// Nome corto mostrato in UI (senza prefisso slug ne' suffisso `.service`).
    pub label: String,
    pub state: ServiceState,
    pub main_pid: Option<u32>,
    /// Chi gestisce concretamente il servizio: uno fra
    /// `"windows"`, `"systemd"`, `"detached"`, `"docker-compose"`.
    pub managed_by: &'static str,
}

/// Traduce la shape JSON dei primitivi `services::list_*` in una [`ServiceEntry`].
///
/// PUNTO DI LETTURA UNICO di quella shape (regola L): i due backend — Windows e
/// systemd — leggevano gli stessi quattro campi con lo stesso `unwrap_or_default`
/// ricopiato, e quel JSON e' un contratto senza tipo. Con due letture, bastava
/// che un primitivo rinominasse `short` in `name` perche' un backend mostrasse
/// etichette vuote e l'altro no, senza che niente fallisse.
///
/// `managed_by_default` e' il gestore da attribuire quando il primitivo non lo
/// dichiara: su Windows i servizi vengono tutti da li', mentre il ramo systemd
/// distingue `detached` da `docker-compose` leggendolo dal JSON.
fn voce_da_json(v: &serde_json::Value, managed_by_default: &'static str) -> ServiceEntry {
    let stringa = |campo: &str| {
        v.get(campo)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let managed_by = match v.get("managed_by").and_then(|s| s.as_str()) {
        Some("docker-compose") => "docker-compose",
        Some("detached") => "detached",
        Some("windows") => "windows",
        Some("systemd") => "systemd",
        _ => managed_by_default,
    };
    ServiceEntry {
        id: stringa("unit"),
        label: stringa("short"),
        state: ServiceState::normalize_from(
            v.get("state").and_then(|s| s.as_str()).unwrap_or(""),
            v.get("sub").and_then(|s| s.as_str()).unwrap_or(""),
        ),
        main_pid: None,
        managed_by,
    }
}

/// Esito di un'azione start/stop/restart.
///
/// `acted` e' `true` SOLO se l'operazione ha realmente agito sul servizio
/// (spawnato/killato/riavviato un processo o una unit reale). E' il campo su cui
/// i chiamanti decideranno se emettere l'evento `ServiceStarted/Stopped/Restarted`
/// (regola M: esito da segnale strutturato, non da parsing di prosa). Chiude il
/// bug in cui `restart_project_unit` emetteva `ServiceRestarted` anche quando non
/// aveva riavviato nulla.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceActionOutcome {
    pub acted: bool,
    pub message: String,
}

impl ServiceActionOutcome {
    /// Ha realmente agito sul servizio.
    pub fn acted(message: impl Into<String>) -> Self {
        Self {
            acted: true,
            message: message.into(),
        }
    }

    /// Non c'era nulla da fare, oppure il servizio non esiste / l'azione e'
    /// fallita: nessun effetto sul servizio.
    pub fn noop(message: impl Into<String>) -> Self {
        Self {
            acted: false,
            message: message.into(),
        }
    }
}

/// Stato del "manager" dei servizi, sostituisce l'euristica booleana
/// `user_manager_unavailable` basata su `contains()` su stderr (regola M: stato da
/// segnale strutturato, non da parsing di prosa).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ManagerStatus {
    /// Il manager e' raggiungibile e gestisce i servizi (Linux: bus systemd
    /// `--user` attivo).
    Available,
    /// Il manager esiste ma non e' raggiungibile: i servizi girano in modalita'
    /// detached. `hint` e' il suggerimento operativo mostrato in UI.
    Unavailable { hint: String },
    /// Non esiste un manager da contattare (Windows: i servizi sono processi
    /// gestiti in `agent_processes`).
    NotApplicable,
}

/// Terna "chi ascolta su quale porta" gia' usata altrove (port_enforcer/cleanup).
#[derive(Debug, Clone, Serialize)]
pub struct PortListener {
    pub port: u16,
    pub pid: u32,
    pub program: String,
}

impl From<(u16, u32, String)> for PortListener {
    fn from((port, pid, program): (u16, u32, String)) -> Self {
        Self { port, pid, program }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Consolidamento path (regola G + L): UNICA fonte del percorso
// ~/.config/systemd/user, chiude i default HOME divergenti ("/home/administrator"
// vs "/root") sparsi nei vecchi call site. Le wave successive faranno convergere
// anche i vecchi call site su questa funzione; qui la creiamo e la usiamo nel
// backend nuovo.
// ─────────────────────────────────────────────────────────────────────────────

/// Percorso della directory delle unit utente systemd (`$HOME/.config/systemd/user`).
///
/// Niente HOME hardcoded: legge la env `HOME`. Il fallback e' documentato ed e'
/// una sola scelta esplicita (regola G: nessun default divergente sparso). In
/// pratica su Linux `HOME` e' sempre valorizzato per il processo mcp-core; il
/// fallback serve solo a non far panicare in ambienti degradati.
pub fn user_systemd_dir() -> String {
    // Fallback commentato: su Linux il servizio gira come utente con HOME
    // valorizzato; "/root" e' l'unico HOME certo quando la env non e' propagata
    // (avvio da unit di sistema senza `User=`), coerente con l'utente di deploy.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    format!("{home}/.config/systemd/user")
}

// ─────────────────────────────────────────────────────────────────────────────
// Trait unico
// ─────────────────────────────────────────────────────────────────────────────

/// Contesto minimo per identificare un servizio di progetto in modo
/// platform-neutral, passato per riferimento ai metodi del backend. Evita di
/// dipendere dalla struct privata `AppState` di `main.rs` e dagli handler HTTP:
/// i backend ricevono solo i componenti che servono davvero.
pub struct ServiceContext<'a> {
    /// Pool DB meta (i backend instradano internamente sul pool del progetto per
    /// le tabelle migrate, come fanno gia' i primitivi che avvolgono).
    pub db: &'a sqlx::PgPool,
    /// Registro porte per l'alloca+inietta della porta stabile ai web service
    /// Windows. `None` quando il chiamante non ha un registry a disposizione (es.
    /// i tool agente, che girano nel contesto `execute()` senza `AppState`).
    ///
    /// `None` NON significa piu' "parte senza PORT": lo start di un servizio WEB
    /// da un contesto senza registro viene RIFIUTATO e lo dice. Iniettare PORT e'
    /// il contratto di avvio di un web service — senza, la porta la sceglie il
    /// literal nel sorgente, fuori dal bucket e magari su una porta di un altro
    /// servizio (misurato il 03/08/2026 su agenda-corsi). Un contesto senza
    /// registro resta valido per list/status/stop, che non avviano nulla.
    pub port_registry: Option<&'a PortRegistryCache>,
    pub project_id: Uuid,
    /// Slug di servizio del progetto (`project_service_slug`), NON `projects.slug`.
    pub slug: &'a str,
    /// Root del progetto (working directory di default per lo spawn).
    pub project_root: &'a Path,
}

/// Backend platform-specifico per il ciclo di vita dei servizi di progetto.
///
/// Scelta del meccanismo async: `#[async_trait]`. Motivazione:
/// 1. i futures del trait devono essere `Send` (i chiamanti futuri sono
///    `tokio::spawn` in port_enforcer/observer); `async_trait` genera
///    `Pin<Box<dyn Future + Send>>`, garantendo `Send` senza acrobazie di
///    bound sui tipi di ritorno;
/// 2. `async-trait` e' gia' dipendenza pervasiva di mcp-core (usato in ~29
///    moduli, incluso tutto `agent_graph_adapter`): riusarlo mantiene un solo
///    meccanismo async per i trait del crate, coerente con il codice esistente.
#[async_trait]
pub trait ServiceBackend {
    /// Enumera i servizi del progetto con stato normalizzato.
    async fn list(&self, ctx: &ServiceContext<'_>) -> Vec<ServiceEntry>;

    /// Avvia il servizio corto `short`. Ritorna l'esito con `acted`.
    async fn start(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome;

    /// Ferma il servizio corto `short`.
    async fn stop(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome;

    /// Riavvia il servizio corto `short`.
    async fn restart(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome;

    /// Terne (porta, pid, programma) di TUTTE le porte TCP in ascolto sull'host
    /// (usato da port_enforcer/cleanup).
    async fn listening_ports(&self) -> Vec<PortListener>;

    /// Stato del manager (Linux: bus systemd `--user`; Windows: `NotApplicable`).
    async fn manager_status(&self) -> ManagerStatus;
}

// ─────────────────────────────────────────────────────────────────────────────
// Selezione unica del backend (composition over inheritance, niente dyn/Box)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub type ActiveBackend = WindowsProcessBackend;
#[cfg(not(windows))]
pub type ActiveBackend = SystemdUserBackend;

/// UNICO punto di selezione del backend attivo per la piattaforma corrente.
pub fn active() -> ActiveBackend {
    #[cfg(windows)]
    {
        WindowsProcessBackend
    }
    #[cfg(not(windows))]
    {
        SystemdUserBackend
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WindowsProcessBackend: avvolge i primitivi Windows di services.rs /
// agent_processes.rs (NON li riscrive).
// ─────────────────────────────────────────────────────────────────────────────

/// Backend Windows: i servizi di progetto sono processi gestiti in
/// `agent_processes` (kind='service'). Niente systemd.
#[cfg(windows)]
pub struct WindowsProcessBackend;

#[cfg(windows)]
#[async_trait]
impl ServiceBackend for WindowsProcessBackend {
    async fn list(&self, ctx: &ServiceContext<'_>) -> Vec<ServiceEntry> {
        // Avvolge services::list_services_windows (che a sua volta delega a
        // visible_windows_services): riusa la stessa shape JSON e la mappa nei
        // tipi neutri.
        super::services::list_services_windows(ctx.db, ctx.project_id, ctx.slug)
            .await
            .iter()
            .map(|v| voce_da_json(v, "windows"))
            .collect()
    }

    async fn start(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_windows(ctx, short, WinAction::Start).await
    }

    async fn stop(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_windows(ctx, short, WinAction::Stop).await
    }

    async fn restart(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_windows(ctx, short, WinAction::Restart).await
    }

    async fn listening_ports(&self) -> Vec<PortListener> {
        // Avvolge services::windows_listening_ports (PUNTO UNICO Windows).
        super::services::windows_listening_ports()
            .await
            .into_iter()
            .map(PortListener::from)
            .collect()
    }

    async fn manager_status(&self) -> ManagerStatus {
        // Su Windows non c'e' un manager da contattare.
        ManagerStatus::NotApplicable
    }
}

/// Azione da eseguire sul servizio Windows (evita di ripassare per l'handler HTTP).
#[cfg(windows)]
#[derive(Clone, Copy)]
enum WinAction {
    Start,
    Stop,
    Restart,
}

/// Logica start/stop/restart per il backend Windows, estratta dai passi di
/// `control_project_service_windows` in services.rs (riuso dei suoi primitivi:
/// stop_similar_running_services, spawn_agent_process, taskkill, find_or_allocate
/// + link_allocation_to_service_unit). Ritorna `acted` in base a un effetto reale.
#[cfg(windows)]
async fn control_windows(
    ctx: &ServiceContext<'_>,
    short: &str,
    action: WinAction,
) -> ServiceActionOutcome {
    use sqlx::query_as;

    // agent_processes e' migrata -> pool del progetto. DB progetto non
    // disponibile: nessuna azione (noop con motivazione), mai il meta come ripiego.
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(ctx.db, ctx.project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %ctx.project_id,
                    error = %e,
                    "control_windows: DB progetto non disponibile, nessuna azione sul servizio"
                );
                return ServiceActionOutcome::noop(format!("DB progetto non disponibile: {e}"));
            }
        };

    // STOP: taskkill dei soli processi running di questa label. `acted` = true se
    // e solo se abbiamo davvero killato almeno un pid vivo.
    if matches!(action, WinAction::Stop) {
        let running: Vec<(Option<i32>,)> = query_as(
            "SELECT pid FROM agent_processes \
             WHERE project_id = $1 AND label = $2 AND kind = 'service' AND status = 'running'",
        )
        .bind(ctx.project_id)
        .bind(short)
        .fetch_all(&proj_pool)
        .await
        .unwrap_or_default();
        let mut killed = 0usize;
        for (pid,) in running {
            if let Some(p) = pid {
                if p > 0 {
                    crate::process_util::kill_pid(p as u32).await;
                    killed += 1;
                }
            }
        }
        let _ = sqlx::query(
            "UPDATE agent_processes SET status = 'stopped', stopped_at = now() \
             WHERE project_id = $1 AND label = $2 AND kind = 'service' AND status = 'running'",
        )
        .bind(ctx.project_id)
        .bind(short)
        .execute(&proj_pool)
        .await;
        return if killed > 0 {
            ServiceActionOutcome::acted(format!("fermati {killed} processi"))
        } else {
            ServiceActionOutcome::noop("nessun processo running da fermare".to_string())
        };
    }

    // START / RESTART: ferma le varianti simili e ri-spawna dalla definizione
    // piu' recente. `acted` = true se lo spawn e' andato a buon fine.
    let _ =
        crate::agent_processes::stop_similar_running_services(ctx.db, ctx.project_id, short).await;

    let def: Option<(String, Option<String>)> = query_as(
        "SELECT command, working_dir FROM agent_processes \
         WHERE project_id = $1 AND label = $2 AND kind = 'service' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ctx.project_id)
    .bind(short)
    .fetch_optional(&proj_pool)
    .await
    .ok()
    .flatten();

    let (command, working_dir) = match def {
        Some(d) => d,
        None => return ServiceActionOutcome::noop(format!("servizio '{short}' non trovato")),
    };
    let cwd = working_dir
        .filter(|w| !w.trim().is_empty())
        .unwrap_or_else(|| ctx.project_root.to_string_lossy().to_string());

    // Web service: alloca+inietta la porta stabile del bucket PRIMA dello spawn
    // (PUNTO UNICO `web_service_port_env`).
    let port_env = match (
        crate::agent_tools::service::looks_like_web_service(&command),
        ctx.port_registry,
    ) {
        (true, Some(registry)) => {
            match super::allocate_port::web_service_port_env(
                ctx.db,
                registry,
                ctx.project_id,
                short,
            )
            .await
            {
                Ok(env) => Some(env),
                // Porta non utilizzabile: NON si avvia. Proseguire senza PORT
                // lascerebbe scegliere la porta al framework, fuori dal bucket.
                Err(e) => return ServiceActionOutcome::noop(e),
            }
        }
        // Web service SENZA registro: non si avvia. Prima cadeva nel catch-all e
        // partiva senza PORT, cioe' lasciando decidere la porta al literal nel
        // sorgente — ed e' la stessa conseguenza che il ramo sopra rifiuta
        // esplicitamente due righe piu' su. La differenza era solo che li'
        // l'errore era visibile e qui il silenzio no.
        //
        // MISURATO il 03/08/2026 su agenda-corsi: `service_control` (tool
        // agente, costruito con `port_registry: None`) avviava il frontend, e
        // `vite.config.ts` ripiegava sul proprio default numerico 26548 — una
        // porta nel frattempo registrata a un ALTRO servizio.
        //
        // Non e' un ripiego mancato: e' un contratto che il chiamante non puo'
        // onorare, e dirlo e' l'unico modo perche' venga riparato invece che
        // aggirato.
        (true, None) => {
            return ServiceActionOutcome::noop(format!(
                "servizio '{short}' non avviato: e' un servizio web e questo percorso non ha \
                 accesso al registro delle porte, quindi PORT non puo' essere iniettato. \
                 Avvialo con run_service (tool agente) o dal pannello Servizi, che allocano \
                 la porta dal bucket del progetto."
            ));
        }
        (false, _) => None,
    };

    match crate::agent_processes::spawn_agent_process(
        ctx.db,
        ctx.project_id,
        None,
        short,
        &command,
        &cwd,
        Some(ctx.project_root.to_path_buf()),
        port_env,
        false, // niente sandbox Docker su Windows
        "service",
        None,
    )
    .await
    {
        Ok(_) => ServiceActionOutcome::acted(format!("servizio '{short}' avviato")),
        Err(e) => ServiceActionOutcome::noop(format!("avvio fallito: {e}")),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SystemdUserBackend: avvolge i primitivi Linux di services.rs / wizard.rs.
// ─────────────────────────────────────────────────────────────────────────────

/// Backend Linux: i servizi di progetto sono unit `systemd --user`, con fallback
/// detached quando il bus utente e' giu' (WSL/container).
#[cfg(not(windows))]
pub struct SystemdUserBackend;

#[cfg(not(windows))]
#[async_trait]
impl ServiceBackend for SystemdUserBackend {
    async fn list(&self, ctx: &ServiceContext<'_>) -> Vec<ServiceEntry> {
        // Avvolge list_services_fallback (fonte dei file unit su disco, indipendente
        // dal bus). Il ramo systemctl completo con diagnosi vive ancora
        // nell'handler HTTP (get_project_services_status): consolidarlo qui e'
        // compito di una wave successiva. Qui usiamo la fonte robusta.
        super::services::list_services_fallback(ctx.slug, ctx.project_root)
            .await
            .iter()
            .map(|v| voce_da_json(v, "detached"))
            .collect()
    }

    async fn start(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_systemd(ctx, short, "start").await
    }

    async fn stop(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_systemd(ctx, short, "stop").await
    }

    async fn restart(&self, ctx: &ServiceContext<'_>, short: &str) -> ServiceActionOutcome {
        control_systemd(ctx, short, "restart").await
    }

    async fn listening_ports(&self) -> Vec<PortListener> {
        // Avvolge read_listening_ports_ss con fallback su /proc (read_listening_ports_proc),
        // come fa gia' port_recovery::listening_ports sul ramo Linux.
        let via_ss = super::services::read_listening_ports_ss().await;
        let raw = match via_ss {
            Ok(v) if !v.is_empty() => v,
            _ => super::services::read_listening_ports_proc(),
        };
        raw.into_iter().map(PortListener::from).collect()
    }

    async fn manager_status(&self) -> ManagerStatus {
        // Riusa la logica di user_manager_unavailable, ma ritornata come stato
        // strutturato (Available vs Unavailable{hint}) invece che come bool
        // (regola M). Interroga il bus con un comando innocuo (`is-system-running`
        // non richiede argomenti unit) e classifica sullo status/stderr strutturato.
        let out = tokio::process::Command::new("systemctl")
            .args([
                "--user",
                "list-units",
                "--type=service",
                "--no-legend",
                "--no-pager",
            ])
            .output()
            .await;
        match out {
            Ok(o) if super::services::user_manager_unavailable(&o) => ManagerStatus::Unavailable {
                hint: super::services::user_manager_hint().to_string(),
            },
            Ok(_) => ManagerStatus::Available,
            // systemctl assente (host senza systemd): equivale a manager non
            // raggiungibile, i servizi girano detached.
            Err(_) => ManagerStatus::Unavailable {
                hint: super::services::user_manager_hint().to_string(),
            },
        }
    }
}

/// Logica start/stop/restart per il backend systemd, estratta da
/// `control_project_service_systemd` in services.rs: prova systemctl e, se il bus
/// utente e' giu', ricade sul detached (`spawn_detached_service`) leggendo l'unit
/// file. Ritorna `acted` in base all'effetto reale (systemctl OK o detached
/// spawnato/killato), MAI dedotto dalla prosa (regola M).
#[cfg(not(windows))]
async fn control_systemd(
    ctx: &ServiceContext<'_>,
    short: &str,
    action: &str,
) -> ServiceActionOutcome {
    let svc_name = super::services::service_unit_name(ctx.slug, short);

    // Pre-start (comportamento degli handler originali su Linux): libera le porte
    // dichiarate nell'unit occupate da processi estranei. No-op per lo stop.
    if matches!(action, "start" | "restart") {
        let _ = super::services::free_ports_for_unit(&svc_name).await;
    }

    let out = tokio::process::Command::new("systemctl")
        .args(["--user", action, &svc_name])
        .output()
        .await;

    // Bus utente giu' -> fallback detached leggendo l'unit file su disco.
    let needs_detached = match &out {
        Ok(o) => super::services::user_manager_unavailable(o),
        Err(_) => true,
    };

    if !needs_detached {
        // systemctl ha risposto: `acted` = successo del comando (segnale
        // strutturato: exit status, non parsing dell'output).
        let ok = out.map(|o| o.status.success()).unwrap_or(false);
        return if ok {
            ServiceActionOutcome::acted(format!("systemctl {action} {svc_name} ok"))
        } else {
            ServiceActionOutcome::noop(format!("systemctl {action} {svc_name} fallito"))
        };
    }

    // Fallback detached: legge l'unit file dalla directory unica.
    let dir = user_systemd_dir();
    let unit_path = format!("{dir}/{svc_name}");
    let content = tokio::fs::read_to_string(&unit_path)
        .await
        .unwrap_or_default();
    let exec_start = super::services::unit_exec_start(&content);
    if exec_start.trim().is_empty() {
        return ServiceActionOutcome::noop(format!(
            "unit file assente o senza ExecStart: {unit_path}"
        ));
    }
    let cwd = {
        let w = super::services::unit_working_dir(&content);
        if w.trim().is_empty() {
            ctx.project_root.to_string_lossy().to_string()
        } else {
            w
        }
    };
    let env_map = super::services::unit_env_map(&content);

    match action {
        "stop" => {
            // pkill sull'ExecStart (stesso criterio di detached_process_running).
            let _ = tokio::process::Command::new("pkill")
                .args(["-f", &exec_start])
                .output()
                .await;
            ServiceActionOutcome::acted("fermato (detached)".to_string())
        }
        // start e restart: spawn_detached_service e' idempotente (pkill del
        // precedente prima di riavviare), quindi copre entrambi.
        _ => match super::wizard::spawn_detached_service(&svc_name, &cwd, &env_map, &exec_start)
            .await
        {
            Ok(log) => ServiceActionOutcome::acted(format!("avviato (detached), log={log}")),
            Err(e) => ServiceActionOutcome::noop(format!("spawn detached fallito: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_from_casi_systemd_principali() {
        assert_eq!(
            ServiceState::normalize_from("active", "running"),
            ServiceState::Running
        );
        assert_eq!(
            ServiceState::normalize_from("active", "exited"),
            ServiceState::Stopped
        );
        assert_eq!(
            ServiceState::normalize_from("activating", "start"),
            ServiceState::Starting
        );
        assert_eq!(
            ServiceState::normalize_from("activating", "auto-restart"),
            ServiceState::Starting
        );
        assert_eq!(
            ServiceState::normalize_from("failed", "failed"),
            ServiceState::Failed
        );
        assert_eq!(
            ServiceState::normalize_from("inactive", "dead"),
            ServiceState::Stopped
        );
        assert_eq!(
            ServiceState::normalize_from("qualcosa", "di-ignoto"),
            ServiceState::Unknown
        );
    }

    #[test]
    fn service_state_serializza_minuscolo() {
        let cases = [
            (ServiceState::Running, "\"running\""),
            (ServiceState::Starting, "\"starting\""),
            (ServiceState::Stopped, "\"stopped\""),
            (ServiceState::Failed, "\"failed\""),
            (ServiceState::Unknown, "\"unknown\""),
        ];
        for (state, expected) in cases {
            let json = serde_json::to_string(&state).expect("serializzazione ServiceState");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn service_action_outcome_mapping() {
        let acted = ServiceActionOutcome::acted("fatto");
        assert!(acted.acted);
        assert_eq!(acted.message, "fatto");

        let noop = ServiceActionOutcome::noop("niente da fare");
        assert!(!noop.acted);
        assert_eq!(noop.message, "niente da fare");

        // Serializzazione: acted deve comparire come booleano strutturato.
        let json = serde_json::to_value(&acted).expect("serializzazione outcome");
        assert_eq!(json.get("acted").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(json.get("message").and_then(|v| v.as_str()), Some("fatto"));
    }

    #[test]
    fn manager_status_serde() {
        let available = serde_json::to_value(ManagerStatus::Available).expect("serde available");
        assert_eq!(
            available.get("status").and_then(|v| v.as_str()),
            Some("available")
        );

        let not_applicable =
            serde_json::to_value(ManagerStatus::NotApplicable).expect("serde not_applicable");
        assert_eq!(
            not_applicable.get("status").and_then(|v| v.as_str()),
            Some("notapplicable")
        );

        let unavailable = serde_json::to_value(ManagerStatus::Unavailable {
            hint: "bus giu'".to_string(),
        })
        .expect("serde unavailable");
        assert_eq!(
            unavailable.get("status").and_then(|v| v.as_str()),
            Some("unavailable")
        );
        assert_eq!(
            unavailable.get("hint").and_then(|v| v.as_str()),
            Some("bus giu'")
        );
    }

    #[test]
    fn service_entry_serializza_i_campi_attesi() {
        let entry = ServiceEntry {
            id: "beauty-book-backend.service".to_string(),
            label: "backend".to_string(),
            state: ServiceState::Running,
            main_pid: Some(4242),
            managed_by: "systemd",
        };
        let json = serde_json::to_value(&entry).expect("serde ServiceEntry");
        assert_eq!(
            json.get("id").and_then(|v| v.as_str()),
            Some("beauty-book-backend.service")
        );
        assert_eq!(json.get("label").and_then(|v| v.as_str()), Some("backend"));
        assert_eq!(json.get("state").and_then(|v| v.as_str()), Some("running"));
        assert_eq!(json.get("main_pid").and_then(|v| v.as_u64()), Some(4242));
        assert_eq!(
            json.get("managed_by").and_then(|v| v.as_str()),
            Some("systemd")
        );
    }

    #[test]
    fn port_listener_da_terna() {
        let l: PortListener = (39555u16, 1234u32, "node".to_string()).into();
        assert_eq!(l.port, 39555);
        assert_eq!(l.pid, 1234);
        assert_eq!(l.program, "node");
    }

    #[test]
    fn user_systemd_dir_termina_con_config_systemd_user() {
        // Non testiamo l'IO: solo che il percorso costruito ha la forma attesa,
        // indipendentemente dal valore di HOME nell'ambiente di test.
        let dir = user_systemd_dir();
        assert!(
            dir.ends_with("/.config/systemd/user"),
            "percorso inatteso: {dir}"
        );
    }
}
