//! `nexus-project-pools` — risoluzione READ-ONLY del pool Postgres del
//! DB metadati per-progetto (`<slug>_nexus`) dove vive il dominio chat/run
//! migrato (cutover separazione DB 2026-07-01, set `db/migrations/project`).
//!
//! Punto unico (regola L) del concern "instradare una query del dominio
//! migrato da un processo che NON e' mcp-core" (admin-service,
//! tool di nexus-tool-kit). Dentro mcp-core il punto unico
//! resta `project_db_routes` (che in piu' PROVISIONA il DB e ne applica le
//! migrazioni al primo accesso): quel contratto non e' replicabile qui perche'
//! il migrator sqlx non e' concurrency-safe cross-processo — due processi che
//! migrano lo stesso DB insieme corrompono `_sqlx_migrations`. Percio' questo
//! crate NON provisiona e NON migra: se il DB del progetto non e' ancora
//! registrato la risoluzione fallisce con errore tipizzato (regola M) e il
//! chiamante degrada esplicitamente (WARN + skip progetto), mai in silenzio.
//!
//! I mattoni comuni ai due contratti — lettura del registry
//! `project_database_config`, elenco progetti, directory `nexus_data_routing`
//! — vivono SOLO qui: `mcp-core::project_db_routes` delega a queste funzioni e
//! vi aggiunge il layer provisioning+migrazione (lock per-progetto) e la
//! propria cache pool condivisa con AppState.
//!
//! La separazione DB per-progetto e' SEMPRE attiva: il cutover e' chiuso (le
//! tabelle meta `zz_decommissioned_*` sono droppate, mig 0525) e il flag
//! storico `db.project_separation.enabled` e' stato rimosso (mig 0527). Le
//! funzioni instradano sempre al DB `<slug>_nexus` del progetto; l'unica via
//! che ritorna il pool meta e' la resilienza (registry non inizializzato o DB
//! non provisionato), mai un ramo di configurazione.

pub mod sizing;

use std::sync::OnceLock;
use std::time::Duration;

use nexus_cache::TtlCache;
use sqlx::PgPool;
use uuid::Uuid;

/// Errore di risoluzione del pool per-progetto. Tipizzato (regola M): i call
/// site decidono la degradazione sul variante, mai sul testo.
#[derive(Debug, thiserror::Error)]
pub enum ProjectPoolError {
    /// Nessuna riga `connection_role='nexus_metadata'` in
    /// `project_database_config`: il DB del progetto non e' mai stato
    /// provisionato da mcp-core (progetto mai usato a flag ON).
    #[error("DB metadati del progetto {0} non provisionato")]
    NotProvisioned(Uuid),

    /// La lettura del registry `project_database_config` sul meta e' fallita.
    #[error("lettura project_database_config per il progetto {project_id} fallita: {message}")]
    Registry { project_id: Uuid, message: String },

    /// Il DB del progetto e' registrato ma il pool non si apre.
    #[error("apertura pool DB metadati del progetto {project_id} fallita: {message}")]
    Connect { project_id: Uuid, message: String },

    /// Ricerca by-session esaurita: la sessione non esiste in alcun DB progetto.
    #[error("sessione {0} non trovata in alcun DB progetto")]
    SessionNotFound(Uuid),
}

static POOLS: OnceLock<TtlCache<Uuid, PgPool>> = OnceLock::new();

fn pool_cache() -> &'static TtlCache<Uuid, PgPool> {
    // TTL 5 min: alla scadenza il pool viene riaperto; l'ultima clone droppata
    // chiude le connessioni. I servizi separati sono a basso QPS, va bene.
    POOLS.get_or_init(|| TtlCache::new(Duration::from_secs(300)))
}

/// Elenco dei `project_id` (tabella globale `projects`, sempre sul meta).
/// Le viste globali aggregano i domini migrati iterando questo elenco.
pub async fn list_project_ids(meta: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects")
        .fetch_all(meta)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "nexus-project-pools: SELECT projects fallita");
            Vec::new()
        })
}

/// URL del DB metadati Nexus del progetto dal registry
/// `project_database_config` (`connection_role='nexus_metadata'`). Fonte unica
/// (regola L) della lettura del registry: anche
/// `mcp-core::project_db_routes` vi delega prima del suo layer provisioning.
pub async fn resolve_meta_db_url(
    meta: &PgPool,
    project_id: Uuid,
) -> Result<Option<String>, ProjectPoolError> {
    let secret: Option<Vec<u8>> = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT connection_secret FROM project_database_config \
         WHERE project_id = $1 AND connection_role = 'nexus_metadata' \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(meta)
    .await
    .map_err(|e| ProjectPoolError::Registry {
        project_id,
        message: e.to_string(),
    })?
    .flatten();
    Ok(secret
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// Pool DOVE risiedono i dati del dominio chat/run per `project_id`: il DB
/// `<slug>_nexus` del progetto (separazione sempre attiva). READ-ONLY: nessun
/// provisioning; DB non registrato -> `Err(NotProvisioned)`.
///
/// Niente lock di provisioning: due task concorrenti possono aprire due pool
/// per lo stesso progetto; uno rimpiazza l'altro in cache e il perdente si
/// chiude alla drop dell'ultima clone. Accettabile per servizi a basso QPS.
pub async fn project_data_pool(
    meta: &PgPool,
    project_id: Uuid,
) -> Result<PgPool, ProjectPoolError> {
    if let Some(pool) = pool_cache().get(&project_id) {
        return Ok(pool);
    }
    let url = resolve_meta_db_url(meta, project_id)
        .await?
        .ok_or(ProjectPoolError::NotProvisioned(project_id))?;
    let pool = sizing::project_pool_options()
        .connect(&url)
        .await
        .map_err(|e| ProjectPoolError::Connect {
            project_id,
            message: e.to_string(),
        })?;
    pool_cache().insert(project_id, pool.clone());
    Ok(pool)
}

/// `project_id` di un'entita' dalla directory di routing (`nexus_data_routing`
/// nel meta, mig 0496). `None` se non mappata o su errore di lettura (loggato).
pub async fn project_id_for_entity(
    meta: &PgPool,
    entity_kind: &str,
    entity_id: Uuid,
) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT project_id FROM nexus_data_routing WHERE entity_kind = $1 AND entity_id = $2",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .fetch_optional(meta)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            entity_kind,
            entity_id = %entity_id,
            error = %e,
            "nexus-project-pools: lettura nexus_data_routing fallita"
        );
        None
    })
}

/// Registra la mappa entita' -> progetto nella directory (idempotente,
/// best-effort: un fallimento e' loggato WARN, mai propagato — la creazione
/// dell'entita' non deve fallire per la directory). Fonte unica (regola L): il
/// wrapper omonimo di `mcp-core::project_db_routes` vi delega.
pub async fn register_entity_routing(meta: &PgPool, entity_kind: &str, entity_id: Uuid, pid: Uuid) {
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(pid)
    .execute(meta)
    .await
    {
        tracing::warn!(
            entity_kind,
            entity_id = %entity_id,
            project_id = %pid,
            error = %e,
            "nexus-project-pools: insert in nexus_data_routing fallito"
        );
    }
}

/// Pool del progetto risolto dal `session_id`: directory di routing (O(1));
/// se la sessione non e' mappata la CERCA nei DB-progetto e auto-registra la
/// mappa (self-healing, stesso pattern di
/// `mcp-core::project_db_routes::project_data_pool_by_session_from`). MAI
/// fallback silenzioso al meta: le tabelle chat sul meta sono decommissionate
/// (mig 0507/0525) e la query fallirebbe comunque.
pub async fn project_data_pool_by_session(
    meta: &PgPool,
    session_id: Uuid,
) -> Result<PgPool, ProjectPoolError> {
    if let Some(pid) = project_id_for_entity(meta, "session", session_id).await {
        return project_data_pool(meta, pid).await;
    }
    for pid in list_project_ids(meta).await {
        let pool = match project_data_pool(meta, pid).await {
            Ok(p) => p,
            Err(ProjectPoolError::NotProvisioned(_)) => continue,
            Err(e) => {
                tracing::warn!(
                    project_id = %pid,
                    error = %e,
                    "nexus-project-pools: pool progetto non risolvibile durante la ricerca by-session"
                );
                continue;
            }
        };
        let found = sqlx::query_scalar::<_, i32>("SELECT 1 FROM chat_sessions WHERE id = $1 LIMIT 1")
            .bind(session_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .is_some();
        if found {
            register_entity_routing(meta, "session", session_id, pid).await;
            return Ok(pool);
        }
    }
    Err(ProjectPoolError::SessionNotFound(session_id))
}
