//! Helper di recovery porte: probe, kill estranei, adozione processi orfani.
//!
//! Risolve due classi di problemi osservati in produzione:
//!  1. `nexus_port_allocations` contiene una riga (project_id, label, port=X)
//!     ma nessuno e' realmente in ascolto su X (il processo che doveva
//!     servirla e' morto o non e' mai stato avviato). I tool agente
//!     ricevono X, tentano connect, falliscono. Soluzione: TCP probe
//!     dell'allocazione prima di restituirla; se fallisce, scan del bucket
//!     del progetto per trovare il processo orfano e "adottarlo".
//!  2. Una porta candidata e' occupata da un PID estraneo (non tracciato in
//!     `agent_processes`). Soluzione: `try_free_port` tenta SIGTERM, poi
//!     SIGKILL dopo 500ms.

use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use super::services::{project_bucket_start, PROJECT_PORT_BUCKET_SIZE};
#[cfg(not(windows))]
use super::services::{read_listening_ports_proc, read_listening_ports_ss};

/// True se la porta accetta una connessione TCP entro `timeout_ms`.
pub async fn tcp_probe(port: u16, timeout_ms: u64) -> bool {
    let addr = format!("127.0.0.1:{port}");
    matches!(
        tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            tokio::net::TcpStream::connect(&addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Legge il process group id (PGID) di `pid` da /proc/{pid}/stat (campo `pgrp`,
/// il 3° dopo la `)` che chiude `comm`). None se non leggibile.
///
/// Solo Unix: i process group POSIX e `/proc` non esistono su Windows, dove la
/// terminazione dell'albero e' gestita da `taskkill /T` (vedi `kill_process_tree`).
#[cfg(unix)]
async fn read_pgid(pid: u32) -> Option<u32> {
    let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
        .await
        .ok()?;
    // Formato: "pid (comm) state ppid pgrp ...". `comm` puo' contenere spazi e
    // parentesi, quindi si parte da dopo l'ULTIMA ')'.
    let after = stat.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse::<u32>().ok()
}

/// Legge il comm (nome processo) di `pid` da /proc/{pid}/comm. None se non
/// leggibile o pid morto. Solo Unix (dipende da `/proc`).
#[cfg(unix)]
async fn read_comm(pid: u32) -> Option<String> {
    let raw = tokio::fs::read_to_string(format!("/proc/{pid}/comm"))
        .await
        .ok()?;
    Some(raw.trim().to_string())
}

/// Legge /proc/{pid}/cmdline (argomenti NUL-separati) come stringa con spazi.
/// Serve a riconoscere i servizi Nexus il cui `comm` e' il runtime generico
/// (brain == "python3", web-ide == "node"): il modulo/entrypoint compare solo
/// nella cmdline completa. None se illeggibile o vuoto (kernel thread / pid morto).
/// Solo Unix (dipende da `/proc`).
#[cfg(unix)]
async fn read_cmdline(pid: u32) -> Option<String> {
    let raw = tokio::fs::read(format!("/proc/{pid}/cmdline")).await.ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string())
}

/// Scansiona /proc e ritorna i `(pid, comm)` dei processi il cui process group
/// (`pgrp`) coincide con `pgid` e il cui `comm` appartiene all'infrastruttura
/// Nexus (mcp-core, brain, gateway, microservizi). Difesa-in-profondita' per
/// `kill_process_tree`: il confronto `target_pgid == own_pgid` (check 2) si basa
/// sul solo `read_pgid(own_pid)`; questa enumerazione usa invece il `comm` REALE
/// di OGNI membro del gruppo, criterio indipendente, e copre il caso in cui un
/// qualunque servizio Nexus condivida il gruppo bersaglio (regola E: mai abbattere
/// l'infrastruttura). Solo Unix (enumera `/proc` e usa i process group POSIX).
#[cfg(unix)]
async fn nexus_processes_in_group(pgid: u32) -> Vec<(u32, String)> {
    let mut hits = Vec::new();
    let mut entries = match tokio::fs::read_dir("/proc").await {
        Ok(e) => e,
        Err(_) => return hits,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let pid: u32 = match entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if read_pgid(pid).await != Some(pgid) {
            continue;
        }
        let comm = read_comm(pid).await;
        // La cmdline si legge solo se il comm non basta gia' a decidere: i servizi
        // il cui comm e' il runtime generico (brain == "python3", web-ide == "node")
        // richiedono il match sull'entrypoint completo.
        let cmdline = read_cmdline(pid).await;
        if is_nexus_process(comm.as_deref(), cmdline.as_deref()) {
            hits.push((pid, comm.or(cmdline).unwrap_or_default()));
        }
    }
    hits
}

/// Punto unico (regola L) testabile senza /proc reale: decide se un processo
/// appartiene all'infrastruttura Nexus dato il suo `comm` e/o la sua `cmdline`.
/// Il `comm` e' il match primario; la cmdline copre i servizi il cui comm e' il
/// runtime generico (brain == "python3", web-ide == "node"), che NON comparirebbero
/// in NEXUS_COMMS — gap che lasciava il brain scoperto dal check #4 anti-suicidio.
/// Solo Unix: usata esclusivamente dalla logica di kill del process group POSIX.
#[cfg(unix)]
fn is_nexus_process(comm: Option<&str>, cmdline: Option<&str>) -> bool {
    const NEXUS_COMMS: &[&str] = &[
        "mcp-core",
        "brain",
        "nexus-gateway",
        "admin-service",
        "doc-service",
        "billing-service",
        "plugin-service",
        "browser-bridge",
    ];
    const NEXUS_CMDLINE: &[&str] = &["brain.grpc_server", "apps/web-ide/server.js"];
    if let Some(comm) = comm {
        let c = comm.to_lowercase();
        if NEXUS_COMMS.iter().any(|n| c.contains(n)) {
            return true;
        }
    }
    if let Some(cmdline) = cmdline {
        let cl = cmdline.to_lowercase();
        if NEXUS_CMDLINE.iter().any(|n| cl.contains(n)) {
            return true;
        }
    }
    false
}

/// Termina l'intero PROCESS GROUP di `pid` (SIGTERM, poi SIGKILL dopo 500ms se
/// ancora vivo). Cosi' una catena `pnpm dev -> node -> vite` viene fermata per
/// intero e il processo padre non rilancia il figlio che reggeva il listener
/// (causa per cui un kill del solo PID lasciava la porta occupata). Fallback al
/// solo `pid` se il PGID non e' leggibile.
///
/// SAFETY NET STRUTTURALI (incidente 2026-06-05 22:51): un PID di dev-server
/// puo' essere stato riciclato dal kernel dopo la morte del processo originale.
/// Se nel frattempo il kernel ha riassegnato quel PID a un thread di mcp-core
/// (LWP), `read_pgid(pid)` ritorna il PGID di mcp-core e `kill -TERM -<PGID>`
/// uccide mcp-core stesso (suicidio osservato in produzione). Tre check
/// difensivi prima di inviare segnali:
///   1. pid != own_pid: mai uccidere se stessi (banale ma necessario).
///   2. pgid != own_pgid: se il process group coincide con quello di mcp-core
///      siamo certi che il PID e' stato riciclato per un thread del padre,
///      o che il chiamante e' stato chiamato per errore (es. cleanup_duplicate
///      che ha trovato un proprio thread). Abort.
///   3. comm != mcp-core: ultima difesa di identita' — se il nome del processo
///      e' "mcp-core" il PID e' stato riciclato per il binario stesso, abort.
///   4. nessun servizio Nexus nel gruppo bersaglio: enumerazione EFFETTIVA dei
///      membri del process group (comm reale, non solo confronto pgid del check 2);
///      se contiene mcp-core/brain/gateway/microservizi, abort (incidente
///      2026-06-06: una catena dev-server scambiata per duplicati portava a un
///      kill di gruppo che abbatteva mcp-core).
#[cfg(unix)]
pub(crate) async fn kill_process_tree(pid: u32) {
    let own_pid = std::process::id();

    // Check 1: mai uccidere il proprio PID. Banale ma essenziale.
    if pid == own_pid {
        tracing::error!(
            target = "port_recovery",
            pid,
            "kill_process_tree: RIFIUTO di uccidere il proprio PID (mcp-core)"
        );
        return;
    }

    // Check 3: verifica identita' tramite /proc/<pid>/comm. Se il PID e' stato
    // riciclato per un thread di mcp-core, comm conterra' "mcp-core" e abort.
    // Se il processo e' morto (comm illeggibile), procedi: nessun gruppo da
    // colpire, il fallback `kill <pid>` sara' un no-op.
    if let Some(comm) = read_comm(pid).await {
        if comm == "mcp-core" {
            tracing::error!(
                target = "port_recovery",
                pid,
                comm = %comm,
                "kill_process_tree: RIFIUTO — il PID e' stato riciclato per mcp-core stesso"
            );
            return;
        }
    }

    let pgid_opt = read_pgid(pid).await;

    // Check 2: se il PGID del target coincide col PGID di mcp-core, il PID
    // appartiene allo stesso gruppo del padre (caso patologico: PID riciclato
    // o spawn senza process_group(0)). Abort prima di toccare il gruppo.
    if let Some(target_pgid) = pgid_opt {
        let own_pgid = read_pgid(own_pid).await;
        if Some(target_pgid) == own_pgid {
            tracing::error!(
                target = "port_recovery",
                pid,
                target_pgid,
                own_pid,
                own_pgid = ?own_pgid,
                "kill_process_tree: RIFIUTO — il PGID del target coincide col gruppo di mcp-core (PID riciclato o spawn senza setsid)"
            );
            return;
        }

        // Check 4 (difesa-in-profondita'): enumerazione EFFETTIVA dei membri del
        // process group bersaglio. Il check 2 confronta solo `read_pgid(own_pid)`;
        // se un QUALSIASI servizio Nexus (non solo mcp-core) condividesse il gruppo
        // bersaglio, il `kill -TERM -<pgid>` lo abbatterebbe. Verifica il comm reale
        // dei membri e abort se trova infrastruttura Nexus (regola E, anti-suicidio).
        if target_pgid > 1 {
            let nexus_in_group = nexus_processes_in_group(target_pgid).await;
            if !nexus_in_group.is_empty() {
                tracing::error!(
                    target = "port_recovery",
                    pid,
                    target_pgid,
                    nexus_members = ?nexus_in_group,
                    "kill_process_tree: RIFIUTO — il process group bersaglio contiene infrastruttura Nexus (anti-suicidio, regola E)"
                );
                return;
            }
        }
    }

    let target = match pgid_opt {
        // Il `-` davanti al PGID dice a kill(1) di colpire l'intero gruppo.
        Some(pgid) if pgid > 1 => format!("-{pgid}"),
        _ => pid.to_string(),
    };
    let _ = tokio::process::Command::new("kill")
        .args(["-TERM", &target])
        .output()
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    if std::path::Path::new(&format!("/proc/{pid}")).exists() {
        let _ = tokio::process::Command::new("kill")
            .args(["-KILL", &target])
            .output()
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Termina l'intero albero di processi di `pid` su Windows.
///
/// Non esistono i process group POSIX ne' `/proc`: `taskkill /T /F` termina il
/// processo E l'intera discendenza (equivalente affidabile del kill del gruppo,
/// necessario per fermare catene `pnpm dev -> node -> vite` che altrimenti
/// rilanciano il figlio col listener). `/F` forza (nessun graceful step: su
/// Windows non c'e' un SIGTERM intermedio applicabile a tutto l'albero).
///
/// Anti-suicidio minimo (regola E): mai toccare pid 0 ne' il proprio PID. I check
/// difensivi PGID/comm della variante Unix non hanno equivalente qui (nessun
/// `/proc`), ma `taskkill` colpisce solo l'albero del PID indicato, non un gruppo
/// condiviso, quindi il rischio di abbattere l'infrastruttura Nexus e' assente
/// finche' non si passa un PID Nexus (i chiamanti a monte gia' escludono own_pid
/// e i PID tracciati).
#[cfg(windows)]
pub(crate) async fn kill_process_tree(pid: u32) {
    if pid == 0 || pid == std::process::id() {
        tracing::error!(
            target = "port_recovery",
            pid,
            "kill_process_tree: RIFIUTO di uccidere pid 0 o il proprio PID (mcp-core)"
        );
        return;
    }
    let _ = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .await;
}

/// Lista (port, pid, program) in ascolto. PUNTO UNICO cross-platform (regola L):
/// Unix usa `ss` con fallback /proc; Windows delega a `windows_listening_ports`
/// (Get-NetTCPConnection). Prima del dispatch #[cfg(windows)] questa funzione
/// ritornava SEMPRE vuoto su Windows: try_free_port non risolveva mai il PID
/// occupante, scan_bucket_orphans era cieco e la guardia RefuseActive mai
/// attiva (incidente Beaty-Book 2026-07-02: 6 node orfani, EADDRINUSE ricorrente).
pub async fn listening_ports() -> Vec<(u16, u32, String)> {
    #[cfg(windows)]
    {
        super::services::windows_listening_ports().await
    }
    #[cfg(not(windows))]
    {
        if let Ok(v) = read_listening_ports_ss().await {
            if !v.is_empty() {
                return v;
            }
        }
        tokio::task::spawn_blocking(read_listening_ports_proc)
            .await
            .unwrap_or_default()
    }
}

/// `(pid, program)` del processo in LISTEN su `port`, se risolvibile. Delega a
/// `listening_ports` (punto unico): nessun call site deve ri-implementare la
/// domanda "chi occupa questa porta".
pub async fn port_occupant(port: u16) -> Option<(u32, String)> {
    listening_ports()
        .await
        .into_iter()
        .find(|(p, _, _)| *p == port)
        .map(|(_, pid, program)| (pid, program))
}

/// True se `pid` e' un processo GOVERNATO dal progetto: presente in
/// `agent_processes` con status running/starting. Un occupante tracciato non va
/// mai killato per liberare una porta (e' il servizio legittimo); uno non
/// tracciato e' un orfano/avvio manuale.
pub async fn is_tracked_pid(db: &PgPool, project_id: Uuid, pid: u32) -> bool {
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_processes WHERE project_id = $1 AND pid = $2 \
         AND status IN ('running','starting')",
    )
    .bind(project_id)
    .bind(pid as i64)
    .fetch_one(&proj_pool)
    .await
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Predicato PURO (testabile senza processi reali): un processo e' davvero
/// fermo quando il PID (wrapper) non e' piu' vivo E la porta associata — se
/// nota — non e' piu' in LISTEN. `port_listening = None` significa "nessuna
/// porta governata da verificare".
pub(crate) fn process_fully_stopped(wrapper_alive: bool, port_listening: Option<bool>) -> bool {
    !wrapper_alive && port_listening != Some(true)
}

/// Verifica POST-KILL con retry breve che un processo sia davvero terminato:
/// PID morto e porta della label libera. Ritorna true solo a verifica riuscita.
///
/// Causa radice coperta (incidente Beaty-Book 2026-07-02): su Windows il PID
/// registrato e' il wrapper bash e `taskkill /T` risolve i discendenti dalla
/// catena dei parent AL MOMENTO del kill; se un intermedio (npm/cmd) e' gia'
/// uscito, il node col listener resta orfano e sopravvive con la porta occupata
/// mentre il DB dichiarava gia' 'stopped' (EADDRINUSE al riavvio successivo).
/// Qui, se la porta resta in LISTEN dopo una breve grazia, si risale al VERO
/// occupante via `try_free_port` (lookup per porta, non per albero) e si
/// continua a verificare fino al timeout (~3s).
pub async fn ensure_process_stopped(pid: Option<u32>, port: Option<u16>) -> bool {
    const ATTEMPTS: u32 = 10;
    const STEP_MS: u64 = 300;
    // Tentativo (0-based) dopo il quale, a porta ancora occupata, si prova UNA
    // volta a liberare la porta risalendo al PID reale dell'occupante.
    const FREE_PORT_AFTER_ATTEMPT: u32 = 2;

    let mut free_attempted = false;
    for attempt in 0..ATTEMPTS {
        let wrapper_alive = pid.map(crate::process_util::process_alive).unwrap_or(false);
        let port_listening = match port {
            Some(p) => Some(tcp_probe(p, 200).await),
            None => None,
        };
        if process_fully_stopped(wrapper_alive, port_listening) {
            return true;
        }
        if let (Some(true), Some(p)) = (port_listening, port) {
            if !free_attempted && attempt >= FREE_PORT_AFTER_ATTEMPT {
                free_attempted = true;
                tracing::warn!(
                    port = p,
                    pid = ?pid,
                    "ensure_process_stopped: porta ancora in LISTEN dopo il kill, risalgo all'occupante reale via try_free_port"
                );
                let _ = try_free_port(p).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(STEP_MS)).await;
    }
    false
}

/// Tenta di liberare `port` terminando il processo in ascolto (SIGTERM, poi
/// SIGKILL dopo 500ms se ancora vivo). Salta se il listener e' lo stesso mcp-core
/// o se non e' possibile risalire al PID. Ritorna true se la porta risulta poi
/// libera (bind possibile).
pub async fn try_free_port(port: u16) -> bool {
    let own_pid = std::process::id();
    let listeners = listening_ports().await;
    let target = listeners.into_iter().find(|(p, _, _)| *p == port);

    if let Some((_, pid, program)) = target {
        if pid == 0 || pid == own_pid {
            return false;
        }
        tracing::warn!(
            port,
            pid,
            program = %program,
            "try_free_port: termino il process group del PID che occupa la porta"
        );
        kill_process_tree(pid).await;
    }

    tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .is_ok()
}

/// Lista (port, pid, program) in ascolto nel bucket del progetto, escludendo
/// PID gia' tracciati in `agent_processes` (status running/starting) e il PID
/// di mcp-core stesso. Sono "orfani candidati" all'adozione o al cleanup.
pub async fn scan_bucket_orphans(db: &PgPool, project_id: Uuid) -> Vec<(u16, u32, String)> {
    let start = project_bucket_start(&project_id);
    let end = start
        .saturating_add(PROJECT_PORT_BUCKET_SIZE)
        .saturating_sub(1);

    // Routing separazione DB: `agent_processes` e' una tabella migrata, va letta
    // sul pool del progetto (a flag OFF ritorna il meta-pool, comportamento storico).
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let tracked: std::collections::HashSet<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT pid FROM agent_processes WHERE project_id = $1 \
         AND status IN ('running','starting')",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .flatten()
    .collect();

    let own_pid = std::process::id() as i64;
    listening_ports()
        .await
        .into_iter()
        .filter(|(p, pid, _)| {
            *p >= start
                && *p <= end
                && *pid != 0
                && (*pid as i64) != own_pid
                && !tracked.contains(&(*pid as i64))
        })
        .collect()
}

/// Euristica per riconoscere se un processo (`program` da ss/proc) e'
/// plausibilmente un web server / API server adottabile come servizio di
/// progetto.
pub fn looks_like_server_process(program: &str) -> bool {
    let s = program.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "node",
        "next",
        "vite",
        "nuxt",
        "remix",
        "deno",
        "bun",
        "esbuild",
        "python",
        "uvicorn",
        "gunicorn",
        "hypercorn",
        "fastapi",
        "flask",
        "ruby",
        "rails",
        "puma",
        "unicorn",
        "php",
        "php-fpm",
        "go",
        "main",
        "cargo",
        "rustc",
        "java",
        "tomcat",
        "jetty",
        "dotnet",
        "nginx",
        "apache",
        "caddy",
    ];
    KEYWORDS.iter().any(|kw| s.contains(kw))
}

/// Termina tutti i processi del bucket del progetto che NON sono tracciati in
/// `agent_processes` running/starting. Usato dal cleanup esplicito utente per
/// evitare proliferazione. Ritorna i PID effettivamente terminati.
pub async fn kill_bucket_orphans(db: &PgPool, project_id: Uuid) -> Vec<u32> {
    let orphans = scan_bucket_orphans(db, project_id).await;
    let mut killed = Vec::new();
    for (port, pid, program) in orphans {
        if !looks_like_server_process(&program) {
            continue;
        }
        tracing::warn!(
            project_id = %project_id, port, pid, program = %program,
            "kill_bucket_orphans: termino il process group dell'orfano del bucket"
        );
        kill_process_tree(pid).await;
        killed.push(pid);
    }
    killed
}

#[cfg(test)]
mod stop_verification_tests {
    use super::process_fully_stopped;

    /// La verifica post-kill richiede ENTRAMBE le condizioni: PID morto E porta
    /// libera. Il vecchio flusso (marca 'stopped' e taskkill fire-and-forget)
    /// dichiarava fermo un servizio il cui figlio node era ancora in LISTEN.
    #[test]
    fn fermo_solo_se_pid_morto_e_porta_libera() {
        // Wrapper morto, porta libera: fermo.
        assert!(process_fully_stopped(false, Some(false)));
        // Wrapper morto ma nessuna porta da verificare: fermo.
        assert!(process_fully_stopped(false, None));
        // Wrapper morto ma porta ANCORA in LISTEN (discendente orfano): NON fermo.
        assert!(!process_fully_stopped(false, Some(true)));
        // Wrapper ancora vivo: NON fermo, qualunque sia la porta.
        assert!(!process_fully_stopped(true, None));
        assert!(!process_fully_stopped(true, Some(false)));
        assert!(!process_fully_stopped(true, Some(true)));
    }

    #[tokio::test]
    async fn senza_pid_ne_porta_e_gia_fermo() {
        // Processo mai partito (pid NULL) e nessuna porta governata: la verifica
        // riesce subito, senza attese.
        assert!(super::ensure_process_stopped(None, None).await);
    }

    #[tokio::test]
    async fn processo_reale_terminato_supera_la_verifica() {
        // Spawn di un processo inerte di lunga durata, kill via punto unico
        // process_util::kill_pid, poi verifica. Idempotente: il processo e'
        // creato e distrutto interamente dal test.
        #[cfg(windows)]
        let child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 > NUL"])
            .spawn();
        #[cfg(unix)]
        let child = std::process::Command::new("sleep").arg("30").spawn();
        let mut child = child.expect("spawn del processo di test");
        let pid = child.id();
        assert!(crate::process_util::process_alive(pid));

        crate::process_util::kill_pid(pid).await;
        // Reap + rilascio dell'handle: in produzione lo fa il task monitor con
        // child.wait().await. Senza, su Unix il PID resta zombie in /proc e su
        // Windows l'oggetto processo resta apribile finche' l'handle e' vivo.
        let _ = child.wait();
        drop(child);

        assert!(super::ensure_process_stopped(Some(pid), None).await);
    }
}

// `is_nexus_process` esiste solo su Unix: i test relativi vanno compilati solo li'.
#[cfg(all(test, unix))]
mod is_nexus_process_tests {
    use super::is_nexus_process;

    #[test]
    fn comm_match_diretto() {
        assert!(is_nexus_process(Some("mcp-core"), None));
        assert!(is_nexus_process(Some("nexus-gateway"), Some("/usr/bin/nexus-gateway")));
    }

    /// Regressione: il brain gira come `python3 -m brain.grpc_server.main`, quindi
    /// /proc/<pid>/comm == "python3" (NON "brain"). Il match deve scattare sulla
    /// cmdline, altrimenti il check #4 anti-suicidio non lo protegge dal kill.
    #[test]
    fn brain_riconosciuto_via_cmdline_non_comm() {
        assert!(!is_nexus_process(Some("python3"), None));
        assert!(is_nexus_process(
            Some("python3"),
            Some("/usr/bin/python3 -m brain.grpc_server.main --rest")
        ));
    }

    /// Il web-ide Next standalone gira come `node .../server.js` (comm == "node").
    #[test]
    fn web_ide_riconosciuto_via_cmdline() {
        assert!(is_nexus_process(
            Some("node"),
            Some("/usr/bin/node /home/administrator/ideai/apps/web-ide/server.js")
        ));
    }

    /// Un dev-server di progetto (es. vite) NON e' infrastruttura Nexus: deve
    /// restare uccidibile dal cleanup, altrimenti il GC delle porte non funziona.
    #[test]
    fn dev_server_progetto_non_e_nexus() {
        assert!(!is_nexus_process(
            Some("node"),
            Some("node /home/administrator/projects/beauty-book/node_modules/.bin/vite")
        ));
        assert!(!is_nexus_process(None, None));
    }
}
