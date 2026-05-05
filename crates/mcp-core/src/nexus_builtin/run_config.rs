//! Handler per il gruppo `run_config` del server Nexus Builtin.
//!
//! Gestisce le configurazioni di avvio dei progetti (lista, rileva, crea,
//! aggiorna, elimina, avvia). Include anche gli helper condivisi
//! `get_project_root` e `run_git` usati da più sotto-moduli.

use super::*;

// ---------------------------------------------------------------------------
// Helper condivisi (usati anche da git.rs via super::*)
// ---------------------------------------------------------------------------

pub(super) async fn get_project_root(db: &PgPool, project_id: Uuid) -> Result<String, String> {
    sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("[Errore DB] {e}"))?
        .and_then(|r| r.try_get::<Option<String>, _>("repository_root_path").ok().flatten())
        .ok_or_else(|| "[Errore] Progetto non trovato o path non configurata".to_string())
}

pub(super) async fn run_git(root: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .map_err(|e| format!("[Errore esecuzione git] {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() && !stderr.is_empty() {
        return Err(format!("[git error] {stderr}"));
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

// ---------------------------------------------------------------------------
// Handler: run_config
// ---------------------------------------------------------------------------

pub(super) async fn handle_run_config_list(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };

    match sqlx::query(
        "SELECT id, label, kind, command, args, cwd, env, created_at
         FROM run_configurations WHERE project_id=$1 ORDER BY created_at ASC"
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            let configs: Vec<Value> = rows.iter().map(|r| {
                let args_arr: Vec<String> = r.try_get::<Vec<String>, _>("args").unwrap_or_default();
                let env: Value = r.try_get::<Value, _>("env").unwrap_or(json!({}));
                json!({
                    "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                    "label": r.try_get::<String, _>("label").unwrap_or_default(),
                    "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                    "command": r.try_get::<String, _>("command").unwrap_or_default(),
                    "args": args_arr,
                    "cwd": r.try_get::<Option<String>, _>("cwd").unwrap_or(None),
                    "env": env,
                })
            }).collect();
            format_json(&json!({ "configs": configs, "count": configs.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_run_config_detect(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };

    // Legge il root path del progetto
    let root_path = match sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(r)) => r.try_get::<Option<String>, _>("repository_root_path").unwrap_or(None).unwrap_or_default(),
        _ => return "[Errore] Progetto non trovato".to_string(),
    };

    let mut suggestions: Vec<Value> = Vec::new();
    let root = std::path::PathBuf::from(&root_path);

    // Rileva npm scripts
    let pkg_json = root.join("package.json");
    if pkg_json.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&pkg_json).await {
            if let Ok(pkg) = serde_json::from_str::<Value>(&content) {
                if let Some(scripts) = pkg.get("scripts").and_then(Value::as_object) {
                    for (name, _) in scripts {
                        suggestions.push(json!({
                            "label": format!("npm: {name}"),
                            "kind": "npm",
                            "command": "npm",
                            "args": ["run", name.as_str()],
                            "cwd": null,
                        }));
                    }
                }
            }
        }
    }

    // Cargo
    let cargo_toml = root.join("Cargo.toml");
    if cargo_toml.exists() {
        suggestions.push(json!({"label":"cargo build","kind":"cargo","command":"cargo","args":["build"],"cwd":null}));
        suggestions.push(json!({"label":"cargo run","kind":"cargo","command":"cargo","args":["run"],"cwd":null}));
        suggestions.push(json!({"label":"cargo test","kind":"cargo","command":"cargo","args":["test"],"cwd":null}));
    }

    // Python
    if root.join("manage.py").exists() {
        suggestions.push(json!({"label":"Django runserver","kind":"python","command":"python","args":["manage.py","runserver"],"cwd":null}));
    }
    if root.join("main.py").exists() || root.join("app.py").exists() {
        let entry = if root.join("main.py").exists() { "main.py" } else { "app.py" };
        suggestions.push(json!({"label":format!("python {}",entry),"kind":"python","command":"python","args":[entry],"cwd":null}));
    }

    format_json(&json!({ "suggestions": suggestions, "count": suggestions.len() }))
}

pub(super) async fn handle_run_config_create(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let label = match args.get("label").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore] Parametro 'label' obbligatorio".to_string(),
    };
    let command = match args.get("command").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore] Parametro 'command' obbligatorio".to_string(),
    };
    let run_args: Vec<String> = args.get("args").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let cwd: Option<String> = args.get("cwd").and_then(Value::as_str).map(str::to_string);
    let env: Value = args.get("env").cloned().unwrap_or(json!({}));
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("shell").to_string();

    let config_id = Uuid::new_v4();
    match sqlx::query(
        "INSERT INTO run_configurations (id, project_id, label, kind, command, args, cwd, env)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
    )
    .bind(config_id)
    .bind(project_id)
    .bind(&label)
    .bind(&kind)
    .bind(&command)
    .bind(&run_args)
    .bind(&cwd)
    .bind(&env)
    .execute(db)
    .await
    {
        Ok(_) => format_json(&json!({
            "ok": true,
            "id": config_id.to_string(),
            "label": label,
            "command": command,
            "args": run_args,
            "kind": kind,
        })),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_run_config_update(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return e,
    };

    // Legge valori correnti come fallback
    let current = sqlx::query(
        "SELECT label, kind, command, args, cwd, env FROM run_configurations WHERE id=$1 AND project_id=$2"
    )
    .bind(config_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(cur) = current else {
        return "[Errore] Configurazione non trovata".to_string();
    };

    let label = args.get("label").and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("label").unwrap_or("")).to_string();
    let kind = args.get("kind").and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("kind").unwrap_or("shell")).to_string();
    let command = args.get("command").and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("command").unwrap_or("")).to_string();
    let run_args: Vec<String> = args.get("args").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_else(|| cur.try_get::<Vec<String>, _>("args").unwrap_or_default());
    let cwd: Option<String> = args.get("cwd").and_then(Value::as_str).map(str::to_string)
        .or_else(|| cur.try_get("cwd").ok().flatten());
    let env: Value = args.get("env").cloned()
        .unwrap_or_else(|| cur.try_get::<Value, _>("env").unwrap_or(json!({})));

    match sqlx::query(
        "UPDATE run_configurations SET label=$1, kind=$2, command=$3, args=$4, cwd=$5, env=$6, updated_at=NOW()
         WHERE id=$7 AND project_id=$8"
    )
    .bind(&label).bind(&kind).bind(&command).bind(&run_args)
    .bind(&cwd).bind(&env).bind(config_id).bind(project_id)
    .execute(db)
    .await
    {
        Ok(_) => format_json(&json!({ "ok": true, "id": config_id.to_string() })),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_run_config_delete(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return e,
    };

    match sqlx::query("DELETE FROM run_configurations WHERE id=$1 AND project_id=$2")
        .bind(config_id)
        .bind(project_id)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => format_json(&json!({ "ok": true })),
        Ok(_) => "[Errore] Configurazione non trovata o non eliminabile".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_run_config_launch(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return e,
    };

    let row = sqlx::query(
        "SELECT label, command, args, cwd FROM run_configurations WHERE id=$1 AND project_id=$2"
    )
    .bind(config_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return "[Errore] Configurazione non trovata".to_string();
    };

    let label: String = row.try_get("label").unwrap_or_default();
    let command: String = row.try_get("command").unwrap_or_default();
    let run_args: Vec<String> = row.try_get::<Vec<String>, _>("args").unwrap_or_default();
    let config_cwd: Option<String> = row.try_get("cwd").ok().flatten();

    // Risolve la directory di lavoro
    let root_path = sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<Option<String>, _>("repository_root_path").ok().flatten())
        .unwrap_or_default();

    let cwd = match config_cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => {
            let p = std::path::PathBuf::from(c);
            if p.is_absolute() { p } else { std::path::PathBuf::from(&root_path).join(p) }
        }
        None => std::path::PathBuf::from(&root_path),
    };

    let full_cmd = if run_args.is_empty() {
        command.clone()
    } else {
        format!("{} {}", command, run_args.join(" "))
    };

    let project_root = if root_path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&root_path))
    };

    match crate::agent_processes::spawn_agent_process(
        db,
        project_id,
        None,
        &label,
        &full_cmd,
        &cwd.to_string_lossy(),
        project_root,
        None,
        crate::sandbox::sandbox_enabled(),
        "service",
        None,
    ).await {
        Ok(process_id) => format_json(&json!({
            "ok": true,
            "processId": process_id.to_string(),
            "label": label,
            "command": full_cmd,
        })),
        Err(e) => format!("[Errore avvio] {e}"),
    }
}
