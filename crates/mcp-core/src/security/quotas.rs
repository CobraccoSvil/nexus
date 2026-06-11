//! Quote risorse per progetto: porte, RAM, disco, container, pool DB.
//!
//! Lette da `nexus_resource_quotas`. La riga con `project_id = '00000000-...'`
//! contiene i default globali, usati per progetti senza override esplicito.
//!
//! Cache 60s tramite `OnceLock<RwLock<HashMap<Uuid, (ResourceQuota, Instant)>>>`.
//! Refresh lazy: la prossima `load_quota` dopo la scadenza ricarica.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

/// UUID sentinella per la riga dei default globali in `nexus_resource_quotas`.
pub const SENTINEL_PROJECT_ID: Uuid = Uuid::nil();

/// TTL della cache per evitare round-trip DB ad ogni allocazione.
const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub struct ResourceQuota {
    pub max_ports: i32,
    pub max_memory_mb: i32,
    pub max_disk_mb: i32,
    pub max_containers: i32,
    pub max_db_pool_size: i32,
}

impl ResourceQuota {
    /// Default conservativo usato come fallback se il DB e' down al primo accesso
    /// e la cache non ha ancora la riga sentinella. NON e' la fonte di verita':
    /// la fonte e' `nexus_resource_quotas` (sezione G CLAUDE.md).
    pub const fn emergency_default() -> Self {
        Self {
            max_ports: 20,
            max_memory_mb: 4096,
            max_disk_mb: 10240,
            max_containers: 5,
            max_db_pool_size: 10,
        }
    }
}

type CacheMap = HashMap<Uuid, (ResourceQuota, Instant)>;
static CACHE: OnceLock<RwLock<CacheMap>> = OnceLock::new();

fn cache() -> &'static RwLock<CacheMap> {
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Carica la quota per un progetto. Strategia:
/// 1. Cache hit fresco → ritorna
/// 2. Cache miss o stale → query DB su `project_id` specifico
/// 3. Nessuna riga per il progetto → query sentinella (default globali)
/// 4. Sentinella mancante (DB appena migrato male) → `emergency_default()` + warn log
pub async fn load_quota(db: &PgPool, project_id: Uuid) -> ResourceQuota {
    // Hot path: cache hit
    {
        let read = cache().read().await;
        if let Some((q, ts)) = read.get(&project_id) {
            if ts.elapsed() < CACHE_TTL {
                return *q;
            }
        }
    }

    // Cache miss o stale: load da DB
    let row = sqlx::query_as::<_, (i32, i32, i32, i32, i32)>(
        "SELECT max_ports, max_memory_mb, max_disk_mb, max_containers, max_db_pool_size \
         FROM nexus_resource_quotas WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let quota = match row {
        Some((mp, mm, md, mc, mp_db)) => ResourceQuota {
            max_ports: mp,
            max_memory_mb: mm,
            max_disk_mb: md,
            max_containers: mc,
            max_db_pool_size: mp_db,
        },
        None => {
            // Fallback: riga sentinella (default globali)
            let sent = sqlx::query_as::<_, (i32, i32, i32, i32, i32)>(
                "SELECT max_ports, max_memory_mb, max_disk_mb, max_containers, max_db_pool_size \
                 FROM nexus_resource_quotas WHERE project_id = $1",
            )
            .bind(SENTINEL_PROJECT_ID)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
            match sent {
                Some((mp, mm, md, mc, mp_db)) => ResourceQuota {
                    max_ports: mp,
                    max_memory_mb: mm,
                    max_disk_mb: md,
                    max_containers: mc,
                    max_db_pool_size: mp_db,
                },
                None => {
                    tracing::warn!(
                        "nexus_resource_quotas vuota (manca anche sentinella): \
                         uso emergency_default. Riapplicare migrazione 0165."
                    );
                    ResourceQuota::emergency_default()
                }
            }
        }
    };

    // Aggiorna cache
    let mut write = cache().write().await;
    write.insert(project_id, (quota, Instant::now()));
    quota
}

/// Conta porte attualmente allocate per il progetto.
async fn count_allocated_ports(db: &PgPool, project_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM nexus_port_allocations WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0)
}

/// Conta container Docker attivi (kind=service in `agent_processes` con status running/starting,
/// che gireranno in container quando sandbox_enabled o hanno project_image).
async fn count_active_containers(db: &PgPool, project_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_processes \
         WHERE project_id = $1 AND status IN ('running', 'starting') AND sandboxed = true",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0)
}

/// Verifica se il progetto puo' allocare un'altra porta. Ritorna `Err` con messaggio
/// human-readable se la quota e' raggiunta.
pub async fn check_can_allocate_port(db: &PgPool, project_id: Uuid) -> Result<(), String> {
    let quota = load_quota(db, project_id).await;
    let used = count_allocated_ports(db, project_id).await;
    if used >= quota.max_ports as i64 {
        return Err(format!(
            "quota porte raggiunta ({used}/{}); rilascia con release_port prima di allocare",
            quota.max_ports
        ));
    }
    Ok(())
}

/// Verifica se il progetto puo' avviare un altro container. Ritorna `Err` se quota raggiunta.
pub async fn check_can_start_container(db: &PgPool, project_id: Uuid) -> Result<(), String> {
    let quota = load_quota(db, project_id).await;
    let used = count_active_containers(db, project_id).await;
    if used >= quota.max_containers as i64 {
        return Err(format!(
            "quota container raggiunta ({used}/{}); ferma servizi inattivi con stop_service",
            quota.max_containers
        ));
    }
    Ok(())
}

/// Cache dell'uso disco per progetto (il `du` ricorsivo e' costoso): TTL piu'
/// lungo della quota perche' lo spazio cambia lentamente.
static DISK_USAGE_CACHE: OnceLock<RwLock<HashMap<Uuid, (i64, Instant)>>> = OnceLock::new();
const DISK_USAGE_TTL: Duration = Duration::from_secs(300);

fn disk_cache() -> &'static RwLock<HashMap<Uuid, (i64, Instant)>> {
    DISK_USAGE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Uso disco stimato della project_root in MB (cache 5 min). `du` in
/// spawn_blocking per non bloccare il runtime tokio. Best-effort: su errore
/// ritorna 0 (non blocchiamo le scritture per un `du` fallito).
async fn project_disk_usage_mb(project_id: Uuid, root_path: &str) -> i64 {
    {
        let guard = disk_cache().read().await;
        if let Some((mb, at)) = guard.get(&project_id) {
            if at.elapsed() < DISK_USAGE_TTL {
                return *mb;
            }
        }
    }
    let root = root_path.to_string();
    let mb = tokio::task::spawn_blocking(move || {
        // `du -sm` ritorna i MB della directory (esclusi i filesystem montati).
        std::process::Command::new("du")
            .args(["-sm", "--", &root])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                } else {
                    None
                }
            })
            .unwrap_or(0)
    })
    .await
    .unwrap_or(0);
    let mut write = disk_cache().write().await;
    write.insert(project_id, (mb, Instant::now()));
    mb
}

/// Verifica se il progetto e' entro la quota disco (`max_disk_mb`). Chiamata
/// prima delle scritture file degli agenti. Best-effort: se non si riesce a
/// stimare l'uso (du fallito -> 0) non blocca. Ritorna `Err` se oltre quota.
pub async fn check_can_use_disk(
    db: &PgPool,
    project_id: Uuid,
    root_path: &str,
) -> Result<(), String> {
    if root_path.trim().is_empty() {
        return Ok(());
    }
    let quota = load_quota(db, project_id).await;
    let used_mb = project_disk_usage_mb(project_id, root_path).await;
    if used_mb >= quota.max_disk_mb as i64 {
        return Err(format!(
            "quota disco raggiunta ({used_mb}/{} MB) per il progetto; libera spazio prima di scrivere",
            quota.max_disk_mb
        ));
    }
    Ok(())
}

/// Invalida la cache uso disco di un progetto (dopo cancellazioni che liberano
/// spazio, per non bloccare scritture legittime fino allo scadere del TTL).
#[expect(
    dead_code,
    reason = "wiring mancante: tool_delete_file (agent_tools/files.rs) deve chiamarla dopo le cancellazioni, altrimenti un progetto a quota piena resta bloccato in scrittura fino al TTL di 5 minuti"
)]
pub async fn invalidate_disk_cache(project_id: Uuid) {
    let mut write = disk_cache().write().await;
    write.remove(&project_id);
}

/// Somma RSS (MB) dei processi attivi del progetto, letti da
/// `/proc/<pid>/statm` (pagine * 4KB). Best-effort: PID morti o /proc
/// inaccessibile contano 0.
async fn project_memory_usage_mb(db: &PgPool, project_id: Uuid) -> i64 {
    let pids: Vec<i32> = sqlx::query_scalar::<_, Option<i32>>(
        "SELECT pid FROM agent_processes \
         WHERE project_id = $1 AND status IN ('running', 'starting') AND pid IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .flatten()
    .collect();
    if pids.is_empty() {
        return 0;
    }
    tokio::task::spawn_blocking(move || {
        let page_kb = 4; // size pagina tipica
        let mut total_kb: i64 = 0;
        for pid in pids {
            if let Ok(statm) = std::fs::read_to_string(format!("/proc/{pid}/statm")) {
                // Campo 2 = resident set size in pagine.
                if let Some(rss_pages) = statm.split_whitespace().nth(1) {
                    if let Ok(pages) = rss_pages.parse::<i64>() {
                        total_kb += pages * page_kb;
                    }
                }
            }
        }
        total_kb / 1024
    })
    .await
    .unwrap_or(0)
}

/// Verifica RAM pre-avvio: la somma RSS dei processi attivi del progetto deve
/// stare sotto `max_memory_mb`. Best-effort (se non misurabile, non blocca).
/// Ritorna `Err` se oltre quota.
pub async fn check_can_use_memory(db: &PgPool, project_id: Uuid) -> Result<(), String> {
    let quota = load_quota(db, project_id).await;
    let used_mb = project_memory_usage_mb(db, project_id).await;
    if used_mb >= quota.max_memory_mb as i64 {
        return Err(format!(
            "quota memoria raggiunta ({used_mb}/{} MB): ferma un servizio attivo prima di avviarne un altro",
            quota.max_memory_mb
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emergency_default_sensible() {
        let q = ResourceQuota::emergency_default();
        assert!(q.max_ports > 0 && q.max_ports <= 50);
        assert!(q.max_memory_mb >= 256);
        assert!(q.max_containers >= 1);
    }
}
