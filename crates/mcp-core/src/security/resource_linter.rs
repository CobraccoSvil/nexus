//! Linter periodico dei sorgenti progetto per violazioni di governance risorse
//! (porte hardcoded fuori bucket / nel bucket non allocate, URL interni
//! hardcoded). Copre i canali che l'enforcement in scrittura non intercetta:
//! sed/heredoc via run_command, file preesistenti, repository importati.
//!
//! Sola RILEVAZIONE: ogni finding apre una diagnosi `policy_violation`
//! (pannello Problemi) via `resource_governance::open_resource_violation` e
//! viene auditato (`outcome='detected'`); la riparazione e' demandata a
//! `project_workspace::resource_violation_remediation`.
//!
//! Config DB-driven (regola G): settings `agent.resource_violation.linter_*`
//! (mig 0398) + opt-out per progetto `projects.port_lint_enabled` (il
//! meta-progetto Nexus e' escluso alla radice). Parsing delegato ai punti unici
//! `agent_tools::port_scanner` / `agent_tools::url_scanner` (regola L).

use std::collections::HashSet;
use std::path::Path;

use sqlx::PgPool;
use uuid::Uuid;

/// Directory escluse dal walk (generati, dipendenze, VCS).
const LINT_EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "out",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    "coverage",
    ".cache",
    "figma_export",
];

/// Estensioni incluse (sorgenti/config). File senza estensione: solo nomi noti.
const LINT_INCLUDED_EXTS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "rb", "go", "rs", "java", "cs", "php", "sh",
    "yml", "yaml", "json", "toml", "conf", "ini",
];
const LINT_INCLUDED_BARE_NAMES: &[&str] = &["Dockerfile", "Procfile", "Makefile"];

/// Cap difensivi: file enormi (lock/minificati) e walk runaway.
const MAX_FILE_BYTES: u64 = 262_144;
const MAX_FILES_PER_SCAN: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceViolationKind {
    PortOutOfBucket,
    PortBucketNotAllocated,
    InternalUrlHardcoded,
}

impl ResourceViolationKind {
    pub fn rule(&self) -> (&'static str, &'static str) {
        match self {
            Self::PortOutOfBucket => ("port", "enforce_hardcode"),
            Self::PortBucketNotAllocated => ("port", "require_allocation"),
            Self::InternalUrlHardcoded => ("network", "no_hardcoded_internal"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResourceLintFinding {
    pub rel_path: String,
    pub line: usize,
    /// Porta (per le violazioni porte) o 0 per gli URL.
    pub port: u32,
    /// Valore leggibile (porta o URL) per detail/firma.
    pub value: String,
    pub kind: ResourceViolationKind,
    pub snippet: String,
}

/// Lint di un singolo contenuto (funzione pura, testabile). Delega ai punti
/// unici di parsing; `allocated` = porte allocate del progetto.
pub fn lint_file_content(
    rel_path: &str,
    content: &str,
    allocated: &HashSet<u32>,
) -> Vec<ResourceLintFinding> {
    // I file .env* sono il posto canonico delle porte/URL come variabili:
    // skip totale (coerente con scan_content del port_scanner).
    let file_name = Path::new(rel_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(rel_path);
    if file_name.to_lowercase().starts_with(".env") {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for f in crate::agent_tools::port_scanner::collect_out_of_bucket_ports(content) {
        findings.push(ResourceLintFinding {
            rel_path: rel_path.to_string(),
            line: f.line,
            port: f.port,
            value: f.port.to_string(),
            kind: ResourceViolationKind::PortOutOfBucket,
            snippet: f.snippet.clone(),
        });
    }
    for f in crate::agent_tools::port_scanner::collect_bucket_ports(content) {
        if !allocated.contains(&f.port) {
            findings.push(ResourceLintFinding {
                rel_path: rel_path.to_string(),
                line: f.line,
                port: f.port,
                value: f.port.to_string(),
                kind: ResourceViolationKind::PortBucketNotAllocated,
                snippet: f.snippet.clone(),
            });
        }
    }
    for f in crate::agent_tools::url_scanner::collect_internal_urls(rel_path, content) {
        findings.push(ResourceLintFinding {
            rel_path: rel_path.to_string(),
            line: f.line,
            port: 0,
            value: f.url.clone(),
            kind: ResourceViolationKind::InternalUrlHardcoded,
            snippet: f.snippet.clone(),
        });
    }

    findings
}

fn should_lint_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if LINT_INCLUDED_BARE_NAMES
        .iter()
        .any(|n| name == *n || name.starts_with(&format!("{n}.")))
    {
        return true;
    }
    if name.starts_with("docker-compose") {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => LINT_INCLUDED_EXTS.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

/// Walk sincrono della project_root (da chiamare in `spawn_blocking`).
pub fn lint_tree(root: &Path, allocated: &HashSet<u32>) -> Vec<ResourceLintFinding> {
    let mut findings = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= MAX_FILES_PER_SCAN {
                tracing::warn!(
                    "resource_linter: cap {MAX_FILES_PER_SCAN} file raggiunto, scan parziale"
                );
                return findings;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !LINT_EXCLUDED_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            if !should_lint_file(&path) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
            }
            scanned += 1;
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
            findings.extend(lint_file_content(&rel, &content, allocated));
        }
    }
    findings
}

/// Variante mirata per la catena del kill runtime: solo finding sulla porta data.
pub fn lint_tree_for_port(
    root: &Path,
    allocated: &HashSet<u32>,
    target: u32,
) -> Vec<ResourceLintFinding> {
    lint_tree(root, allocated)
        .into_iter()
        .filter(|f| f.port == target)
        .collect()
}

/// Porte allocate di un progetto (fonte unica `nexus_port_allocations`).
pub async fn allocated_ports_for_project(db: &PgPool, project_id: Uuid) -> HashSet<u32> {
    sqlx::query_scalar::<_, i32>(
        "SELECT port::int FROM nexus_port_allocations WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|p| p as u32)
    .collect()
}

/// Esegue il lint di UN progetto, apre le diagnosi e audita i finding nuovi.
/// Ritorna il numero di violazioni aperte (nuove).
pub async fn lint_project(
    state: &crate::AppState,
    project_id: Uuid,
    root_path: &str,
) -> usize {
    let db = &state.db;
    let allocated = allocated_ports_for_project(db, project_id).await;
    let root = std::path::PathBuf::from(root_path);
    if !root.is_dir() {
        return 0;
    }
    let alloc_clone = allocated.clone();
    let findings = match tokio::task::spawn_blocking(move || lint_tree(&root, &alloc_clone)).await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "resource_linter: walk fallito");
            return 0;
        }
    };

    let mut opened = 0usize;
    for f in &findings {
        let (kind, rule) = f.kind.rule();
        let sig = crate::security::resource_governance::violation_signature(
            project_id,
            Some(&f.rel_path),
            &f.value,
            &format!("{kind}/{rule}"),
        );
        let detail = format!(
            "{}:{} {} ({}/{}) | {}",
            f.rel_path,
            f.line,
            f.value,
            kind,
            rule,
            f.snippet.chars().take(160).collect::<String>()
        );
        let opened_id = crate::security::resource_governance::open_resource_violation(
            db,
            project_id,
            kind,
            rule,
            f.port as f64,
            Some(&f.rel_path),
            &detail,
            &sig,
        )
        .await;
        if let Some(_id) = opened_id {
            opened += 1;
            let entry = crate::security::AuditEntry {
                project_id,
                actor: "system",
                actor_user_id: None,
                actor_session_id: None,
                action: "resource_lint_violation".to_string(),
                resource_kind: if f.port > 0 { "port" } else { "network" },
                resource_id: Some(f.value.clone()),
                outcome: "detected",
                details: serde_json::json!({
                    "file": f.rel_path,
                    "line": f.line,
                    "rule": format!("{kind}/{rule}"),
                }),
            };
            crate::security::record_audit(entry);
        }
    }
    if opened > 0 {
        tracing::warn!(
            project_id = %project_id,
            opened,
            total = findings.len(),
            "resource_linter: violazioni nuove aperte"
        );
        // Realtime: badge problemi + pannello si aggiornano subito.
        nexus_events::dispatcher::emit_global(
            project_id,
            nexus_events::event::ProjectEvent::Notification {
                severity: "error".to_string(),
                message: format!(
                    "Rilevate {opened} violazione/i di governance risorse nei sorgenti (vedi pannello Problemi)"
                ),
                panel: Some("problems".to_string()),
                ttl_ms: Some(15_000),
                run_id: None,
            },
        );
    }
    opened
}

/// Worker periodico: linta tutti i progetti con `port_lint_enabled=true` e
/// innesca la riparazione delle violazioni aperte. Flag e cadenza DB-driven
/// letti a ogni ciclo (regola G).
pub fn spawn_resource_linter(state: crate::AppState) {
    tokio::spawn(async move {
        // Primo giro ritardato: lascia partire i worker di base.
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        loop {
            let enabled = crate::settings::get_setting(
                &state.db,
                "agent.resource_violation.linter_enabled",
            )
            .await
            .ok()
            .flatten()
            .map(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off" | "no"))
            .unwrap_or(true);
            let interval_s = crate::settings::get_setting(
                &state.db,
                "agent.resource_violation.linter_interval_seconds",
            )
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s >= 60)
            .unwrap_or(300);

            if enabled {
                let projects: Vec<(Uuid, String)> = sqlx::query_as(
                    "SELECT id, repository_root_path FROM projects \
                      WHERE COALESCE(repository_root_path, '') <> '' \
                        AND port_lint_enabled = TRUE \
                      ORDER BY created_at DESC LIMIT 50",
                )
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();
                for (pid, root) in projects {
                    let opened = lint_project(&state, pid, &root).await;
                    if opened > 0 {
                        crate::project_workspace::resource_violation_remediation::process_open_violations(
                            &state, pid,
                        )
                        .await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_rileva_porta_fuori_bucket_e_url() {
        let allocated = HashSet::new();
        let f = lint_file_content(
            "server.js",
            "const port = process.env.PORT || 5000;\nfetch('http://localhost:3000/api');\n",
            &allocated,
        );
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::PortOutOfBucket && x.port == 5000));
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::InternalUrlHardcoded));
        // 3000 dentro l'URL non e' una porta "di listen": il port_scanner non
        // la cattura (pattern listen/bind/PORT=), e' coperta dal finding URL.
    }

    #[test]
    fn lint_bucket_non_allocata_vs_allocata() {
        let mut allocated = HashSet::new();
        let f = lint_file_content("app.py", "PORT = 21970\n", &allocated);
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::PortBucketNotAllocated));
        allocated.insert(21970);
        let f = lint_file_content("app.py", "PORT = 21970\n", &allocated);
        assert!(f.is_empty());
    }

    #[test]
    fn lint_skippa_env() {
        let allocated = HashSet::new();
        let f = lint_file_content(".env", "PORT=5000\n", &allocated);
        assert!(f.is_empty());
    }

    #[test]
    fn should_lint_filtra_estensioni() {
        assert!(should_lint_file(Path::new("src/server.js")));
        assert!(should_lint_file(Path::new("docker-compose.yml")));
        assert!(should_lint_file(Path::new("Dockerfile")));
        assert!(!should_lint_file(Path::new("README.md")));
        assert!(!should_lint_file(Path::new("logo.png")));
    }
}
