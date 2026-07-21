//! Provisioning database del progetto (mode internal/external).
//!
//! Route:
//!   POST /api/projects/:id/db/provision -> provision_project_db
//!
//! Regola G: host/porta/credenziali del cluster app provengono esclusivamente
//! dai settings nexus_app_db_* / nexus_app_admin_*, mai hardcoded a runtime.

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::config::{set_project_db_config, SetDbConfigBody};
use super::connection::{test_project_db_connection, TestConnectionBody};
use super::shared::{api_err, pg_physical_target, ApiResult};
use crate::{auth::Claims, AppState};

/// Ruolo di una connessione registrata in `project_database_config` (Fase 0
/// separazione DB, regola L): distingue il DB applicativo dell'utente da quello
/// dei metadati Nexus per-progetto. Determina suffisso del nome e visibilita'.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DbRole {
    /// DB applicativo dell'utente (`<slug>_app`), visibile nel pannello SQL.
    App,
    /// DB interno Nexus per-progetto (`<slug>_nexus`): chat/run/costi, mai esposto.
    NexusMetadata,
}

impl DbRole {
    /// Suffisso del nome fisico del database per questo ruolo.
    fn suffix(self) -> &'static str {
        match self {
            DbRole::App => "_app",
            DbRole::NexusMetadata => "_nexus",
        }
    }
    /// Valore della colonna `connection_role`. Punto unico (regola L): i call
    /// site che filtrano le righe per ruolo lo bindano invece di ripetere il
    /// letterale (vedi `config.rs`, dove la riga `nexus_metadata` va nascosta
    /// all'utente e, soprattutto, NON contata dai guard).
    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            DbRole::App => "app",
            DbRole::NexusMetadata => "nexus_metadata",
        }
    }
}

/// Corpo richiesta per `POST /api/projects/:id/db/provision`.
///
/// Provisiona davvero un database per il progetto:
///   - mode="internal": Nexus crea un Postgres isolato nel cluster app
///     (settings nexus_app_db_*), nessuna credenziale richiesta all'utente.
///   - mode="external": l'utente fornisce una connection_string verso un DB
///     gia' esistente; viene testata e registrata.
#[derive(Debug, Deserialize)]
pub struct ProvisionDbBody {
    /// "internal" | "external".
    pub mode: String,
    /// Nome logico della connessione registrata; default "primary".
    pub name: Option<String>,
    /// Nome del database fisico da creare (solo mode=internal); default slug_app.
    pub db_name: Option<String>,
    /// Engine; default "postgres". Internal supporta solo postgres.
    pub engine: Option<String>,
    /// Stringa di connessione verso il DB esterno (richiesta se mode=external).
    pub connection_string: Option<String>,
}

/// Legge un setting con fallback conservativo se assente o su errore DB.
///
/// PUNTO UNICO (regola L) della lettura settings-con-default: delega qui anche
/// `agent_tools::command::ensure_project_db_url`, che ne teneva una copia
/// byte-identica (`load_setting_or`). La query SQL NON vive qui: entrambe le
/// copie la re-implementavano, mentre il punto unico della lettura settings e'
/// `nexus_auth::get_setting` (catalogo ADR 0026).
///
/// Si usa `get_setting_checked` e non `get_setting` per PRESERVARE la semantica
/// dei call site: `get_setting` fa trim e scarta i valori vuoti, e un trim su
/// `nexus_app_db_password` altererebbe una password con spazi significativi.
/// Qui il valore resta raw; il default scatta solo se la riga manca o il DB
/// fallisce, esattamente come nelle due copie precedenti.
pub(crate) async fn load_app_db_setting(db: &sqlx::PgPool, key: &str, default: &str) -> String {
    nexus_auth::get_setting_checked(db, key)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_string())
}

/// Sanitizza un identificatore in caratteri ammessi per nome database Postgres.
fn sanitize_db_ident(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Deriva il nome fisico del database dallo slug del progetto (sanitizzato) e
/// dal `role` (`_app` o `_nexus`).
///
/// PUNTO UNICO (regola L) della derivazione del nome DB fisico: delega qui anche
/// `agent_tools::command::ensure_project_db_url`, che ne teneva una copia
/// (`sanitize_app_db_name`). Le due copie erano identiche riga per riga TRANNE il
/// troncamento — 52 qui, 56 la', entrambi sotto il NAMEDATALEN di Postgres (63) —
/// quindi per uno slug sanificato oltre i 52 caratteri il pannello REST e il tool
/// agente derivavano due nomi diversi e creavano DUE database fisici per lo stesso
/// progetto, senza alcun errore visibile. La derivazione DEVE restare pura e
/// suffix-aware: il budget si calcola sul suffisso effettivo, altrimenti `_nexus`
/// (6 char) e `_app` (4) sforerebbero in modo diverso.
pub(crate) fn derive_project_db_name(slug: Option<&str>, project_id: Uuid, role: DbRole) -> String {
    let suffix = role.suffix();
    let base = slug
        .map(|s| s.to_string())
        .unwrap_or_else(|| project_id.simple().to_string());
    let mut sanitized: String = sanitize_db_ident(&base);
    if sanitized.is_empty() {
        sanitized = project_id.simple().to_string();
    }
    if sanitized.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        sanitized.insert(0, 'p');
    }
    // Tronca lasciando spazio al suffisso, cosi' il nome totale resta valido.
    let max_base = 56_usize.saturating_sub(suffix.len());
    if sanitized.len() > max_base {
        sanitized.truncate(max_base);
    }
    format!("{sanitized}{suffix}")
}

pub async fn provision_project_db(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<ProvisionDbBody>,
) -> ApiResult {
    // Owner check (punto unico, regola L).
    super::shared::ensure_project_owner(&state.db, project_id, &claims).await?;

    let raw_name = body.name.as_deref().unwrap_or("primary").trim().to_string();
    let name = if raw_name.is_empty() {
        "primary".to_string()
    } else {
        raw_name
    };
    let mode = body.mode.trim().to_lowercase();

    match mode.as_str() {
        "internal" => {
            provision_internal(
                &state,
                project_id,
                &name,
                body.engine.as_deref(),
                body.db_name.as_deref(),
            )
            .await
        }
        "external" => {
            let conn = body
                .connection_string
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    api_err(
                        StatusCode::BAD_REQUEST,
                        "connection_string richiesta per mode=external",
                    )
                })?
                .to_string();

            let engine = body
                .engine
                .as_deref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if conn.starts_with("mysql") {
                        "mysql".into()
                    } else if conn.starts_with("sqlite") {
                        "sqlite".into()
                    } else {
                        "postgres".into()
                    }
                });

            // Riusa la logica di test esistente per validare prima di salvare.
            let test = test_project_db_connection(
                State(state.clone()),
                Extension(claims.clone()),
                AxumPath(project_id),
                Json(TestConnectionBody {
                    engine: Some(engine.clone()),
                    connection_string: Some(conn.clone()),
                    name: None,
                    connection_id: None,
                }),
            )
            .await?;

            let test_val = test.0;
            if !test_val
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Ok(Json(json!({
                    "ok": false,
                    "mode": "external",
                    "error": test_val
                        .get("error")
                        .cloned()
                        .unwrap_or_else(|| json!("Connessione fallita")),
                })));
            }

            // Test superato: salva la configurazione (riusa set_project_db_config).
            // Il Json di risposta non serve: questo handler costruisce il suo.
            let _ = set_project_db_config(
                State(state.clone()),
                Extension(claims),
                AxumPath(project_id),
                Json(SetDbConfigBody {
                    engine: Some(engine.clone()),
                    hosting_mode: Some("external".to_string()),
                    migration_tool: None,
                    migration_path: None,
                    allow_ddl_override: None,
                    connection_string: Some(conn),
                    name: Some(name.clone()),
                    is_primary: None,
                }),
            )
            .await?;

            Ok(Json(json!({
                "ok": true,
                "mode": "external",
                "name": name,
                "engine": engine,
                "server_version": test_val.get("server_version").cloned().unwrap_or(json!(null)),
                "table_count": test_val.get("table_count").cloned().unwrap_or(json!(null)),
            })))
        }
        other => Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("mode non valido: {other} (attesi internal | external)"),
        )),
    }
}

/// mode=internal: CREATE DATABASE idempotente nel cluster app piu registrazione.
async fn provision_internal(
    state: &AppState,
    project_id: Uuid,
    name: &str,
    engine_in: Option<&str>,
    db_name_in: Option<&str>,
) -> ApiResult {
    match provision_internal_core(
        &state.db,
        project_id,
        name,
        engine_in,
        db_name_in,
        DbRole::App,
    )
    .await
    {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// Core riusabile del provisioning interno (regola H): condiviso tra l endpoint
/// REST `provision_project_db` (mode=internal) e il tool agente
/// `nexus_db_provision`. Crea il DB nel cluster app dedicato (settings
/// nexus_app_db_*, mai i container ideai-* di Nexus) e registra la connessione
/// in project_database_config. Non richiede credenziali esterne.
pub async fn provision_internal_core(
    db: &sqlx::PgPool,
    project_id: Uuid,
    name: &str,
    engine_in: Option<&str>,
    db_name_in: Option<&str>,
    role: DbRole,
) -> Result<Value, String> {
    let engine = engine_in.unwrap_or("postgres").trim().to_lowercase();
    if engine != "postgres" {
        return Err("Il provisioning interno supporta solo engine=postgres".to_string());
    }

    let slug: Option<String> =
        sqlx::query_scalar("SELECT slug FROM projects WHERE id = $1 LIMIT 1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    let db_name = match db_name_in.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(explicit) => {
            let cleaned = sanitize_db_ident(explicit);
            if cleaned.is_empty() {
                derive_project_db_name(slug.as_deref(), project_id, role)
            } else {
                cleaned
            }
        }
        None => derive_project_db_name(slug.as_deref(), project_id, role),
    };

    let host = load_app_db_setting(db, "nexus_app_db_host", "localhost").await;
    let port = load_app_db_setting(db, "nexus_app_db_port", "5434").await;
    let user = load_app_db_setting(db, "nexus_app_db_user", "nexus_app").await;
    let pwd = load_app_db_setting(db, "nexus_app_db_password", "nexus_app_dev_secret").await;
    let admin_user = load_app_db_setting(db, "nexus_app_admin_user", "nexus_admin").await;
    let admin_pwd = load_app_db_setting(db, "nexus_app_admin_password", "nexus_admin_secret").await;

    let admin_url = format!("postgresql://{admin_user}:{admin_pwd}@{host}:{port}/postgres");
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&admin_url)
        .await
        .map_err(|e| format!("Impossibile connettersi al cluster Postgres dedicato ({host}:{port}): {e}. Verifica i settings nexus_app_db_* e che il servizio sia attivo."))?;

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&db_name)
            .fetch_one(&admin_pool)
            .await
            .unwrap_or(false);

    let mut created = false;
    if !exists {
        let create_sql = format!(
            "CREATE DATABASE \"{}\" OWNER \"{}\" TEMPLATE template0",
            db_name, user
        );
        if let Err(e) = sqlx::query(&create_sql).execute(&admin_pool).await {
            admin_pool.close().await;
            return Err(format!("CREATE DATABASE \"{db_name}\" fallita: {e}"));
        }
        created = true;
        tracing::info!(
            "provision_project_db: created db={} owner={} project_id={}",
            db_name,
            user,
            project_id
        );
    }
    admin_pool.close().await;

    let url = format!("postgresql://{user}:{pwd}@{host}:{port}/{db_name}");

    let new_target = pg_physical_target(&url);

    let mut tx = db.begin().await.map_err(|e| e.to_string())?;

    // ── Idempotenza (regola H): se esiste gia' una connessione del progetto che
    // punta allo STESSO database fisico (host+port+dbname), RIUSALA invece di
    // crearne una seconda con nome diverso. Confrontiamo i target risolti
    // decifrando `connection_secret` (qui e' la URL in chiaro, stessa logica di
    // resolve_project_conn). Evita la duplicazione osservata su beauty_book_app.
    let existing_rows = sqlx::query(
        "SELECT name, connection_secret, is_primary FROM project_database_config WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let existing_count = existing_rows.len() as i64;

    let mut reused_name: Option<String> = None;
    if let Some(ref target) = new_target {
        for r in &existing_rows {
            let secret: Option<Vec<u8>> = r.try_get("connection_secret").unwrap_or(None);
            let existing_url = secret
                .and_then(|b| String::from_utf8(b).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(eu) = existing_url {
                if pg_physical_target(&eu).as_ref() == Some(target) {
                    reused_name = Some(r.try_get::<String, _>("name").unwrap_or_default());
                    break;
                }
            }
        }
    }

    // Nome effettivo della connessione: quella riusata (se esiste) o il nome
    // richiesto. Cosi' aggiorniamo la riga esistente invece di duplicare.
    let effective_name = reused_name.clone().unwrap_or_else(|| name.to_string());

    // Una connessione che gia' esiste e viene riusata non e' una "prima"
    // connessione; la primary viene garantita dopo (vedi sotto).
    // Il DB metadati (NexusMetadata) NON e' mai primary: la primary e' sempre il
    // DB applicativo dell'utente. Solo il ruolo App puo' essere primario.
    let is_first = role == DbRole::App && existing_count == 0;

    let detection_meta = serde_json::json!({ "source": "panel_provision_internal" });
    sqlx::query(
        "INSERT INTO project_database_config (project_id, name, engine, hosting_mode, connection_secret, is_primary, allow_ddl_override, detection_metadata, connection_role) VALUES ($1, $2, 'postgres', 'internal', $3::bytea, $4, false, $5, $6) ON CONFLICT (project_id, LOWER(name)) DO UPDATE SET engine = EXCLUDED.engine, hosting_mode = EXCLUDED.hosting_mode, connection_secret = EXCLUDED.connection_secret, connection_role = EXCLUDED.connection_role, updated_at = NOW()",
    )
    .bind(project_id)
    .bind(&effective_name)
    .bind(url.as_bytes())
    .bind(is_first)
    .bind(&detection_meta)
    .bind(role.as_db_value())
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // ── Garantisci una primary (regola H): mai lasciare il progetto senza una
    // connessione primary. Se nessuna riga ha is_primary=true, promuovi quella
    // appena provisionata/riusata.
    let has_primary: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_database_config WHERE project_id = $1 AND is_primary = true)",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Solo il ruolo App puo' diventare primary: un DB metadati non promuove mai
    // se stesso (la primary resta il DB applicativo dell'utente).
    if !has_primary && role == DbRole::App {
        sqlx::query("UPDATE project_database_config SET is_primary = false WHERE project_id = $1")
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query(
            "UPDATE project_database_config SET is_primary = true, updated_at = NOW() \
             WHERE project_id = $1 AND LOWER(name) = LOWER($2)",
        )
        .bind(project_id)
        .bind(&effective_name)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }

    // Stato primary effettivo della connessione provisionata, per la risposta.
    let is_primary: bool = sqlx::query_scalar(
        "SELECT is_primary FROM project_database_config WHERE project_id = $1 AND LOWER(name) = LOWER($2)",
    )
    .bind(project_id)
    .bind(&effective_name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?
    .unwrap_or(false);

    let reused = reused_name.is_some();

    tx.commit().await.map_err(|e| e.to_string())?;

    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::event::ProjectEvent::DbConfigUpdated {
            name: effective_name.clone(),
            engine: Some("postgres".to_string()),
            action: if created {
                "created".to_string()
            } else {
                "updated".to_string()
            },
        },
    );

    let masked = format!("postgresql://{user}:***@{host}:{port}/{db_name}");
    Ok(json!({
        "ok": true,
        "mode": "internal",
        "name": effective_name,
        "db_name": db_name,
        "dsn": masked,
        "created": created,
        "reused": reused,
        "is_primary": is_primary,
    }))
}

/// Core (regola L) del resolver del pool DB metadati per-progetto: opera su
/// `meta` (pool meta-DB) e `cache` espliciti, cosi' lo stesso codice serve sia i
/// call-site con `&AppState` sia gli helper che usano il registry globale.
/// Risolve da `project_database_config` (connection_role='nexus_metadata'); se
/// assente provisiona `<slug>_nexus`, ne applica lo schema (db/migrations/project)
/// e cacha il pool. Tutti i percorsi passano da qui, mai aprendo pool a mano.
/// Lock per-progetto che SERIALIZZA provision+migrazione: il sqlx-migrator NON e'
/// concurrency-safe (piu' worker che iterano i progetti a flag-on aprono lo stesso
/// pool insieme -> race su `_sqlx_migrations`, "relazione non esiste"). Il primo
/// che entra provisiona+migra+cacha; gli altri attendono e ritrovano il pool.
static PROVISION_LOCKS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

async fn project_meta_pool_core(
    meta: &sqlx::PgPool,
    cache: &nexus_cache::TtlCache<Uuid, std::sync::Arc<sqlx::PgPool>>,
    project_id: Uuid,
) -> Result<std::sync::Arc<sqlx::PgPool>, String> {
    if let Some(pool) = cache.get(&project_id) {
        return Ok(pool);
    }
    // Serializza provision+migrazione per questo progetto (vedi PROVISION_LOCKS).
    let lock = {
        let mut map = PROVISION_LOCKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(project_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    // Doppio controllo: un altro worker potrebbe aver provisionato durante l'attesa.
    if let Some(pool) = cache.get(&project_id) {
        return Ok(pool);
    }
    let url = match resolve_meta_db_url(meta, project_id).await? {
        Some(u) => u,
        None => {
            provision_internal_core(
                meta,
                project_id,
                "nexus_metadata",
                Some("postgres"),
                None,
                DbRole::NexusMetadata,
            )
            .await?;
            resolve_meta_db_url(meta, project_id)
                .await?
                .ok_or_else(|| "DB metadati non risolvibile dopo il provisioning".to_string())?
        }
    };
    // Tetto e attesa dal punto unico (regola L): lo stesso DB `<slug>_nexus`
    // veniva aperto anche da nexus-project-pools, con un tetto diverso.
    let pool = nexus_project_pools::sizing::project_pool_options()
        .connect(&url)
        .await
        .map_err(|e| format!("apertura pool DB metadati (progetto {project_id}) fallita: {e}"))?;
    let arc = std::sync::Arc::new(pool);
    // Schema per-progetto idempotente (db/migrations/project, _sqlx_migrations nel DB-progetto).
    sqlx::migrate::Migrator::new(std::path::Path::new("db/migrations/project"))
        .await
        .map_err(|e| format!("caricamento migrazioni per-progetto fallito: {e}"))?
        .run(arc.as_ref())
        .await
        .map_err(|e| {
            format!("migrazioni schema per-progetto (progetto {project_id}) fallite: {e}")
        })?;
    cache.insert(project_id, arc.clone());
    Ok(arc)
}

/// Risolve la URL del DB metadati Nexus del progetto dal registry
/// `project_database_config` (connection_role='nexus_metadata'). `None` se non
/// ancora provisionato. Delega al punto unico
/// `nexus_project_pools::resolve_meta_db_url` (regola L); l'errore tipizzato e'
/// reso String al bordo del layer provisioning.
async fn resolve_meta_db_url(
    meta_db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Option<String>, String> {
    nexus_project_pools::resolve_meta_db_url(meta_db, project_id)
        .await
        .map_err(|e| e.to_string())
}

/// Errore tipizzato (regola M) della risoluzione del pool per-progetto dentro
/// mcp-core. Niente fallback al meta-DB: a separazione sempre attiva (cutover
/// chiuso, flag rimosso mig 0527) una query del dominio migrato sul meta legge
/// tabelle vuote/decommissionate o, peggio, SCRIVE sul DB sbagliato (incidente
/// 2026-07-20: "Chat 1" inserita sul meta durante il provisioning del
/// DB-progetto e "sparita" dalla UI al primo accesso riuscito). I call site
/// decidono la degradazione sul variante, mai sul testo.
#[derive(Debug, thiserror::Error)]
pub enum ProjectDbError {
    /// Il DB `<slug>_nexus` del progetto non e' utilizzabile ADESSO:
    /// provisioning fallito/in corso, connect fallita o migrazioni fallite.
    /// Condizione transitoria: gli handler HTTP rispondono 503 (retry lato
    /// client), i worker saltano il progetto per questo giro.
    #[error("DB del progetto {project_id} non disponibile: {message}")]
    Unavailable { project_id: Uuid, message: String },

    /// Il registry globale dei pool (`init_global_pools`) non e' inizializzato:
    /// accade solo se un helper viene invocato prima del bootstrap di main.
    #[error("registry dei pool per-progetto non inizializzato")]
    RegistryNotReady,

    /// L'entita' non esiste in ALCUN DB-progetto raggiungibile (ricerca
    /// by-id esaurita con tutti i progetti interrogati).
    #[error("{entity_kind} {entity_id} non trovata in alcun DB progetto")]
    EntityNotFound {
        entity_kind: &'static str,
        entity_id: Uuid,
    },

    /// Ricerca by-id NON conclusiva: l'entita' non e' stata trovata ma almeno
    /// un DB-progetto era irraggiungibile, quindi "non trovata" non e'
    /// dimostrato. Si tratta come indisponibilita' (503), mai come 404.
    #[error(
        "{entity_kind} {entity_id} non risolvibile: {unreachable} DB progetto irraggiungibili durante la ricerca"
    )]
    SearchInconclusive {
        entity_kind: &'static str,
        entity_id: Uuid,
        unreachable: usize,
    },
}

impl ProjectDbError {
    /// Status HTTP coerente col variante: 404 solo quando il "non trovato" e'
    /// dimostrato su tutti i DB-progetto, 503 per ogni indisponibilita'.
    pub fn status_code(&self) -> StatusCode {
        match self {
            ProjectDbError::EntityNotFound { .. } => StatusCode::NOT_FOUND,
            ProjectDbError::Unavailable { .. }
            | ProjectDbError::RegistryNotReady
            | ProjectDbError::SearchInconclusive { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Codice macchina stabile per il client (regola M/N): il frontend decide
    /// su questo, mai sul testo del messaggio.
    pub fn error_code(&self) -> &'static str {
        match self {
            ProjectDbError::EntityNotFound { .. } => "entity_not_found",
            ProjectDbError::Unavailable { .. }
            | ProjectDbError::RegistryNotReady
            | ProjectDbError::SearchInconclusive { .. } => "project_db_unavailable",
        }
    }
}

/// Conversione per gli handler axum: `?` su un `ApiResult` produce la risposta
/// strutturata (status + `{error, code}`) senza boilerplate ai call site.
impl From<ProjectDbError> for (StatusCode, Json<Value>) {
    fn from(e: ProjectDbError) -> Self {
        (
            e.status_code(),
            Json(json!({ "error": e.to_string(), "code": e.error_code() })),
        )
    }
}

/// Punto unico (regola L) per il pool DOVE risiedono i dati per-progetto di un
/// dominio gia' migrato: il DB metadati del progetto (`<slug>_nexus`). La
/// separazione e' SEMPRE attiva (cutover chiuso, flag rimosso mig 0527); i
/// call-site dei domini migrati usano QUESTO, mai `state.db` diretto.
/// NIENTE fallback al meta-DB (regola M): se il DB del progetto non si apre
/// l'esito e' un errore tipizzato che il chiamante gestisce esplicitamente —
/// il fallback silenzioso leggeva liste vuote e SCRIVEVA sul DB sbagliato.
async fn project_data_pool_core(
    meta: &sqlx::PgPool,
    cache: &nexus_cache::TtlCache<Uuid, std::sync::Arc<sqlx::PgPool>>,
    project_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    match project_meta_pool_core(meta, cache, project_id).await {
        Ok(pool) => Ok((*pool).clone()),
        Err(message) => Err(ProjectDbError::Unavailable {
            project_id,
            message,
        }),
    }
}

pub async fn project_data_pool(
    state: &AppState,
    project_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_core(&state.db, &state.project_meta_pools, project_id).await
}

/// Risolve il `project_id` di un'entita' (session/message/run) dalla directory di
/// routing (`nexus_data_routing` nel meta-DB, mig 0496). Delega al punto unico
/// `nexus_project_pools::project_id_for_entity` (regola L). `None` se non mappata.
async fn resolve_project_for_entity(
    meta_db: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: Uuid,
) -> Option<Uuid> {
    nexus_project_pools::project_id_for_entity(meta_db, entity_kind, entity_id).await
}

/// Registra la mappa `entita' -> progetto` nella directory di routing (meta),
/// idempotente. Chiamata ai punti di CREAZIONE dell'entita' (insert_message,
/// spawn_agent_run, ...) cosi' gli endpoint keyed solo dall'id (feedback,
/// delete, confirm/cancel run) possono risolvere il pool del progetto.
/// Best-effort. Delega al punto unico omonimo di nexus-project-pools (regola L).
pub async fn register_entity_routing(
    meta: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: Uuid,
    project_id: Uuid,
) {
    nexus_project_pools::register_entity_routing(meta, entity_kind, entity_id, project_id).await
}

pub async fn project_data_pool_by_session(
    state: &AppState,
    session_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    // Self-healing (regola L, stesso punto unico del by-id): directory O(1); se
    // la sessione NON e' mappata (creata prima della registrazione, o INSERT in
    // directory fallito) NON si degrada al meta — a separazione attiva le
    // tabelle chat sul meta sono vuote/decommissionate e la sessione "sparisce"
    // dalla UI (fetch 404/lista vuota -> il client svuota la chat, incidente
    // 2026-07-02). Si CERCA la sessione nei DB-progetto e si auto-registra la
    // mappa per le chiamate successive.
    project_data_pool_by_search_from(&state.db, "session", "chat_sessions", session_id).await
}

// ── Registry globale del pool per-progetto (route-at-helper) ──────────────────
// Permette agli helper centrali (insert_message, load_message_by_id,
// persist_message_attachments, ...) di instradare i dati per-progetto SENZA
// ricevere &AppState: hanno gia' il pool meta-DB e il project_id, e la cache dei
// pool vive qui. La cache CONDIVIDE lo store con AppState.project_meta_pools
// (TtlCache::clone condivide l'Arc<DashMap>) -> nessun pool aperto due volte.

/// Cache globale dei pool metadati per-progetto. Inizializzata una volta all'avvio.
static GLOBAL_PROJECT_POOL_CACHE: once_cell::sync::OnceCell<
    nexus_cache::TtlCache<Uuid, std::sync::Arc<sqlx::PgPool>>,
> = once_cell::sync::OnceCell::new();

/// Inizializza il registry globale con la cache di `AppState` (chiamato in main.rs).
pub fn init_global_pools(cache: nexus_cache::TtlCache<Uuid, std::sync::Arc<sqlx::PgPool>>) {
    let _ = GLOBAL_PROJECT_POOL_CACHE.set(cache);
}

/// Pool dove risiedono i dati per-progetto, per gli helper che hanno il pool
/// meta-DB (`meta`) e il `project_id` ma non `&AppState`. Usa la cache globale;
/// registry non inizializzato -> `Err(RegistryNotReady)` (regola M): il vecchio
/// "ricade su meta, sicuro" era il fallback silenzioso che scriveva sul DB
/// sbagliato quando un helper partiva prima del bootstrap.
pub async fn project_data_pool_from(
    meta: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    match GLOBAL_PROJECT_POOL_CACHE.get() {
        Some(cache) => project_data_pool_core(meta, cache, project_id).await,
        None => Err(ProjectDbError::RegistryNotReady),
    }
}

/// Variante per worker e task best-effort (punto unico del pattern, regola L):
/// pool del progetto o `None` con WARN gia' emesso. La DECISIONE di degradare
/// (continue nel loop, return, metrica omessa) resta al call site; qui si
/// centralizza solo la coppia risoluzione+log, cosi' il degrado esplicito non
/// gonfia ogni funzione chiamante. MAI usare dove l'errore va propagato.
pub async fn project_data_pool_or_warn(
    meta: &sqlx::PgPool,
    project_id: Uuid,
    context: &'static str,
) -> Option<sqlx::PgPool> {
    match project_data_pool_from(meta, project_id).await {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                context,
                "DB progetto non disponibile, operazione saltata"
            );
            None
        }
    }
}

/// Pool del progetto risolto dal `session_id` (directory di routing).
/// Self-healing come [`project_data_pool_by_session`]: sessione non mappata ->
/// ricerca nei DB-progetto + auto-registrazione, MAI fallback silenzioso al
/// meta (le tabelle chat sul meta sono vuote e la sessione "sparisce").
pub async fn project_data_pool_by_session_from(
    meta: &sqlx::PgPool,
    session_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_by_search_from(meta, "session", "chat_sessions", session_id).await
}

/// Elenco dei `project_id` (tabella globale `projects`, meta-DB). Serve al
/// fallback di ricerca per gli endpoint keyed solo dall'id di un'entita' non
/// ancora in directory, e alle viste admin GLOBALI che aggregano i domini migrati
/// iterando i DB-progetto. Delega al punto unico
/// `nexus_project_pools::list_project_ids` (regola L).
pub async fn list_all_project_ids(meta: &sqlx::PgPool) -> Vec<Uuid> {
    nexus_project_pools::list_project_ids(meta).await
}

/// Risolve il pool del progetto per un'entita' keyed solo dall'id: prima la
/// directory di routing (O(1)); se assente, CERCA l'entita' iterando i
/// DB-progetto (`SELECT 1 FROM <table> WHERE id=$1`) e, trovatala, AUTO-REGISTRA
/// la mappa in directory cosi' le chiamate successive sono O(1) (self-healing).
/// Esaurita la ricerca l'esito e' tipizzato (regola M): `EntityNotFound` se
/// TUTTI i progetti erano interrogabili, `SearchInconclusive` (503, mai 404) se
/// almeno un DB-progetto era irraggiungibile — il vecchio fallback al meta
/// produceva a valle un errore SQL non strutturato o una lettura vuota.
async fn project_data_pool_by_search_from(
    meta: &sqlx::PgPool,
    entity_kind: &'static str,
    table: &str,
    entity_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    let Some(cache) = GLOBAL_PROJECT_POOL_CACHE.get() else {
        return Err(ProjectDbError::RegistryNotReady);
    };
    // Fast-path: directory.
    if let Some(pid) = resolve_project_for_entity(meta, entity_kind, entity_id).await {
        return project_data_pool_core(meta, cache, pid).await;
    }
    // Fallback: cerca l'entita' nei DB-progetto. `table` e' un identificatore
    // costante interno (mai input utente): nessuna SQL-injection.
    let sql = format!("SELECT 1 FROM {table} WHERE id = $1 LIMIT 1");
    let mut unreachable = 0usize;
    for pid in list_all_project_ids(meta).await {
        let pool = match project_data_pool_core(meta, cache, pid).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %pid,
                    entity_kind,
                    entity_id = %entity_id,
                    error = %e,
                    "routing by-id: DB-progetto irraggiungibile durante la ricerca, progetto saltato"
                );
                unreachable += 1;
                continue;
            }
        };
        let found = sqlx::query_scalar::<_, i32>(&sql)
            .bind(entity_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .is_some();
        if found {
            register_entity_routing(meta, entity_kind, entity_id, pid).await;
            return Ok(pool);
        }
    }
    Err(search_exhausted_outcome(entity_kind, entity_id, unreachable))
}

/// Verdetto (e logging) della ricerca by-id esaurita: "non trovata" e'
/// dimostrato SOLO se ogni DB-progetto era interrogabile; con almeno un DB
/// irraggiungibile (es. in provisioning) l'esito e' `SearchInconclusive`
/// (503, mai un 404 bugiardo).
fn search_exhausted_outcome(
    entity_kind: &'static str,
    entity_id: Uuid,
    unreachable: usize,
) -> ProjectDbError {
    if unreachable > 0 {
        return ProjectDbError::SearchInconclusive {
            entity_kind,
            entity_id,
            unreachable,
        };
    }
    // Livello error: un'entita' introvabile in ogni DB-progetto e' sempre
    // un'anomalia da diagnosticare (id inesistente o directory incompleta).
    tracing::error!(
        entity_kind,
        entity_id = %entity_id,
        "routing by-id: entita' non trovata in nessun DB-progetto"
    );
    ProjectDbError::EntityNotFound {
        entity_kind,
        entity_id,
    }
}

/// Pool del progetto risolto dal `message_id` (directory + fallback ricerca). Per
/// gli endpoint keyed solo dal messaggio (feedback_error/positive, delete_message).
pub async fn project_data_pool_by_message_from(
    meta: &sqlx::PgPool,
    message_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_by_search_from(meta, "message", "chat_messages", message_id).await
}

/// Pool del progetto risolto dal `run_id` (directory + fallback ricerca). Per gli
/// endpoint keyed solo dal run (confirm/cancel run).
pub async fn project_data_pool_by_run_from(
    meta: &sqlx::PgPool,
    run_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_by_search_from(meta, "run", "agent_runs", run_id).await
}

/// Pool del progetto risolto dall'id di una `prompt_corrections` (directory +
/// fallback ricerca). Per gli endpoint admin/memory keyed solo dalla correzione
/// (toggle_project_memory, admin_delete_prompt_correction).
pub async fn project_data_pool_by_correction_from(
    meta: &sqlx::PgPool,
    correction_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_by_search_from(meta, "correction", "prompt_corrections", correction_id).await
}

/// Pool del progetto risolto dall'id di un `ai_response_feedback` (directory +
/// fallback ricerca). Per gli endpoint admin keyed solo dal feedback
/// (admin_review_feedback).
pub async fn project_data_pool_by_feedback_from(
    meta: &sqlx::PgPool,
    feedback_id: Uuid,
) -> Result<sqlx::PgPool, ProjectDbError> {
    project_data_pool_by_search_from(meta, "feedback", "ai_response_feedback", feedback_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Limite Postgres per un identificatore (NAMEDATALEN 64 - 1). Un nome che
    /// lo supera viene TRONCATO da Postgres, non rifiutato: due nomi diversi
    /// possono collassare sullo stesso DB senza errore visibile.
    const PG_MAX_IDENT: usize = 63;

    fn pid() -> Uuid {
        Uuid::parse_str("98138624-cf23-4edb-a3b6-bbecadcbb809").expect("uuid di test valido")
    }

    /// Regressione del bug del troncamento divergente: la derivazione viveva in
    /// due copie (qui e `agent_tools::command::sanitize_app_db_name`) che
    /// troncavano la base a 52 e a 56. Entrambi i nomi restavano sotto
    /// PG_MAX_IDENT, quindi nessuno falliva: per uno slug oltre i 52 caratteri il
    /// pannello REST e il tool agente creavano DUE database fisici distinti per
    /// lo stesso progetto. Ora la derivazione e' un punto unico (regola L): un
    /// solo slug -> un solo nome.
    #[test]
    fn slug_lungo_deriva_un_nome_solo_e_valido() {
        let slug = "a".repeat(80);
        let nome = derive_project_db_name(Some(&slug), pid(), DbRole::App);

        // La base e' troncata lasciando spazio al suffisso: 52 + "_app" = 56.
        assert_eq!(nome, format!("{}_app", "a".repeat(52)));
        assert!(nome.len() <= PG_MAX_IDENT);

        // Il vecchio nome divergente (base troncata a 56) non e' piu' derivabile.
        assert_ne!(nome, format!("{}_app", "a".repeat(56)));
    }

    /// Il budget di troncamento si calcola sul suffisso EFFETTIVO: `_nexus` (6)
    /// costa piu' di `_app` (4). Un troncamento a lunghezza fissa che ignora il
    /// suffisso e' esattamente il difetto che ha prodotto la divergenza.
    #[test]
    fn troncamento_e_suffix_aware() {
        let slug = "b".repeat(80);
        let app = derive_project_db_name(Some(&slug), pid(), DbRole::App);
        let nexus = derive_project_db_name(Some(&slug), pid(), DbRole::NexusMetadata);

        assert_eq!(app.len(), 56);
        assert_eq!(nexus.len(), 56);
        assert!(app.ends_with("_app") && nexus.ends_with("_nexus"));
        assert_ne!(app, nexus);
    }

    /// Sotto la soglia lo slug non viene toccato: il troncamento non deve
    /// alterare i nomi dei DB gia' esistenti (nessun progetto reale supera 52).
    #[test]
    fn slug_corto_resta_intatto() {
        assert_eq!(
            derive_project_db_name(Some("beaty-book"), pid(), DbRole::App),
            "beaty_book_app"
        );
    }

    /// Sanitizzazione: solo `[a-z0-9_]`, maiuscole abbassate, resto a `_`.
    #[test]
    fn caratteri_non_ammessi_diventano_underscore() {
        assert_eq!(
            derive_project_db_name(Some("My-Proj.X 1"), pid(), DbRole::App),
            "my_proj_x_1_app"
        );
    }

    /// Un identificatore Postgres non puo' iniziare con una cifra: prefisso 'p'.
    #[test]
    fn slug_che_inizia_con_cifra_riceve_prefisso() {
        let nome = derive_project_db_name(Some("2024-app"), pid(), DbRole::App);
        assert_eq!(nome, "p2024_app_app");
        assert!(!nome.starts_with(|c: char| c.is_ascii_digit()));
    }

    /// Slug assente: si ricade sul project_id. L'uuid di test inizia con "9",
    /// quindi riceve anche il prefisso 'p' (un identificatore Postgres non puo'
    /// iniziare con una cifra): il fallback passa per la stessa sanitizzazione.
    #[test]
    fn slug_assente_ricade_su_project_id_prefissato() {
        let nome = derive_project_db_name(None, pid(), DbRole::App);

        assert_eq!(nome, format!("p{}_app", pid().simple()));
        assert!(nome.len() <= PG_MAX_IDENT);
    }

    /// Uno slug fatto di soli caratteri non ammessi sanifica in underscore: NON
    /// e' vuoto, quindi non innesca il fallback al project_id.
    #[test]
    fn slug_non_sanificabile_resta_underscore() {
        assert_eq!(
            derive_project_db_name(Some("---"), pid(), DbRole::App),
            "____app"
        );
    }

    /// Regressione del fallback silenzioso al meta-DB (incidente 2026-07-20:
    /// GET /api/chat/sessions rispondeva lista vuota dal meta e l'auto-create
    /// "Chat 1" scriveva la sessione sul DB sbagliato mentre il DB del progetto
    /// era in provisioning). Il test attraversa il PRODUTTORE reale (regola O):
    /// `project_data_pool_core` -> `project_meta_pool_core` ->
    /// `resolve_meta_db_url` con un pool lazy verso un endpoint irraggiungibile
    /// — lo scenario "DB giu'" vero, non un input fabbricato. L'esito DEVE
    /// essere `Err(Unavailable)`: reintrodurre `meta.clone()` sul ramo Err
    /// (la mutazione che ha causato l'incidente) fa tornare Ok e il test
    /// fallisce.
    #[tokio::test]
    async fn db_progetto_non_raggiungibile_errore_tipizzato_mai_meta() {
        let meta = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://nexus:nope@127.0.0.1:9/meta_irraggiungibile")
            .expect("connect_lazy non contatta il server: l'URL basta che sia ben formata");
        let cache: nexus_cache::TtlCache<Uuid, std::sync::Arc<sqlx::PgPool>> =
            nexus_cache::TtlCache::new(std::time::Duration::from_secs(60));
        let ghost = Uuid::new_v4();
        match project_data_pool_core(&meta, &cache, ghost).await {
            Err(ProjectDbError::Unavailable { project_id, .. }) => {
                assert_eq!(project_id, ghost);
            }
            Ok(_) => panic!(
                "DB del progetto non raggiungibile deve dare Err(Unavailable): \
                 un Ok qui significa che il fallback silenzioso al meta e' tornato"
            ),
            Err(e) => panic!("variante inattesa per DB irraggiungibile: {e}"),
        }
    }

    /// Il contratto wire dell'errore (regola M/N): status e codice macchina
    /// sono cio' su cui decide il frontend, non il testo. 404 SOLO quando il
    /// "non trovato" e' dimostrato su tutti i DB-progetto; ogni
    /// indisponibilita' (incluso il "non trovato" NON dimostrato di
    /// SearchInconclusive) e' 503 con codice `project_db_unavailable`.
    #[test]
    fn project_db_error_status_e_codici_stabili() {
        let pid = Uuid::new_v4();
        let unavailable = ProjectDbError::Unavailable {
            project_id: pid,
            message: "connect fallita".into(),
        };
        assert_eq!(
            unavailable.status_code(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(unavailable.error_code(), "project_db_unavailable");

        assert_eq!(
            ProjectDbError::RegistryNotReady.status_code(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let inconclusive = ProjectDbError::SearchInconclusive {
            entity_kind: "session",
            entity_id: pid,
            unreachable: 1,
        };
        assert_eq!(inconclusive.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(inconclusive.error_code(), "project_db_unavailable");

        let not_found = ProjectDbError::EntityNotFound {
            entity_kind: "session",
            entity_id: pid,
        };
        assert_eq!(not_found.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(not_found.error_code(), "entity_not_found");

        // La conversione per gli handler axum trasporta ENTRAMBI i campi
        // strutturati nel body: `error` (testo per display) e `code` (macchina).
        let (status, body): (StatusCode, Json<Value>) = unavailable.into();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.0["code"], "project_db_unavailable");
        assert!(body.0["error"].as_str().is_some_and(|s| !s.is_empty()));
    }
}
