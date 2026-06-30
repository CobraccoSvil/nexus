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
    /// Valore della colonna `connection_role`.
    fn as_db_value(self) -> &'static str {
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

/// Legge un setting da DB con fallback conservativo (allineato a
/// agent_tools::command::ensure_project_db_url).
async fn load_app_db_setting(db: &sqlx::PgPool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
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
/// dal `suffix` del ruolo (`_app` o `_nexus`). Stessa logica idempotente di
/// `ensure_project_db_url` per riconoscere un DB gia' creato.
fn derive_project_db_name(slug: Option<&str>, project_id: Uuid, suffix: &str) -> String {
    let base = slug
        .map(|s| s.to_string())
        .unwrap_or_else(|| project_id.simple().to_string());
    let mut sanitized: String = sanitize_db_ident(&base);
    if sanitized.is_empty() {
        sanitized = project_id.simple().to_string();
    }
    if sanitized
        .chars()
        .next()
        .is_none_or(|c| c.is_ascii_digit())
    {
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
    match provision_internal_core(&state.db, project_id, name, engine_in, db_name_in, DbRole::App)
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
                derive_project_db_name(slug.as_deref(), project_id, role.suffix())
            } else {
                cleaned
            }
        }
        None => derive_project_db_name(slug.as_deref(), project_id, role.suffix()),
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

/// Punto unico (regola L, Fase 0 separazione DB) per ottenere il pool del DB
/// metadati Nexus del progetto (`<slug>_nexus`). Risolve la connessione da
/// `project_database_config` (connection_role='nexus_metadata'); se assente,
/// provisiona il DB e rilegge. Il pool e' cachato in `state.project_meta_pools`
/// (TtlCache, invalidazione su re-provisioning / scadenza TTL). Tutti i call-site
/// che accedono ai dati per-progetto passano da qui, mai aprendo pool a mano.
pub async fn project_meta_pool(
    state: &AppState,
    project_id: Uuid,
) -> Result<std::sync::Arc<sqlx::PgPool>, String> {
    if let Some(pool) = state.project_meta_pools.get(&project_id) {
        return Ok(pool);
    }
    let url = match resolve_meta_db_url(&state.db, project_id).await? {
        Some(u) => u,
        None => {
            provision_internal_core(
                &state.db,
                project_id,
                "nexus_metadata",
                Some("postgres"),
                None,
                DbRole::NexusMetadata,
            )
            .await?;
            resolve_meta_db_url(&state.db, project_id)
                .await?
                .ok_or_else(|| "DB metadati non risolvibile dopo il provisioning".to_string())?
        }
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&url)
        .await
        .map_err(|e| format!("apertura pool DB metadati (progetto {project_id}) fallita: {e}"))?;
    let arc = std::sync::Arc::new(pool);
    // Applica lo schema per-progetto (db/migrations/project/) al DB metadati.
    // Idempotente: sqlx salta le migrazioni gia' applicate (tracciate in
    // _sqlx_migrations del DB-progetto). Path relativo alla root del repo, come
    // db.rs (cwd del servizio = D:\IDEAI via workingdirectory WinSW).
    sqlx::migrate::Migrator::new(std::path::Path::new("db/migrations/project"))
        .await
        .map_err(|e| format!("caricamento migrazioni per-progetto fallito: {e}"))?
        .run(arc.as_ref())
        .await
        .map_err(|e| format!("migrazioni schema per-progetto (progetto {project_id}) fallite: {e}"))?;
    state.project_meta_pools.insert(project_id, arc.clone());
    Ok(arc)
}

/// Risolve la URL del DB metadati Nexus del progetto dal registry
/// `project_database_config` (connection_role='nexus_metadata'). `None` se non
/// ancora provisionato. La URL e' salvata in chiaro nel `connection_secret`
/// (vedi nota sicurezza in cleanup.rs); la lettura e' coerente con
/// `resolve_project_conn`.
async fn resolve_meta_db_url(
    meta_db: &sqlx::PgPool,
    project_id: Uuid,
) -> Result<Option<String>, String> {
    let secret: Option<Vec<u8>> = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT connection_secret FROM project_database_config \
         WHERE project_id = $1 AND connection_role = 'nexus_metadata' \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(meta_db)
    .await
    .map_err(|e| e.to_string())?
    .flatten();
    Ok(secret
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}
