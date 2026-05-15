//! Fix M1: parser auto-popola `nexus_port_allocations` dai metadata del progetto.
//!
//! POST /api/projects/:id/services/scan-ports
//!
//! Scansiona:
//! - package.json scripts.dev / scripts.start per `--port N` pattern
//! - vite.config.ts per `server.port = N`
//! - next.config.js per `PORT` env
//! - Procfile per `web: ... -p N`
//! - docker-compose.yml per `ports: - "N:M"`
//!
//! Per ogni porta rilevata fa UPSERT in nexus_port_allocations con label inferita.

use super::*;
use regex::Regex;

/// Fix M31: scansiona il filesystem del progetto e ritorna le porte rilevate.
/// Helper sync senza dipendenze HTTP, riusabile da auto_populate_port_allocations
/// e dall'handler scan_ports REST.
/// Ritorna: Vec<(port, label, source)>
pub fn compute_detected_ports(root: &std::path::Path) -> Vec<(i32, String, String)> {
    let mut detected: Vec<(i32, String, String)> = Vec::new();

    fn scan_file(path: &std::path::Path, patterns: &[(Regex, &str)]) -> Vec<(i32, String)> {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut found = Vec::new();
        for (re, label) in patterns {
            for cap in re.captures_iter(&content) {
                if let Some(m) = cap.get(1) {
                    if let Ok(p) = m.as_str().parse::<i32>() {
                        if p >= 1024 && p < 65535 {
                            found.push((p, label.to_string()));
                        }
                    }
                }
            }
        }
        found
    }

    // 1) package.json (root + frontend/ + backend/)
    let pkg_paths = [
        root.join("package.json"),
        root.join("frontend").join("package.json"),
        root.join("backend").join("package.json"),
    ];
    for (i, pkg) in pkg_paths.iter().enumerate() {
        if !pkg.is_file() {
            continue;
        }
        let label_base = match i {
            0 => "app",
            1 => "frontend",
            2 => "backend",
            _ => "app",
        };
        let patterns: Vec<(Regex, &str)> = vec![
            (Regex::new(r"--port[= ](\d+)").unwrap(), label_base),
            (Regex::new(r#""PORT"\s*:\s*"?(\d+)"?"#).unwrap(), label_base),
            (Regex::new(r"PORT=(\d+)").unwrap(), label_base),
        ];
        for (p, lbl) in scan_file(pkg, &patterns) {
            detected.push((p, lbl, format!("package.json:{}", label_base)));
        }
    }

    // 2) vite.config.ts/js/mjs (frontend)
    for ext in &["ts", "js", "mjs"] {
        let p = root.join("frontend").join(format!("vite.config.{}", ext));
        if !p.is_file() {
            continue;
        }
        let patterns: Vec<(Regex, &str)> = vec![(
            Regex::new(r"port\s*[:=]\s*(\d+)").unwrap(),
            "frontend",
        )];
        for (port, lbl) in scan_file(&p, &patterns) {
            detected.push((port, lbl, "vite.config".to_string()));
        }
    }

    // 3) Procfile
    let procfile = root.join("Procfile");
    if procfile.is_file() {
        let patterns: Vec<(Regex, &str)> = vec![
            (Regex::new(r"-p\s+(\d+)").unwrap(), "app"),
            (Regex::new(r"--port[= ](\d+)").unwrap(), "app"),
        ];
        for (port, lbl) in scan_file(&procfile, &patterns) {
            detected.push((port, lbl, "Procfile".to_string()));
        }
    }

    // 4) docker-compose.yml
    for name in &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let p = root.join(name);
        if !p.is_file() {
            continue;
        }
        let patterns: Vec<(Regex, &str)> = vec![(
            Regex::new(r#"-\s+"?(\d{4,5}):\d{2,5}"?"#).unwrap(),
            "compose",
        )];
        for (port, lbl) in scan_file(&p, &patterns) {
            detected.push((port, lbl, format!("compose:{}", name)));
        }
        break;
    }

    detected
}

/// Fix M31: auto-popola la tabella `nexus_port_allocations` con le porte
/// rilevate scansionando il filesystem. Idempotente via ON CONFLICT DO NOTHING.
/// Chiamata da `register_project` come spawn-and-forget post-insert.
pub async fn auto_populate_port_allocations(
    db: &sqlx::PgPool,
    project_id: Uuid,
    project_root: &std::path::Path,
) {
    let detected = compute_detected_ports(project_root);
    if detected.is_empty() {
        tracing::debug!(
            "auto_populate_port_allocations: nessuna porta rilevata per {}",
            project_id
        );
        return;
    }
    let mut seen = std::collections::HashSet::new();
    let mut inserted = 0_usize;
    for (port, label, _source) in &detected {
        let key = (*port, label.clone());
        if !seen.insert(key) {
            continue;
        }
        let res = sqlx::query(
            r#"
            INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
            VALUES ($1, $2, $3, 'auto-detected')
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(project_id)
        .bind(port)
        .bind(label)
        .execute(db)
        .await;
        if res.map(|r| r.rows_affected() > 0).unwrap_or(false) {
            inserted += 1;
        }
    }
    tracing::info!(
        "auto_populate_port_allocations: {} porte inserite per progetto {}",
        inserted,
        project_id
    );
}

pub async fn scan_ports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = &context.root_path;

    let mut detected: Vec<(i32, String, String)> = Vec::new(); // (port, label, source)

    // Helper per scansionare un file con regex
    async fn scan_file(path: &std::path::Path, patterns: &[(Regex, &str)]) -> Vec<(i32, String)> {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut found = Vec::new();
        for (re, label) in patterns {
            for cap in re.captures_iter(&content) {
                if let Some(m) = cap.get(1) {
                    if let Ok(p) = m.as_str().parse::<i32>() {
                        if p >= 1024 && p < 65535 {
                            found.push((p, label.to_string()));
                        }
                    }
                }
            }
        }
        found
    }

    // 1) package.json (root + frontend/ + backend/)
    let pkg_paths = [
        root.join("package.json"),
        root.join("frontend").join("package.json"),
        root.join("backend").join("package.json"),
    ];
    for (i, pkg) in pkg_paths.iter().enumerate() {
        if !pkg.is_file() {
            continue;
        }
        let label_base = match i {
            0 => "app",
            1 => "frontend",
            2 => "backend",
            _ => "app",
        };
        let patterns = vec![
            (Regex::new(r"--port[= ](\d+)").unwrap(), label_base),
            (Regex::new(r#""PORT"\s*:\s*"?(\d+)"?"#).unwrap(), label_base),
            (Regex::new(r"PORT=(\d+)").unwrap(), label_base),
        ];
        let found = scan_file(pkg, &patterns).await;
        for (p, lbl) in found {
            detected.push((p, lbl, format!("package.json:{}", label_base)));
        }
    }

    // 2) vite.config.ts/js/mjs (frontend)
    for ext in &["ts", "js", "mjs"] {
        let p = root.join("frontend").join(format!("vite.config.{}", ext));
        if !p.is_file() {
            continue;
        }
        let patterns = vec![
            (
                Regex::new(r"port\s*[:=]\s*(\d+)").unwrap(),
                "frontend",
            ),
        ];
        let found = scan_file(&p, &patterns).await;
        for (port, lbl) in found {
            detected.push((port, lbl, "vite.config".to_string()));
        }
    }

    // 3) Procfile
    let procfile = root.join("Procfile");
    if procfile.is_file() {
        let patterns = vec![
            (Regex::new(r"-p\s+(\d+)").unwrap(), "app"),
            (Regex::new(r"--port[= ](\d+)").unwrap(), "app"),
        ];
        let found = scan_file(&procfile, &patterns).await;
        for (port, lbl) in found {
            detected.push((port, lbl, "Procfile".to_string()));
        }
    }

    // 4) docker-compose.yml
    for name in &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let p = root.join(name);
        if !p.is_file() {
            continue;
        }
        let patterns = vec![
            (Regex::new(r#"-\s+"?(\d{4,5}):\d{2,5}"?"#).unwrap(), "compose"),
        ];
        let found = scan_file(&p, &patterns).await;
        for (port, lbl) in found {
            detected.push((port, lbl, format!("compose:{}", name)));
        }
        break;
    }

    // Deduplica per (port, label) e UPSERT
    let mut seen = std::collections::HashSet::new();
    let mut inserted = Vec::new();
    for (port, label, source) in &detected {
        let key = (*port, label.clone());
        if !seen.insert(key) {
            continue;
        }
        let result = sqlx::query(
            r#"
            INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
            VALUES ($1, $2, $3, 'auto-detected')
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(project_id)
        .bind(port)
        .bind(label)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        if result.is_some() {
            inserted.push(json!({"port": port, "label": label, "source": source}));
        }
    }

    Ok(Json(json!({
        "ok": true,
        "detected_count": detected.len(),
        "inserted_count": inserted.len(),
        "inserted": inserted,
        "raw_detections": detected.iter().map(|(p, l, s)| json!({"port": p, "label": l, "source": s})).collect::<Vec<_>>(),
    })))
}
