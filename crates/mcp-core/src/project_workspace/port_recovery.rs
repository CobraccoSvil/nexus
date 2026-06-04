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

use super::services::{
    project_bucket_start, read_listening_ports_proc, read_listening_ports_ss,
    PROJECT_PORT_BUCKET_SIZE,
};

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

/// Termina l'intero PROCESS GROUP di `pid` (SIGTERM, poi SIGKILL dopo 500ms se
/// ancora vivo). Cosi' una catena `pnpm dev -> node -> vite` viene fermata per
/// intero e il processo padre non rilancia il figlio che reggeva il listener
/// (causa per cui un kill del solo PID lasciava la porta occupata). Fallback al
/// solo `pid` se il PGID non e' leggibile.
pub(crate) async fn kill_process_tree(pid: u32) {
    let target = match read_pgid(pid).await {
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

/// Lista (port, pid, program) in ascolto. Usa `ss` se disponibile, fallback /proc.
pub async fn listening_ports() -> Vec<(u16, u32, String)> {
    if let Ok(v) = read_listening_ports_ss().await {
        if !v.is_empty() {
            return v;
        }
    }
    tokio::task::spawn_blocking(read_listening_ports_proc)
        .await
        .unwrap_or_default()
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

    let tracked: std::collections::HashSet<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT pid FROM agent_processes WHERE project_id = $1 \
         AND status IN ('running','starting')",
    )
    .bind(project_id)
    .fetch_all(db)
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
