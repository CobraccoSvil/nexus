use super::*;
use super::wizard::{
    collect_workspace_dirs, collect_cargo_workspace_members, collect_compose_files,
    parse_compose_services, classify_role, is_essential,
    push_sugg, compose_file_rank, extract_make_target_body,
    detect_dotnet_suggestions, detect_playwright_suggestions,
    refine_with_nexus,
};
use super::services::{NEXUS_RESERVED_PORTS, find_free_port, find_free_project_port, deterministic_project_port_for_key, is_web_service_script};

/// Scans a project directory and returns suggested run configurations (con tag role/essential/group).
/// Calcola i suggerimenti di run-config scansionando il filesystem.
/// Funzione pura: non legge né scrive DB, non chiama AI.
/// Usata da `detect_run_configs` (con cache) e da `analyze_project` (pre-popola cache).
pub fn compute_run_config_suggestions(root: &std::path::Path) -> Vec<Value> {
    let mut suggestions: Vec<Value> = Vec::new();

    // === JS / Node (monorepo-aware) ===
    let pkg_dirs = collect_workspace_dirs(root);
    for pkg_dir in &pkg_dirs {
        let pkg_path = pkg_dir.join("package.json");
        if !pkg_path.exists() { continue; }
        let content = match std::fs::read_to_string(&pkg_path) { Ok(c) => c, Err(_) => continue };
        let pkg: Value = match serde_json::from_str(&content) { Ok(v) => v, Err(_) => continue };

        let is_root = pkg_dir == root;
        let rel_label = if is_root {
            pkg.get("name").and_then(|n| n.as_str()).unwrap_or("root").to_string()
        } else {
            pkg_dir.strip_prefix(root).ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| pkg_dir.file_name().unwrap_or_default().to_string_lossy().to_string())
        };
        let group_label = rel_label.clone();
        let prefix = format!("[{}] ", rel_label);
        let cwd_val: Value = if is_root { Value::Null } else { json!(pkg_dir.to_string_lossy()) };

        let pkg_manager = {
            let declared = pkg.get("packageManager").and_then(|v| v.as_str()).unwrap_or("");
            if declared.starts_with("pnpm") { "pnpm" }
            else if declared.starts_with("yarn") { "yarn" }
            else if declared.starts_with("bun") { "bun" }
            else if pkg_dir.join("pnpm-lock.yaml").exists() || root.join("pnpm-lock.yaml").exists() { "pnpm" }
            else if pkg_dir.join("yarn.lock").exists() || root.join("yarn.lock").exists() { "yarn" }
            else if pkg_dir.join("bun.lockb").exists() || pkg_dir.join("bun.lock").exists() { "bun" }
            else { "npm" }
        };

        if let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) {
            let priority = ["dev", "start", "serve", "build", "test", "preview", "lint"];
            for name in &priority {
                if !scripts.contains_key(*name) { continue; }
                let env_val = if is_web_service_script(name) {
                    json!({ "PORT": "5000" })
                } else { json!({}) };
                let role = classify_role("npm", name, Some(&pkg));
                let essential = is_essential(role, name, "npm");
                push_sugg(&mut suggestions,
                    format!("{}{} {}", prefix, pkg_manager, name),
                    "npm", pkg_manager,
                    vec![json!("run"), json!(name)],
                    cwd_val.clone(), env_val, role, essential, group_label.clone());
            }
            let mut count = 0;
            for name in scripts.keys() {
                if priority.contains(&name.as_str()) { continue; }
                if count >= 3 { break; }
                let role = classify_role("npm", name, Some(&pkg));
                push_sugg(&mut suggestions,
                    format!("{}{} {}", prefix, pkg_manager, name),
                    "npm", pkg_manager,
                    vec![json!("run"), json!(name)],
                    cwd_val.clone(), json!({}), role, false, group_label.clone());
                count += 1;
            }
        }
    }

    // === Cargo (workspace-aware) ===
    let cargo_root = root.join("Cargo.toml");
    if cargo_root.exists() {
        let members = collect_cargo_workspace_members(root);
        if !members.is_empty() {
            for m in &members {
                if !m.join("src/main.rs").exists() { continue; }
                let cargo_toml = m.join("Cargo.toml");
                let pkg_name = std::fs::read_to_string(&cargo_toml).ok()
                    .and_then(|c| c.lines().find(|l| l.trim().starts_with("name")).map(|l| l.to_string()))
                    .and_then(|l| l.split('"').nth(1).map(|s| s.to_string()))
                    .unwrap_or_else(|| m.file_name().unwrap_or_default().to_string_lossy().to_string());
                let rel = m.strip_prefix(root).ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|| pkg_name.clone());
                push_sugg(&mut suggestions,
                    format!("cargo run -p {}", pkg_name),
                    "cargo", "cargo",
                    vec![json!("run"), json!("-p"), json!(pkg_name.clone())],
                    Value::Null, json!({}), "backend", true, rel);
            }
            push_sugg(&mut suggestions,
                "cargo build --release (workspace)".to_string(), "cargo", "cargo",
                vec![json!("build"), json!("--release")], Value::Null, json!({}),
                "tool", false, "crates".to_string());
            push_sugg(&mut suggestions,
                "cargo test (workspace)".to_string(), "cargo", "cargo",
                vec![json!("test")], Value::Null, json!({}),
                "test", false, "crates".to_string());
        } else {
            let content = std::fs::read_to_string(&cargo_root).unwrap_or_default();
            let has_main = root.join("src/main.rs").exists();
            let package_name = content.lines()
                .find(|l| l.trim().starts_with("name"))
                .and_then(|l| l.split('"').nth(1))
                .unwrap_or("app").to_string();
            if has_main {
                push_sugg(&mut suggestions,
                    format!("cargo run ({})", package_name),
                    "cargo", "cargo",
                    vec![json!("run")], Value::Null, json!({}),
                    "backend", true, "cargo".to_string());
            }
            push_sugg(&mut suggestions,
                "cargo build --release".to_string(), "cargo", "cargo",
                vec![json!("build"), json!("--release")], Value::Null, json!({}),
                "tool", false, "cargo".to_string());
            push_sugg(&mut suggestions,
                "cargo test".to_string(), "cargo", "cargo",
                vec![json!("test")], Value::Null, json!({}),
                "test", false, "cargo".to_string());
        }
    }

    // === Python ===
    for entry in &[
        ("main.py", "python main.py", vec!["main.py"]),
        ("app.py", "python app.py", vec!["app.py"]),
        ("manage.py", "python manage.py runserver", vec!["manage.py", "runserver"]),
        ("wsgi.py", "gunicorn app:app", vec!["app:app"]),
    ] {
        if root.join(entry.0).exists() {
            push_sugg(&mut suggestions,
                entry.1.to_string(), "python", "python",
                entry.2.iter().map(|s| json!(s)).collect(),
                Value::Null, json!({}), "backend", true, "python".to_string());
        }
    }

    // === Makefile ===
    let make_path = root.join("Makefile");
    if make_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&make_path) {
            let targets: Vec<&str> = content.lines()
                .filter(|l| !l.starts_with('\t') && !l.starts_with('#') && l.contains(':') && !l.contains('='))
                .filter_map(|l| l.split(':').next())
                .filter(|t| !t.is_empty() && !t.starts_with('.'))
                .take(8)
                .collect();
            for target in targets {
                let body = extract_make_target_body(&content, target);
                let wraps_docker = body.contains("docker compose")
                    || body.contains("docker-compose")
                    || body.contains("docker build")
                    || body.contains("docker run");
                let (role, essential, group) = if wraps_docker {
                    ("service", true, "docker".to_string())
                } else {
                    (classify_role("shell", target, None), false, "make".to_string())
                };
                push_sugg(&mut suggestions,
                    format!("make {}", target),
                    "shell", "make",
                    vec![json!(target)], Value::Null, json!({}),
                    role, essential, group);
            }
        }
    }

    // === docker-compose ===
    // Scan dinamico: matcha docker-compose*.yml|yaml e compose*.yml|yaml con
    // priorità dev > local > base > prod. Così varianti come
    // docker-compose.dev.yml, compose.prod.yaml, ecc. vengono rilevate.
    let compose_files = collect_compose_files(root);
    let has_dev_variant = compose_files.iter()
        .any(|p| compose_file_rank(p) == 0);
    for compose_path in &compose_files {
        let fname = compose_path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rank = compose_file_rank(compose_path);
        // Essential: file dev sempre essential, base solo se non esiste un dev.
        let base_essential = rank == 0 || (rank == 2 && !has_dev_variant);
        push_sugg(&mut suggestions,
            format!("docker compose -f {} up", fname),
            "shell", "docker",
            vec![json!("compose"), json!("-f"), json!(fname.clone()),
                 json!("up"), json!("--build")],
            Value::Null, json!({}),
            "service", base_essential, "docker".to_string());
        let services = parse_compose_services(compose_path);
        let single_svc = services.len() == 1;
        for (i, svc) in services.iter().take(20).enumerate() {
            // Se il file è dev/local ed è il servizio principale (primo o unico) → essential.
            let svc_essential = (rank == 0 || rank == 1) && (single_svc || i == 0);
            push_sugg(&mut suggestions,
                format!("docker compose -f {} up {}", fname, svc),
                "shell", "docker",
                vec![json!("compose"), json!("-f"), json!(fname.clone()),
                     json!("up"), json!("--build"), json!(svc.clone())],
                Value::Null, json!({}),
                "service", svc_essential, "docker".to_string());
        }
    }

    // === Dockerfile (senza compose) ===
    if root.join("Dockerfile").exists() && compose_files.is_empty() {
        let name = root.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        push_sugg(&mut suggestions,
            "docker build & run".to_string(), "shell", "sh",
            vec![json!("-c"), json!(format!("docker build -t {name} . && docker run --rm -p 8080:8080 {name}"))],
            Value::Null, json!({}),
            "service", true, "docker".to_string());
    }

    // === Go ===
    if root.join("go.mod").exists() {
        push_sugg(&mut suggestions, "go run . [richiede Go SDK]".to_string(), "shell", "go",
            vec![json!("run"), json!(".")], Value::Null, json!({}),
            "backend", false, "go".to_string());
        push_sugg(&mut suggestions, "go test ./...".to_string(), "shell", "go",
            vec![json!("test"), json!("./...")], Value::Null, json!({}),
            "test", false, "go".to_string());
    }

    // === .NET ===
    suggestions.extend(detect_dotnet_suggestions(root));

    // === Playwright ===
    suggestions.extend(detect_playwright_suggestions(root));

    suggestions
}

/// Fix M30: auto-popola la tabella `run_configurations` dai suggerimenti rilevati
/// nel filesystem. Chiamata da `register_project` come spawn-and-forget post-insert,
/// cosi' il pannello Run & Debug mostra subito i run config (npm dev, npm test, ecc.)
/// senza richiedere il click "Configura" manuale.
/// Idempotente: se esistono gia' run_config per il progetto, skippa.
pub async fn auto_populate_run_configs(
    db: &sqlx::PgPool,
    project_id: Uuid,
    project_root: &std::path::Path,
) {
    // Idempotenza: se ci sono gia' record, lascia stare (l'utente potrebbe averli editati).
    let already: i64 = match sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM run_configurations WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("auto_populate_run_configs: count fallita: {e}");
            return;
        }
    };
    if already > 0 {
        tracing::debug!("auto_populate_run_configs: skip (gia' {} record)", already);
        return;
    }

    let suggestions = compute_run_config_suggestions(project_root);
    if suggestions.is_empty() {
        tracing::info!(
            "auto_populate_run_configs: nessun suggerimento rilevato per {}",
            project_id
        );
        return;
    }

    let mut inserted = 0_usize;
    for s in &suggestions {
        let label = s.get("label").and_then(|v| v.as_str()).unwrap_or("run").to_string();
        let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("shell").to_string();
        let command = s.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if command.is_empty() {
            continue;
        }
        let args: Vec<String> = s
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = s.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
        let env = s.get("env").cloned().unwrap_or(json!({}));
        let role = s.get("role").and_then(|v| v.as_str()).map(str::to_string);
        let essential = s.get("essential").and_then(|v| v.as_bool()).unwrap_or(false);
        let group = s.get("group").and_then(|v| v.as_str()).map(str::to_string);

        let res = sqlx::query(
            "INSERT INTO run_configurations \
             (id, project_id, label, kind, command, args, cwd, env, role, essential, group_label) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(Uuid::new_v4())
        .bind(project_id)
        .bind(&label)
        .bind(&kind)
        .bind(&command)
        .bind(&args)
        .bind(&cwd)
        .bind(&env)
        .bind(&role)
        .bind(essential)
        .bind(&group)
        .execute(db)
        .await;
        if res.is_ok() {
            inserted += 1;
        }
    }
    // Salva anche cache suggerimenti per UI "rigenera"
    save_suggestions_cache(db, project_id, &suggestions).await;
    tracing::info!(
        "auto_populate_run_configs: {}/{} run_config inseriti per progetto {}",
        inserted,
        suggestions.len(),
        project_id
    );
}

/// Salva i suggerimenti rilevati nella cache DB del progetto.
pub async fn save_suggestions_cache(db: &sqlx::PgPool, project_id: Uuid, suggestions: &[Value]) {
    let json_val = serde_json::to_value(suggestions).unwrap_or(Value::Null);
    let _ = sqlx::query(
        "UPDATE projects SET detected_suggestions = $1, detected_suggestions_at = NOW() WHERE id = $2",
    )
    .bind(json_val)
    .bind(project_id)
    .execute(db)
    .await;
}

/// GET /api/projects/:id/run-configs/detect
///
/// Logica cache:
/// - Se `?force=1` oppure analisi > 7 giorni: riscansiona filesystem e aggiorna cache.
/// - Altrimenti: restituisce i suggerimenti già calcolati (source = "cached").
/// - Con `?use_ai=1`: dopo la scansione chiama Nexus per raffinare role/essential.
pub async fn detect_run_configs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = &context.root_path;
    let use_ai   = params.get("use_ai").map(|v| v == "1" || v == "true").unwrap_or(false);
    let force    = params.get("force").map(|v| v == "1" || v == "true").unwrap_or(false);

    // --- Prova a leggere dalla cache DB ---
    if !force {
        let row = sqlx::query(
            "SELECT detected_suggestions, detected_suggestions_at FROM projects WHERE id = $1",
        )
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

        if let Some(r) = row {
            let detected_at: Option<chrono::DateTime<chrono::Utc>> =
                r.try_get("detected_suggestions_at").ok().flatten();
            let is_fresh = detected_at.map(|t| {
                let age = chrono::Utc::now().signed_duration_since(t);
                age.num_days() < 7
            }).unwrap_or(false);

            if is_fresh {
                let cached: Option<Value> = r.try_get("detected_suggestions").ok().flatten();
                if let Some(cached) = cached {
                    return Ok(Json(json!({ "suggestions": cached, "source": "cached" })));
                }
            }
        }
    }

    // --- Cache assente/stale: riscansiona ---
    let mut suggestions = compute_run_config_suggestions(root);

    // Normalizza le porte già in fase di analisi: i progetti devono usare bucket deterministico,
    // e non devono mai proporre porte riservate (es. 3000).
    for s in &mut suggestions {
        let Some(obj) = s.as_object_mut() else { continue; };
        // Clone dei campi usati come key, per evitare borrow immutabile+mutabile su `obj`.
        let label: String = obj.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let role: String = obj.get("role").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let lower_label = label.to_lowercase();
        let is_webish = role == "web" || role == "frontend" || lower_label.contains("dev") || lower_label.contains("serve");
        if !is_webish { continue; }
        let env = obj.entry("env").or_insert_with(|| json!({}));
        let Some(env_obj) = env.as_object_mut() else { continue; };
        // Se l'heuristica aveva messo PORT=5000 o non c'è PORT, scegli una porta deterministica per questa run-config.
        let port_missing = !env_obj.contains_key("PORT");
        let port_is_default = env_obj.get("PORT").and_then(|v| v.as_str()).map(|v| v == "5000").unwrap_or(false);
        if port_missing || port_is_default {
            let p = deterministic_project_port_for_key(&project_id, &label, &state.port_registry).await;
            env_obj.insert("PORT".to_string(), json!(p.to_string()));
        }
    }

    // Rifinitura AI opzionale
    let source = if use_ai && !suggestions.is_empty() {
        refine_with_nexus(&state, project_id, user_id, root, &mut suggestions).await;
        "ai"
    } else {
        "heuristic"
    };

    // Aggiorna cache
    save_suggestions_cache(&state.db, project_id, &suggestions).await;

    Ok(Json(json!({ "suggestions": suggestions, "source": source })))
}

// walkdir_specs e' definita in mod.rs (serve anche a wizard.rs)

pub async fn get_run_configs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let rows = sqlx::query(
        "SELECT id, label, kind, command, args, cwd, env, role, essential, group_label, created_at FROM run_configurations WHERE project_id = $1 ORDER BY created_at ASC"
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let configs: Vec<serde_json::Value> = rows.iter().map(|row| {
        let args: Vec<String> = row.try_get::<Vec<String>, _>("args").unwrap_or_default();
        let env: serde_json::Value = row.try_get::<serde_json::Value, _>("env").unwrap_or(json!({}));
        json!({
            "id": row.get::<Uuid, _>("id").to_string(),
            "label": row.get::<String, _>("label"),
            "kind": row.get::<String, _>("kind"),
            "command": row.get::<String, _>("command"),
            "args": args,
            "cwd": row.try_get::<Option<String>, _>("cwd").unwrap_or(None),
            "env": env,
            "role": row.try_get::<Option<String>, _>("role").unwrap_or(None),
            "essential": row.try_get::<bool, _>("essential").unwrap_or(false),
            "group": row.try_get::<Option<String>, _>("group_label").unwrap_or(None),
        })
    }).collect();

    Ok(Json(json!({ "configs": configs })))
}

#[derive(serde::Deserialize)]
pub struct CreateRunConfigBody {
    pub label: String,
    pub kind: String,
    pub command: String,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<serde_json::Value>,
    pub role: Option<String>,
    pub essential: Option<bool>,
    #[serde(alias = "group_label")]
    pub group: Option<String>,
}

pub async fn create_run_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CreateRunConfigBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let config_id = Uuid::new_v4();
    let args = body.args.unwrap_or_default();
    let env = body.env.unwrap_or(json!({}));
    let essential = body.essential.unwrap_or(false);

    sqlx::query(
        "INSERT INTO run_configurations (id, project_id, label, kind, command, args, cwd, env, role, essential, group_label) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)"
    )
    .bind(config_id)
    .bind(project_id)
    .bind(&body.label)
    .bind(&body.kind)
    .bind(&body.command)
    .bind(&args)
    .bind(&body.cwd)
    .bind(&env)
    .bind(&body.role)
    .bind(essential)
    .bind(&body.group)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "id": config_id.to_string(),
        "label": body.label,
        "kind": body.kind,
        "command": body.command,
        "args": args,
        "cwd": body.cwd,
        "env": env,
        "role": body.role,
        "essential": essential,
        "group": body.group,
    })))
}

pub async fn update_run_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, config_id_str)): AxumPath<(String, String)>,
    Json(body): Json<CreateRunConfigBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let config_id = Uuid::parse_str(&config_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Config id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let args = body.args.unwrap_or_default();
    let env = body.env.unwrap_or(json!({}));
    let essential = body.essential.unwrap_or(false);

    sqlx::query(
        "UPDATE run_configurations SET label=$1, kind=$2, command=$3, args=$4, cwd=$5, env=$6, role=$7, essential=$8, group_label=$9, updated_at=NOW() WHERE id=$10 AND project_id=$11"
    )
    .bind(&body.label)
    .bind(&body.kind)
    .bind(&body.command)
    .bind(&args)
    .bind(&body.cwd)
    .bind(&env)
    .bind(&body.role)
    .bind(essential)
    .bind(&body.group)
    .bind(config_id)
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_run_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, config_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let config_id = Uuid::parse_str(&config_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Config id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    sqlx::query("DELETE FROM run_configurations WHERE id=$1 AND project_id=$2")
        .bind(config_id)
        .bind(project_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

pub async fn launch_run_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, config_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let config_id = Uuid::parse_str(&config_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Config id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let row = sqlx::query(
        "SELECT label, kind, command, args, cwd, env FROM run_configurations WHERE id=$1 AND project_id=$2"
    )
    .bind(config_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Configurazione non trovata"))?;

    let label: String = row.get("label");
    let command: String = row.get("command");
    let args: Vec<String> = row.try_get::<Vec<String>, _>("args").unwrap_or_default();
    let config_cwd: Option<String> = row.try_get("cwd").unwrap_or(None);
    let env_json: serde_json::Value = row.try_get::<serde_json::Value, _>("env")
        .unwrap_or(serde_json::Value::Null);

    let cwd = match config_cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => {
            let p = std::path::PathBuf::from(c);
            if p.is_absolute() { p } else { context.root_path.join(p) }
        }
        None => context.root_path.clone(),
    };

    if !cwd.exists() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("Working directory non trovata: {}", cwd.display()),
        ));
    }

    fn parse_port_token(s: &str) -> Option<u16> {
        let t = s.trim();
        if t.is_empty() { return None; }
        // strip common wrappers
        let t = t.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
        t.parse::<u16>().ok()
    }

    fn extract_cli_port_hint(command: &str) -> Option<u16> {
        // Best-effort: detect common patterns like "--port 3000", "--port=3000", "-p 3000", "-p3000"
        let tokens: Vec<&str> = command.split_whitespace().collect();
        let mut i = 0usize;
        while i < tokens.len() {
            let tok = tokens[i];
            if tok == "--port" || tok == "-p" {
                if let Some(next) = tokens.get(i + 1).and_then(|v| parse_port_token(v)) {
                    return Some(next);
                }
            }
            if let Some(v) = tok.strip_prefix("--port=") {
                if let Some(p) = parse_port_token(v) { return Some(p); }
            }
            if let Some(v) = tok.strip_prefix("-p") {
                if v.len() >= 2 {
                    if let Some(p) = parse_port_token(v) { return Some(p); }
                }
            }
            i += 1;
        }
        None
    }

    fn looks_like_web_server_command(command: &str) -> bool {
        let lower = command.to_lowercase();
        // Node/web frameworks (common in managed projects)
        lower.contains(" next dev")
            || lower.contains(" next start")
            || lower.contains(" vite")
            || lower.contains(" nuxt")
            || lower.contains(" astro")
            || lower.contains(" react-scripts start")
            || lower.contains(" serve ")
            || lower.contains(" pnpm run dev")
            || lower.contains(" npm run dev")
            || lower.contains(" yarn dev")
            || lower.contains(" pnpm dev")
            || lower.contains(" npm start")
            || lower.contains(" dotnet run")
    }

    fn rewrite_port_flags(command: &str, target_port: u16) -> String {
        // Minimal rewriting to avoid conflicts with Nexus reserved ports.
        // This does NOT attempt to be a shell parser; it covers common dev flags.
        let p = target_port.to_string();
        let mut out = command.to_string();
        for bad in ["3000", "4000", "4010", "4020", "4030", "4040", "4050", "4060", "8001"] {
            out = out.replace(&format!("--port={}", bad), &format!("--port={}", p));
            out = out.replace(&format!("--port {}", bad), &format!("--port {}", p));
            out = out.replace(&format!("-p {}", bad), &format!("-p {}", p));
            out = out.replace(&format!("-p{}", bad), &format!("-p{}", p));
            out = out.replace(&format!("localhost:{}", bad), &format!("localhost:{}", p));
            out = out.replace(&format!("127.0.0.1:{}", bad), &format!("127.0.0.1:{}", p));
        }
        // If it's clearly Next and no explicit port flag, add one (Next dev/start does not reliably honor PORT env).
        let lower = out.to_lowercase();
        let has_flag = lower.contains("--port") || lower.split_whitespace().any(|t| t == "-p" || t.starts_with("-p"));
        if (lower.contains("next dev") || lower.contains("next start")) && !has_flag {
            out.push_str(&format!(" -p {}", p));
        }
        out
    }

    // Build env vars come HashMap — passate direttamente alla sandbox Docker.
    // Non si usa più il prefisso shell "KEY=val CMD" per evitare injection e
    // per garantire che le variabili non siano visibili nell'output di `ps`.
    let mut env_vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(env_obj) = env_json.as_object() {
        for (k, v) in env_obj {
            let val_str = v.as_str().unwrap_or("").to_string();
            if k == "PORT" {
                let configured_port: u16 = val_str.parse().unwrap_or(5000);
                let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
                let is_free = !reserved.contains(&configured_port)
                    && state.port_registry.is_port_available(configured_port).await
                    && tokio::net::TcpListener::bind(format!("127.0.0.1:{}", configured_port)).await.is_ok();
                let actual_port = if is_free { configured_port } else { find_free_project_port(&project_id, &state.port_registry).await };
                env_vars.insert("PORT".to_string(), actual_port.to_string());
            } else {
                env_vars.insert(k.clone(), val_str);
            }
        }
    }

    // Build full command string (args appendati al comando base)
    let full_cmd_raw = if args.is_empty() {
        command.clone()
    } else {
        format!("{} {}", command, args.join(" "))
    };

    // Guardrail: i servizi di progetto non devono mai usare porte riservate (incl. 3000).
    // Se il run config non imposta PORT ma il comando sembra un web server (o contiene un hint porta),
    // assegniamo una porta dal range progetti (5000+) e riscriviamo i flag più comuni.
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let configured_hint = extract_cli_port_hint(&full_cmd_raw);
    let should_force_port = looks_like_web_server_command(&full_cmd_raw) || configured_hint.is_some();
    let forced_port: Option<u16> = if should_force_port && !env_vars.contains_key("PORT") {
        Some(find_free_project_port(&project_id, &state.port_registry).await)
    } else {
        env_vars.get("PORT").and_then(|s| parse_port_token(s))
    };

    if let Some(p) = forced_port {
        if reserved.contains(&p) {
            // extra safety; should not happen because find_free_port excludes reserved
            let safe = find_free_project_port(&project_id, &state.port_registry).await;
            env_vars.insert("PORT".to_string(), safe.to_string());
        } else if !env_vars.contains_key("PORT") {
            env_vars.insert("PORT".to_string(), p.to_string());
        }
    }

    let final_port = env_vars.get("PORT").and_then(|s| parse_port_token(s)).unwrap_or(0);
    let full_cmd = if final_port > 0 { rewrite_port_flags(&full_cmd_raw, final_port) } else { full_cmd_raw };

    // Se l'immagine del progetto è già stata buildata in precedenza, usala.
    // Non buildare automaticamente qui — il build è lento e causerebbe timeout.
    // Usare il tool build_project_image o l'endpoint /build-image per buildare.
    let service_image = if state.sandbox_available {
        crate::sandbox::check_project_service_image(
            project_id,
            &context.root_path,
            &cwd,
        ).await
    } else {
        None
    };

    let process_id = crate::agent_processes::spawn_agent_process(
        &state.db,
        project_id,
        None,
        &label,
        &full_cmd,
        &cwd.to_string_lossy(),
        Some(context.root_path.clone()),
        Some(env_vars),
        state.sandbox_available,
        "service",
        service_image,
    ).await.map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(json!({
        "ok": true,
        "processId": process_id.to_string(),
        "channelId": format!("agent:{}", process_id),
    })))
}
