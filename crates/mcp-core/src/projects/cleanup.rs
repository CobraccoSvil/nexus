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
        cleanup_systemd_units(slug),
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
        Ok(ids) => container_ids.extend(ids.into_iter()),
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
                if name == slug || name.starts_with(&prefix) {
                    if !container_ids.contains(&id) {
                        container_ids.push(id);
                    }
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

/// Ferma, disabilita e rimuove i service file `~/.config/systemd/user/<slug>-*.service`.
///
/// Esegue `daemon-reload` alla fine se almeno un file e' stato rimosso.
/// Skip se l'unit name inizia con `nexus-` o `ideai-`.
async fn cleanup_systemd_units(slug: &str) -> SystemdResult {
    let mut out = SystemdResult::default();

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            out.errors.push("HOME env non impostata".to_string());
            return out;
        }
    };
    let dir = std::path::PathBuf::from(&home).join(".config/systemd/user");
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
}
