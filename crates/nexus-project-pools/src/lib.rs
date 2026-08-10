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
//! vi aggiunge il layer provisioning+migrazione (lock per-progetto).
//!
//! Anche il REGISTRO dei pool vive solo qui (`pool_or_open`): i due contratti
//! differiscono su COME si apre un pool, non su quanti ne esistono. Finche' ne
//! avevano uno per uno, lo stesso `<slug>_nexus` veniva aperto due volte dallo
//! stesso processo — vedi il commento su `POOLS`.
//!
//! La separazione DB per-progetto e' SEMPRE attiva: il cutover e' chiuso (le
//! tabelle meta `zz_decommissioned_*` sono droppate, mig 0525) e il flag
//! storico `db.project_separation.enabled` e' stato rimosso (mig 0527). Le
//! funzioni instradano sempre al DB `<slug>_nexus` del progetto e NON ritornano
//! MAI il pool meta: DB non provisionato o non apribile -> errore tipizzato.
//! Lo stesso contratto vale dal 2026-07-20 anche per
//! `mcp-core::project_db_routes` (`ProjectDbError`): il vecchio fallback
//! "resiliente" al meta leggeva liste vuote e scriveva sul DB sbagliato.

pub mod sizing;

use std::sync::{Arc, OnceLock};

use sqlx::PgPool;
use uuid::Uuid;

/// Le entita' che la directory di routing (`nexus_data_routing`) sa mappare a
/// un progetto.
///
/// Punto unico del vocabolario (regole L + N): la CHECK della tabella DISCENDE
/// da questo enum, non gli corre accanto. Finche' il kind era una `&str` scritta
/// a mano nei call site, le due liste potevano divergere — e sono divergute.
///
/// MISURATO il 10/08/2026 sul meta-DB vivo: `nexus_data_routing` conteneva 706
/// righe `run` e 187 `session`, e ZERO `message`, `correction`, `feedback` —
/// perche' la CHECK della mig 0496 (30/06/2026) ammetteva i soli due kind
/// esistenti allora, mentre il codice ne ha aggiunti tre. Ogni scrittura di
/// quei tre era respinta dal DB (SQLSTATE 23514) e l'errore finiva in un WARN
/// indistinguibile da un guasto di rete.
///
/// La conseguenza non era cosmetica: `project_data_pool_by_message_from`,
/// `_by_correction_from` e `_by_feedback_from` non trovavano MAI la riga in
/// directory, quindi cadevano sempre sul fallback che itera TUTTI i DB-progetto
/// con una SELECT ciascuno; e il self-healing che avrebbe dovuto rendere O(1)
/// la chiamata successiva riscriveva ogni volta la stessa riga rifiutata. Il
/// fast-path era inerte per costruzione, per 41 giorni.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Session,
    Run,
    Message,
    Correction,
    Feedback,
}

impl EntityKind {
    /// Tutti i kind ammessi. La migrazione che scrive la CHECK e il test che la
    /// verifica leggono DA QUI: aggiungere una variante senza allineare lo
    /// schema fa fallire il test, invece di produrre scritture respinte in
    /// silenzio.
    pub const TUTTI: [EntityKind; 5] = [
        EntityKind::Session,
        EntityKind::Run,
        EntityKind::Message,
        EntityKind::Correction,
        EntityKind::Feedback,
    ];

    /// Identificatore canonico (regola N), quello che finisce in colonna.
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Session => "session",
            EntityKind::Run => "run",
            EntityKind::Message => "message",
            EntityKind::Correction => "correction",
            EntityKind::Feedback => "feedback",
        }
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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

// ── Registro dei pool per-progetto (punto unico, regola L) ───────────────────
// UN pool per database, per tutta la vita del processo.
//
// Qui c'era una `TtlCache` (300s), e mcp-core ne aveva una seconda (600s) per lo
// stesso DB: due strade verso `<slug>_nexus`, ognuna col proprio registro. Il TTL
// era il difetto piu' grave dei due, perche' un pool NON e' un dato che scade —
// e' una risorsa con un ciclo di vita. `TtlCache::get` alla scadenza risponde
// `None` senza rimuovere la entry: il chiamante apriva un pool NUOVO, e il
// vecchio restava vivo finche' l'ultima `PgPool` clonata (che un run tiene per
// tutta la sua durata) non veniva droppata. A crescere non era il singolo pool ma
// il LORO NUMERO, che il tetto per-pool non governa.
//
// Misurato il 2026-07-22 sul cluster app: 50 connessioni per il ruolo `nexus_app`
// su un `rolconnlimit` di 50, TUTTE idle — 15 su un solo database (tre pool), 10
// su altri due (due pool ciascuno). Da li' in poi qualunque apertura di pool
// falliva e il sistema era fermo per intero, non un singolo run.
//
// Il registro non scade: l'unica invalidazione e' esplicita (`forget_pool`, sul
// re-provisioning che cambia la URL del DB).
static POOLS: OnceLock<std::sync::Mutex<std::collections::HashMap<Uuid, Arc<PgPool>>>> =
    OnceLock::new();

/// Lock per-progetto che serializza l'APERTURA. Senza, due task che non trovano
/// il pool in registro aprono entrambi il proprio: e' il fenomeno osservato come
/// coppie di connessioni nate nello stesso istante sullo stesso database.
static OPEN_LOCKS: OnceLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

fn pools() -> &'static std::sync::Mutex<std::collections::HashMap<Uuid, Arc<PgPool>>> {
    POOLS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Pool gia' aperto per il progetto, se c'e'. Non ne apre mai uno.
pub fn cached_pool(project_id: Uuid) -> Option<Arc<PgPool>> {
    pools()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&project_id)
        .cloned()
}

fn open_lock(project_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    OPEN_LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(project_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Punto unico dell'apertura di un pool per-progetto: restituisce quello gia'
/// registrato oppure lo apre UNA volta con `open`, sotto lock per-progetto.
///
/// `open` e' fornito dal chiamante perche' le due strade hanno contratti diversi
/// — questo crate risolve e basta, `mcp-core` provisiona e migra prima — ma il
/// REGISTRO e' uno solo: chi arriva secondo, da qualunque strada, ritrova il pool
/// del primo invece di aprirne un altro verso lo stesso database.
pub async fn pool_or_open<F, Fut, E>(project_id: Uuid, open: F) -> Result<Arc<PgPool>, E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<PgPool, E>>,
{
    if let Some(pool) = cached_pool(project_id) {
        return Ok(pool);
    }
    let lock = open_lock(project_id);
    let _guard = lock.lock().await;
    // Doppio controllo: chi attendeva il lock ritrova il pool aperto dal primo.
    if let Some(pool) = cached_pool(project_id) {
        return Ok(pool);
    }
    let pool = Arc::new(open().await?);
    pools()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_id, Arc::clone(&pool));
    Ok(pool)
}

/// Dimentica il pool di un progetto (re-provisioning: la URL del DB e' cambiata,
/// il pool registrato punta al database sbagliato). Le connessioni si chiudono
/// quando l'ultima clone in uso viene droppata.
pub fn forget_pool(project_id: Uuid) {
    pools()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&project_id);
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
/// Un solo pool per database (`pool_or_open`): due task concorrenti non ne
/// aprono piu' uno ciascuno, e chi arriva dopo — anche dall'altra strada, quella
/// di `mcp-core` che provisiona — ritrova questo.
pub async fn project_data_pool(
    meta: &PgPool,
    project_id: Uuid,
) -> Result<PgPool, ProjectPoolError> {
    if let Some(pool) = cached_pool(project_id) {
        return Ok((*pool).clone());
    }
    let url = resolve_meta_db_url(meta, project_id)
        .await?
        .ok_or(ProjectPoolError::NotProvisioned(project_id))?;
    let pool = pool_or_open(project_id, || async {
        sizing::project_pool_options().connect(&url).await
    })
    .await
    .map_err(|e: sqlx::Error| ProjectPoolError::Connect {
        project_id,
        message: e.to_string(),
    })?;
    Ok((*pool).clone())
}

/// `project_id` di un'entita' dalla directory di routing (`nexus_data_routing`
/// nel meta, mig 0496). `None` se non mappata o su errore di lettura (loggato).
pub async fn project_id_for_entity(
    meta: &PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT project_id FROM nexus_data_routing WHERE entity_kind = $1 AND entity_id = $2",
    )
    .bind(entity_kind.as_str())
    .bind(entity_id)
    .fetch_optional(meta)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            entity_kind = entity_kind.as_str(),
            entity_id = %entity_id,
            error = %e,
            "nexus-project-pools: lettura nexus_data_routing fallita"
        );
        None
    })
}

/// SQLSTATE `check_violation`: lo schema rifiuta il valore, non il DB a rifiutare
/// la connessione. Nessun retry lo sana.
const SQLSTATE_CHECK_VIOLATION: &str = "23514";

/// Esito della registrazione in directory (regola Q: l'esito e' un campo).
///
/// Le due varianti di fallimento NON sono la stessa cosa e hanno rimedi
/// opposti: `SchemaNonAllineato` e' un difetto PERMANENTE — la migrazione che
/// allinea la CHECK non e' stata applicata, e nessun tentativo successivo
/// riuscira' mai — mentre `ScritturaFallita` e' un guasto contingente, che al
/// prossimo giro puo' andare bene. Collassarle in un `()` con lo stesso WARN e'
/// cio' che ha reso invisibile per 41 giorni il rifiuto di tre kind su cinque.
#[derive(Debug)]
pub enum EsitoRegistrazione {
    /// La mappa e' in directory (inserita ora o gia' presente).
    Registrata,
    /// Il DB rifiuta il kind: la CHECK di `nexus_data_routing` non conosce
    /// ancora questo valore. Difetto di schema, non di rete.
    SchemaNonAllineato { kind: EntityKind },
    /// Guasto contingente (connessione, permessi, timeout).
    ScritturaFallita { causa: String },
}

/// Registra la mappa entita' -> progetto nella directory (idempotente).
///
/// Resta BEST-EFFORT per il chiamante — la creazione dell'entita' non deve
/// fallire perche' la directory non ha accettato la riga, ed e' una scelta
/// motivata che qui si conserva. Cio' che cambia e' che l'esito ora e' un
/// VALORE: un `()` non permetteva a nessuno di distinguere "il DB era giu' un
/// istante" da "questo kind non entrera' mai", e le due cose finivano nello
/// stesso WARN. Fonte unica (regola L): il wrapper omonimo di
/// `mcp-core::project_db_routes` vi delega.
pub async fn register_entity_routing(
    meta: &PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    pid: Uuid,
) -> EsitoRegistrazione {
    let esito = sqlx::query(
        "INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(entity_kind.as_str())
    .bind(entity_id)
    .bind(pid)
    .execute(meta)
    .await;

    let Err(e) = esito else {
        return EsitoRegistrazione::Registrata;
    };
    // Segnale strutturato, mai il testo (regola M): il messaggio del vincolo
    // arriva tradotto nella lingua del server.
    let schema_indietro = matches!(
        &e,
        sqlx::Error::Database(db) if db.code().as_deref() == Some(SQLSTATE_CHECK_VIOLATION)
    );
    if schema_indietro {
        tracing::error!(
            entity_kind = entity_kind.as_str(),
            entity_id = %entity_id,
            project_id = %pid,
            error = %e,
            "nexus-project-pools: la CHECK di nexus_data_routing non ammette questo kind. \
             Nessun retry lo sanera': manca la migrazione che allinea lo schema al vocabolario \
             EntityKind. Finche' dura, ogni risoluzione del pool per questo kind paga la \
             scansione di TUTTI i DB-progetto."
        );
        return EsitoRegistrazione::SchemaNonAllineato { kind: entity_kind };
    }
    tracing::warn!(
        entity_kind = entity_kind.as_str(),
        entity_id = %entity_id,
        project_id = %pid,
        error = %e,
        "nexus-project-pools: insert in nexus_data_routing fallito"
    );
    EsitoRegistrazione::ScritturaFallita {
        causa: e.to_string(),
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
    if let Some(pid) = project_id_for_entity(meta, EntityKind::Session, session_id).await {
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
            register_entity_routing(meta, EntityKind::Session, session_id, pid).await;
            return Ok(pool);
        }
    }
    Err(ProjectPoolError::SessionNotFound(session_id))
}
