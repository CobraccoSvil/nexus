//! Cleanup propagato delle risorse esterne di un progetto cancellato.
//!
//! Quando un progetto viene rimosso dal DB (DELETE FROM projects + CASCADE),
//! restano fuori dal database una serie di risorse "esterne" che vanno
//! sincronizzate manualmente, pena residui orfani che gonfiano il sistema:
//!
//!   1. **Filesystem**: directory `repository_root_path` (gestita nel chiamante
//!      con chmod + remove_dir_all + handling permessi). Non ripetiamo qui.
//!   2. **Docker**: container con label `com.docker.compose.project=<slug>` o
//!      con nome `^<slug>-*`. Stop + remove (con `-f` se necessario).
//!   3. **Systemd user services**: unit file `~/.config/systemd/user/<slug>-*.service`.
//!      Stop + disable + rm + daemon-reload.
//!   4. **Qdrant**: points filtrati per `project_id` nelle collezioni multi-tenant
//!      (`knowledge_notes`, `project_code_index`, `project_context`,
//!      `prompt_corrections`, `project_docs`). Best-effort delete by filter.
//!
//! Tutte le operazioni sono **best-effort idempotenti**: un fallimento in uno
//! step non blocca gli altri. Ogni successo/errore viene registrato in
//! [`CleanupReport`] che il chiamante puo' includere nella risposta API per
//! tracciabilita'.
//!
//! ## Safety
//!
//! Lo `slug` viene validato (regex `^[a-z0-9][a-z0-9._-]+$`, lunghezza >= 2) e
//! rifiutato se uguale alle infrastrutture core (`ideai`, `nexus`). Il
//! `repository_root_path` non viene mai toccato qui (lo gestisce il chiamante);
//! viene solo loggato. Container e service file con prefisso `ideai-` o
//! `nexus-` sono sempre ignorati, indipendentemente dallo slug.

use serde::Serialize;
use sqlx::PgPool;
use std::process::Stdio;
use uuid::Uuid;

use crate::settings::get_setting;

/// Collezioni Qdrant note che usano payload `project_id` per filtraggio
/// multi-tenant. Aggiungere qui se nuove collezioni adottano lo stesso schema.
const QDRANT_PROJECT_COLLECTIONS: &[(&str, &str)] = &[
    // (chiave settings, collezione default)
    ("qdrant_knowledge_collection", "knowledge_notes"),
    ("qdrant_code_index_collection", "project_code_index"),
    ("qdrant_project_context_collection", "project_context"),
    ("qdrant_prompt_corrections_collection", "prompt_corrections"),
    ("qdrant_docs_collection", "project_docs"),
];

const QDRANT_DEFAULT_URL: &str = "http://localhost:6333";

/// Slug riservati che non devono mai essere usati come target di cleanup
/// (sono infrastruttura, non progetti utente).
const RESERVED_SLUGS: &[&str] = &["ideai", "nexus", "postgres", "qdrant", "redis", "grafana"];

#[derive(Debug, Serialize, Default, Clone)]
pub struct CleanupReport {
    pub slug: String,
    pub project_id: String,
    pub docker_containers_removed: Vec<String>,
    pub docker_errors: Vec<String>,
    pub systemd_units_removed: Vec<String>,
    pub systemd_errors: Vec<String>,
    pub qdrant_points_purged: Vec<QdrantCollectionResult>,
    pub qdrant_errors: Vec<String>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct QdrantCollectionResult {
    pub collection: String,
    pub status: String,
}

/// Valida lo slug: alfa-numerico minuscolo, separatori `-._`, primo carattere
/// alfanumerico, lunghezza >= 2, non riservato.
fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.len() < 2 {
        return Err(format!("slug troppo corto: '{}'", slug));
    }
    if RESERVED_SLUGS.iter().any(|r| r.eq_ignore_ascii_case(slug)) {
        return Err(format!("slug '{}' e' riservato (infrastruttura)", slug));
    }
    let mut chars = slug.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(format!("slug '{}' deve iniziare con alfanumerico", slug));
    }
    for c in slug.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.';
        if !ok {
            return Err(format!(
                "slug '{}' contiene caratteri non permessi (atteso [a-z0-9._-])",
                slug
            ));
        }
    }
    Ok(())
}

/// Esegue il cleanup completo delle risorse esterne associate al progetto.
///
/// Da chiamare DOPO il `DELETE FROM projects` (i record DB sono autoritativi
/// nel decidere "il progetto esiste o no"). Non fallisce: ritorna sempre un
/// `CleanupReport` valido con elenco di successi/errori.
pub async fn cleanup_external_resources(
    db: &PgPool,
    project_id: Uuid,
    slug: &str,
) -> CleanupReport {
    let mut report = CleanupReport {
        slug: slug.to_string(),
        project_id: project_id.to_string(),
        ..Default::default()
    };

    if let Err(reason) = validate_slug(slug) {
        tracing::warn!("cleanup_external_resources: slug rifiutato ({reason})");
        report.skipped_reason = Some(reason);
        return report;
    }

    // Esegui in parallelo: i 3 step non condividono risorse condivise.
    let (docker, systemd, qdrant) = tokio::join!(
        cleanup_docker_containers(slug),
        cleanup_project_services(db, project_id, slug),
        cleanup_qdrant_points(db, project_id),
    );

    report.docker_containers_removed = docker.removed;
    report.docker_errors = docker.errors;
    report.systemd_units_removed = systemd.removed;
    report.systemd_errors = systemd.errors;
    report.qdrant_points_purged = qdrant.results;
    report.qdrant_errors = qdrant.errors;

    tracing::info!(
        target: "project_cleanup",
        slug = %slug,
        project_id = %project_id,
        docker_n = report.docker_containers_removed.len(),
        systemd_n = report.systemd_units_removed.len(),
        qdrant_n = report.qdrant_points_purged.len(),
        "cleanup esterno completato"
    );

    report
}

// ── Docker ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct DockerResult {
    removed: Vec<String>,
    errors: Vec<String>,
}

/// Ferma e rimuove i container Docker associati al progetto.
///
/// Strategia: due query parallele a `docker ps -aq`:
///  - per label `com.docker.compose.project=<slug>` (compose project)
///  - per nome `^<slug>-` (container con prefisso slug)
///
/// Poi `docker rm -f` su tutti gli id deduplicati. Skip se i container
/// risultano essere infrastruttura `ideai-*` o `nexus-*`.
async fn cleanup_docker_containers(slug: &str) -> DockerResult {
    let mut out = DockerResult::default();

    // 1. Container con label compose
    let label_filter = format!("label=com.docker.compose.project={}", slug);
    let by_label = run_docker_ps(&["--filter", &label_filter]).await;

    // 2. Container con nome che inizia per `<slug>-`
    // Docker `--filter name=` fa substring match, non prefix. Filtriamo client-side.
    let by_name = run_docker_ps(&["--format", "{{.Names}} {{.ID}}"]).await;

    let mut container_ids: Vec<String> = Vec::new();
    match by_label {
        Ok(ids) => container_ids.extend(ids),
        Err(e) => out.errors.push(format!("docker ps (label): {}", e)),
    }
    match by_name {
        Ok(lines) => {
            let prefix = format!("{}-", slug);
            for line in lines {
                let parts: Vec<&str> = line.splitn(2, ' ').collect();
                if parts.len() != 2 {
                    continue;
                }
                let name = parts[0];
                let id = parts[1].to_string();
                if (name == slug || name.starts_with(&prefix)) && !container_ids.contains(&id) {
                    container_ids.push(id);
                }
            }
        }
        Err(e) => out.errors.push(format!("docker ps (name): {}", e)),
    }

    if container_ids.is_empty() {
        return out;
    }

    // Safety: per ogni id risolvi nome e label per evitare di toccare
    // infrastruttura. `docker inspect --format '{{.Name}}|{{index .Config.Labels "com.docker.compose.project"}}'`.
    for id in &container_ids {
        match docker_inspect_meta(id).await {
            Ok((name, compose_proj)) => {
                let bare_name = name.trim_start_matches('/').to_string();
                if bare_name.starts_with("ideai-") || bare_name.starts_with("nexus-") {
                    out.errors.push(format!(
                        "skip container infrastruttura '{}' (id={}, compose={})",
                        bare_name, id, compose_proj
                    ));
                    continue;
                }
                if compose_proj == "ideai" || compose_proj == "nexus" {
                    out.errors.push(format!(
                        "skip container con compose project infrastrutturale '{}' (id={})",
                        compose_proj, id
                    ));
                    continue;
                }
                match docker_rm_force(id).await {
                    Ok(_) => out.removed.push(bare_name),
                    Err(e) => out.errors.push(format!("docker rm {}: {}", id, e)),
                }
            }
            Err(e) => {
                out.errors.push(format!("docker inspect {}: {}", id, e));
            }
        }
    }

    out
}

async fn run_docker_ps(args: &[&str]) -> Result<Vec<String>, String> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("ps").arg("-a");
    for a in args {
        cmd.arg(a);
    }
    if !args
        .iter()
        .any(|a| *a == "--format" || a.starts_with("--format"))
    {
        cmd.arg("-q");
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("docker ps exec failed: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("docker ps exit {}: {}", output.status, stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(lines)
}

async fn docker_inspect_meta(id: &str) -> Result<(String, String), String> {
    let output = tokio::process::Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.Name}}|{{index .Config.Labels \"com.docker.compose.project\"}}",
            id,
        ])
        .output()
        .await
        .map_err(|e| format!("exec: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut parts = raw.splitn(2, '|');
    let name = parts.next().unwrap_or("").to_string();
    let compose = parts.next().unwrap_or("").to_string();
    Ok((name, compose))
}

async fn docker_rm_force(id: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("docker")
        .args(["rm", "-f", "-v", id])
        .output()
        .await
        .map_err(|e| format!("exec: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}

// ── Systemd user services ──────────────────────────────────────────────────

#[derive(Default)]
struct SystemdResult {
    removed: Vec<String>,
    errors: Vec<String>,
}

/// De-registra i servizi del progetto dal manager della piattaforma.
///
/// Dispatch platform-aware (regola L): su Linux ferma/disabilita/rimuove le unit
/// `systemd --user`; su Windows ferma e de-registra i processi gestiti in
/// `agent_processes` (kind='service'). Prima questa funzione era solo-systemd e
/// su Windows produceva "HOME env non impostata" nel report senza toccare i
/// servizi Windows reali.
async fn cleanup_project_services(db: &PgPool, project_id: Uuid, slug: &str) -> SystemdResult {
    #[cfg(windows)]
    {
        let _ = slug;
        cleanup_windows_services(db, project_id).await
    }
    #[cfg(not(windows))]
    {
        let _ = (db, project_id);
        cleanup_systemd_units(slug).await
    }
}

/// Windows: ferma e de-registra i servizi del progetto in `agent_processes`.
///
/// Enumera via il PUNTO UNICO `service_manager::active().list` (regola L) e ferma
/// ciascun servizio col relativo `stop` (kill dei pid running + status='stopped').
/// Poi RIMUOVE le righe kind='service' del progetto: il progetto e' gia' stato
/// cancellato dal DB meta, quindi la de-registrazione e' definitiva ed equivale
/// alla rimozione dei file unit su Linux. `project_root` non e' necessario qui
/// (list/stop Windows non lo usano nel ramo cleanup), quindi si passa una path
/// vuota.
#[cfg(windows)]
async fn cleanup_windows_services(db: &PgPool, project_id: Uuid) -> SystemdResult {
    use crate::project_workspace::service_manager::{self, ServiceBackend, ServiceContext};

    let mut out = SystemdResult::default();
    let empty_root = std::path::Path::new("");
    let ctx = ServiceContext {
        db,
        port_registry: None,
        project_id,
        // Lo slug del ctx serve solo a comporre l'unit name Windows (non usato
        // nel cleanup, che filtra per project_id): valore neutro.
        slug: "",
        project_root: empty_root,
    };

    let backend = service_manager::active();
    for entry in backend.list(&ctx).await {
        // Esito da segnale strutturato (regola M): `acted`, non parsing di prosa.
        let outcome = backend.stop(&ctx, &entry.label).await;
        if outcome.acted {
            out.removed.push(entry.label.clone());
        }
    }

    // De-registrazione definitiva delle righe kind='service' del progetto sul
    // pool del progetto (agent_processes e' tabella migrata). Cleanup
    // best-effort: DB progetto non disponibile -> errore nel report, niente
    // fallback al meta-DB.
    match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(proj_pool) => {
            if let Err(e) =
                sqlx::query("DELETE FROM agent_processes WHERE project_id = $1 AND kind = 'service'")
                    .bind(project_id)
                    .execute(&proj_pool)
                    .await
            {
                out.errors
                    .push(format!("de-registrazione servizi Windows: {e}"));
            }
        }
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "cleanup servizi Windows: DB progetto non disponibile, de-registrazione saltata");
            out.errors
                .push(format!("de-registrazione servizi Windows: DB progetto non disponibile: {e}"));
        }
    }

    out
}

/// Ferma, disabilita e rimuove i service file `~/.config/systemd/user/<slug>-*.service`.
///
/// Esegue `daemon-reload` alla fine se almeno un file e' stato rimosso.
/// Skip se l'unit name inizia con `nexus-` o `ideai-`.
#[cfg(not(windows))]
async fn cleanup_systemd_units(slug: &str) -> SystemdResult {
    let mut out = SystemdResult::default();

    // Fallback HOME consolidato sul punto unico del path (regola L):
    // service_manager::user_systemd_dir(), che gestisce la env mancante senza
    // errore spurio nel report.
    let dir =
        std::path::PathBuf::from(crate::project_workspace::service_manager::user_systemd_dir());
    if !dir.exists() {
        return out;
    }

    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) => {
            out.errors
                .push(format!("read_dir {}: {}", dir.display(), e));
            return out;
        }
    };
    let prefix = format!("{}-", slug);
    let mut matched: Vec<(String, std::path::PathBuf)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let fname = match path.file_name().and_then(|f| f.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Solo .service che iniziano per "<slug>-" oppure esattamente "<slug>.service"
        if !fname.ends_with(".service") {
            continue;
        }
        if fname.starts_with("nexus-") || fname.starts_with("ideai-") {
            continue;
        }
        let bare = &fname[..fname.len() - ".service".len()];
        if bare == slug || bare.starts_with(&prefix) {
            matched.push((fname, path));
        }
    }

    if matched.is_empty() {
        return out;
    }

    for (unit, path) in &matched {
        let stop = tokio::process::Command::new("systemctl")
            .args(["--user", "stop", unit])
            .output()
            .await;
        if let Err(e) = stop {
            out.errors.push(format!("systemctl stop {}: {}", unit, e));
        }
        let disable = tokio::process::Command::new("systemctl")
            .args(["--user", "disable", unit])
            .output()
            .await;
        if let Err(e) = disable {
            // disable di un unit non-enabled e' errore informativo, lo loggo soft
            tracing::debug!("systemctl disable {}: {}", unit, e);
        }
        match tokio::fs::remove_file(path).await {
            Ok(_) => out.removed.push(unit.clone()),
            Err(e) => out.errors.push(format!("rm {}: {}", path.display(), e)),
        }
    }

    if !out.removed.is_empty() {
        let _ = tokio::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output()
            .await;
        let _ = tokio::process::Command::new("systemctl")
            .args(["--user", "reset-failed"])
            .output()
            .await;
    }

    out
}

// ── Qdrant ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct QdrantResult {
    results: Vec<QdrantCollectionResult>,
    errors: Vec<String>,
}

async fn cleanup_qdrant_points(db: &PgPool, project_id: Uuid) -> QdrantResult {
    let mut out = QdrantResult::default();

    let base_url = match get_setting(db, "qdrant_url").await {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => std::env::var("QDRANT_URL").unwrap_or_else(|_| QDRANT_DEFAULT_URL.to_string()),
    };

    for (setting_key, default_name) in QDRANT_PROJECT_COLLECTIONS {
        let collection = match get_setting(db, setting_key).await {
            Ok(Some(v)) if !v.is_empty() => v,
            _ => default_name.to_string(),
        };
        match qdrant_delete_by_project(&base_url, &collection, project_id).await {
            Ok(status) => out
                .results
                .push(QdrantCollectionResult { collection, status }),
            Err(e) => out.errors.push(format!("collection {}: {}", collection, e)),
        }
    }

    out
}

async fn qdrant_delete_by_project(
    base_url: &str,
    collection: &str,
    project_id: Uuid,
) -> Result<String, String> {
    let url = format!(
        "{}/collections/{}/points/delete?wait=true",
        base_url.trim_end_matches('/'),
        collection
    );
    let body = serde_json::json!({
        "filter": {
            "must": [
                { "key": "project_id", "match": { "value": project_id.to_string() } }
            ]
        }
    });
    let client = nexus_http::build_client();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {}", e))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        // collezione assente -> nessun lavoro da fare, non e' un errore
        return Ok("collection_missing".to_string());
    }
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!(
            "http {}: {}",
            status,
            body.chars().take(160).collect::<String>()
        ));
    }
    let result_body = response
        .text()
        .await
        .unwrap_or_else(|_| "<no body>".to_string());
    let parsed: serde_json::Value =
        serde_json::from_str(&result_body).unwrap_or(serde_json::Value::Null);
    let inner_status = parsed
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("acknowledged")
        .to_string();
    Ok(inner_status)
}

// ── Drop dei database applicativi interni ────────────────────────────────────

/// Esito del drop dei database applicativi provisionati internamente da Nexus.
#[derive(Debug, Serialize, Default, Clone)]
pub struct DbDropResult {
    /// Nomi dei database droppati con successo.
    pub dropped: Vec<String>,
    /// Messaggi di errore o skip (best-effort, non bloccanti).
    pub errors: Vec<String>,
}

/// Verifica se un nome di database e' ammesso come bersaglio di DROP.
///
/// Guard di sicurezza (regola E): mai droppare i database di sistema Postgres
/// (`postgres`, `template0`, `template1`) ne' qualsiasi database di
/// infrastruttura Nexus (prefissi `ideai`/`nexus`). Vuoto/whitespace sempre
/// rifiutato. Il match e' case-insensitive.
fn db_name_droppable(dbname: &str) -> bool {
    let name = dbname.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if matches!(name.as_str(), "postgres" | "template0" | "template1") {
        return false;
    }
    if name.starts_with("ideai") || name.starts_with("nexus") {
        return false;
    }
    true
}

/// Droppa i database applicativi che Nexus ha provisionato internamente per il
/// progetto (`engine='postgres' AND hosting_mode='internal'`).
///
/// DA CHIAMARE PRIMA del `DELETE FROM projects`: il CASCADE rimuove anche
/// `project_database_config`, quindi dopo il DELETE non si potrebbe piu' sapere
/// quali database fisici appartenevano al progetto. I database `external` NON
/// vengono toccati (sono di proprieta' dell'utente, non provisionati da Nexus).
///
/// Parsing del DSN (regola L): il `connection_secret` e' la URL postgres in
/// chiaro come bytea (vedi `provision_internal_core`, che la scrive con
/// `url.as_bytes()`). Il bersaglio fisico (host, port, dbname) viene estratto dal
/// PUNTO UNICO `project_db_routes::pg_physical_target`; le credenziali vengono
/// riusate parsando lo stesso DSN con `PgConnectOptions` di sqlx, senza
/// re-implementare il parsing di user/password.
///
/// Drop: ci si connette al MEDESIMO server (host:port, stesse credenziali) ma al
/// database di servizio `postgres` (lo stesso owner puo' droppare il proprio DB)
/// ed esegue `DROP DATABASE IF EXISTS "<dbname>" WITH (FORCE)` (FORCE termina le
/// connessioni residue, Postgres 13+).
///
/// Best-effort idempotente: ogni errore/skip e' loggato (WARN) e accumulato in
/// `DbDropResult`, non blocca la cancellazione del progetto.
pub async fn drop_internal_app_databases(db: &PgPool, project_id: Uuid) -> DbDropResult {
    use std::str::FromStr;

    let mut out = DbDropResult::default();

    // Legge le sole connessioni interne postgres. `connection_secret` e' bytea.
    let rows: Vec<(String, Option<Vec<u8>>)> = match sqlx::query_as(
        r#"SELECT name, connection_secret
           FROM project_database_config
           WHERE project_id = $1
             AND engine = 'postgres'
             AND hosting_mode = 'internal'"#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "project_cleanup",
                project_id = %project_id,
                "drop_internal_app_databases: query config fallita: {e}"
            );
            out.errors
                .push(format!("query project_database_config: {e}"));
            return out;
        }
    };

    for (name, secret) in rows {
        let dsn = secret
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(dsn) = dsn else {
            tracing::warn!(
                target: "project_cleanup",
                project_id = %project_id,
                conn = %name,
                "drop_internal_app_databases: connection_secret vuoto/non utf8, skip"
            );
            out.errors.push(format!(
                "connessione '{name}': secret vuoto o non leggibile"
            ));
            continue;
        };

        // Punto unico (regola L): estrazione host/port/dbname dal DSN.
        let Some((host, port, dbname)) = crate::project_db_routes::pg_physical_target(&dsn) else {
            tracing::warn!(
                target: "project_cleanup",
                project_id = %project_id,
                conn = %name,
                "drop_internal_app_databases: DSN non parsabile come postgres, skip"
            );
            out.errors
                .push(format!("connessione '{name}': DSN non parsabile"));
            continue;
        };

        // Guard di sicurezza (regola E): mai i DB di sistema o l'infrastruttura.
        if !db_name_droppable(&dbname) {
            tracing::warn!(
                target: "project_cleanup",
                project_id = %project_id,
                conn = %name,
                dbname = %dbname,
                "drop_internal_app_databases: dbname protetto da guard, skip"
            );
            out.errors
                .push(format!("database '{dbname}': protetto (guard sicurezza)"));
            continue;
        }

        // Riusa le credenziali del DSN salvato; punta al database di servizio
        // 'postgres' per poter eseguire il DROP del database applicativo.
        let admin_opts = match sqlx::postgres::PgConnectOptions::from_str(&dsn) {
            Ok(o) => o.database("postgres"),
            Err(e) => {
                tracing::warn!(
                    target: "project_cleanup",
                    project_id = %project_id,
                    conn = %name,
                    "drop_internal_app_databases: PgConnectOptions invalide: {e}"
                );
                out.errors
                    .push(format!("connessione '{name}': opzioni non valide"));
                continue;
            }
        };

        let pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(admin_opts)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    target: "project_cleanup",
                    project_id = %project_id,
                    host = %host,
                    port = port,
                    "drop_internal_app_databases: connessione al server app fallita: {e}"
                );
                out.errors.push(format!(
                    "server {host}:{port} (db '{dbname}'): connessione fallita"
                ));
                continue;
            }
        };

        // Il dbname e' gia' validato dai guard; quoting con doppi apici per
        // l'identificatore (non e' interpolazione di un valore utente arbitrario).
        let drop_sql = format!("DROP DATABASE IF EXISTS \"{dbname}\" WITH (FORCE)");
        match sqlx::query(&drop_sql).execute(&pool).await {
            Ok(_) => {
                tracing::info!(
                    target: "project_cleanup",
                    project_id = %project_id,
                    host = %host,
                    port = port,
                    dbname = %dbname,
                    "drop_internal_app_databases: database applicativo droppato"
                );
                out.dropped.push(dbname);
            }
            Err(e) => {
                tracing::warn!(
                    target: "project_cleanup",
                    project_id = %project_id,
                    dbname = %dbname,
                    "drop_internal_app_databases: DROP DATABASE fallito: {e}"
                );
                out.errors.push(format!("DROP DATABASE \"{dbname}\": {e}"));
            }
        }
        pool.close().await;
    }

    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_ok() {
        assert!(validate_slug("demo-wsl").is_ok());
        assert!(validate_slug("project1").is_ok());
        assert!(validate_slug("a.b").is_ok());
    }

    #[test]
    fn slug_reserved() {
        assert!(validate_slug("ideai").is_err());
        assert!(validate_slug("Ideai").is_err());
        assert!(validate_slug("nexus").is_err());
    }

    #[test]
    fn slug_too_short() {
        assert!(validate_slug("a").is_err());
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn slug_bad_chars() {
        assert!(validate_slug("foo/bar").is_err());
        assert!(validate_slug("../etc").is_err());
        assert!(validate_slug("foo bar").is_err());
        assert!(validate_slug("FOO").is_err());
    }

    #[test]
    fn slug_bad_first() {
        assert!(validate_slug("-foo").is_err());
        assert!(validate_slug(".foo").is_err());
    }

    #[test]
    fn db_droppable_rifiuta_sistema_e_infra() {
        // Database di sistema Postgres: mai droppabili.
        assert!(!db_name_droppable("postgres"));
        assert!(!db_name_droppable("Postgres"));
        assert!(!db_name_droppable("template0"));
        assert!(!db_name_droppable("template1"));
        // Infrastruttura Nexus (prefissi riservati, case-insensitive).
        assert!(!db_name_droppable("ideai"));
        assert!(!db_name_droppable("ideai_meta"));
        assert!(!db_name_droppable("IDEAI-postgres-nexus"));
        assert!(!db_name_droppable("nexus"));
        assert!(!db_name_droppable("nexus_main"));
        assert!(!db_name_droppable("Nexus"));
        // Vuoto / solo whitespace: rifiutato.
        assert!(!db_name_droppable(""));
        assert!(!db_name_droppable("   "));
    }

    #[test]
    fn db_droppable_accetta_db_applicativi() {
        // Database applicativi reali provisionati per un progetto utente.
        assert!(db_name_droppable("beauty_book_app"));
        assert!(db_name_droppable("freelance_app"));
        assert!(db_name_droppable("myproject_app"));
        // Nome che contiene (ma non inizia con) un prefisso riservato: ammesso.
        assert!(db_name_droppable("my_nexus_clone_app"));
    }
}
