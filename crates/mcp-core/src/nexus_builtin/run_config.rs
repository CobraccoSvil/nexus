//! Handler per il gruppo `run_config` del server Nexus Builtin.
//!
//! Gestisce le configurazioni di avvio dei progetti (lista, rileva, crea,
//! aggiorna, elimina, avvia). Include anche gli helper condivisi
//! `get_project_root` e `run_git` usati da più sotto-moduli.

use super::*;

/// Costruisce l'esito FALLITO di una query DB in questo file: marker piu'
/// messaggio leggibile (contratto `nexus_types::tool_outcome`). Estrae la
/// ripetizione dei 4 siti `Err(e) => format!("[Errore DB] {e}")`: senza
/// marker questi fallimenti erano indistinguibili da un successo per
/// anti-loop/supervisore/final_gate (regola M).
fn db_tool_failure(e: impl std::fmt::Display) -> String {
    tool_failure(format!("[Errore DB] {e}"))
}

// ---------------------------------------------------------------------------
// Helper condivisi (usati anche da git.rs via super::*)
// ---------------------------------------------------------------------------

pub(super) async fn get_project_root(db: &PgPool, project_id: Uuid) -> Result<String, String> {
    sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("[Errore DB] {e}"))?
        .and_then(|r| {
            r.try_get::<Option<String>, _>("repository_root_path")
                .ok()
                .flatten()
        })
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
        Err(e) => return tool_failure(e),
    };

    match sqlx::query(
        "SELECT id, label, kind, command, args, cwd, env, created_at
         FROM run_configurations WHERE project_id=$1 ORDER BY created_at ASC",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            let configs: Vec<Value> = rows
                .iter()
                .map(|r| {
                    let args_arr: Vec<String> =
                        r.try_get::<Vec<String>, _>("args").unwrap_or_default();
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
                })
                .collect();
            format_json(&json!({ "configs": configs, "count": configs.len() }))
        }
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_run_config_detect(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };

    // Legge il root path del progetto
    let root_path = match sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(r)) => r
            .try_get::<Option<String>, _>("repository_root_path")
            .unwrap_or(None)
            .unwrap_or_default(),
        _ => return tool_failure("[Errore] Progetto non trovato"),
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
        suggestions.push(
            json!({"label":"cargo run","kind":"cargo","command":"cargo","args":["run"],"cwd":null}),
        );
        suggestions.push(json!({"label":"cargo test","kind":"cargo","command":"cargo","args":["test"],"cwd":null}));
    }

    // Python
    if root.join("manage.py").exists() {
        suggestions.push(json!({"label":"Django runserver","kind":"python","command":"python","args":["manage.py","runserver"],"cwd":null}));
    }
    if root.join("main.py").exists() || root.join("app.py").exists() {
        let entry = if root.join("main.py").exists() {
            "main.py"
        } else {
            "app.py"
        };
        suggestions.push(json!({"label":format!("python {}",entry),"kind":"python","command":"python","args":[entry],"cwd":null}));
    }

    format_json(&json!({ "suggestions": suggestions, "count": suggestions.len() }))
}

pub(super) async fn handle_run_config_create(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let label = match args.get("label").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'label' obbligatorio"),
    };
    let command = match args.get("command").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'command' obbligatorio"),
    };
    let run_args: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let cwd: Option<String> = args.get("cwd").and_then(Value::as_str).map(str::to_string);
    let env: Value = args.get("env").cloned().unwrap_or(json!({}));
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("shell")
        .to_string();

    let config_id = Uuid::new_v4();
    match sqlx::query(
        "INSERT INTO run_configurations (id, project_id, label, kind, command, args, cwd, env)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
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
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_run_config_update(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
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
        return tool_failure("[Errore] Configurazione non trovata");
    };

    let label = args
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("label").unwrap_or(""))
        .to_string();
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("kind").unwrap_or("shell"))
        .to_string();
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_else(|| cur.try_get("command").unwrap_or(""))
        .to_string();
    let run_args: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| cur.try_get::<Vec<String>, _>("args").unwrap_or_default());
    let cwd: Option<String> = args
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| cur.try_get("cwd").ok().flatten());
    let env: Value = args
        .get("env")
        .cloned()
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
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_run_config_delete(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };

    match sqlx::query("DELETE FROM run_configurations WHERE id=$1 AND project_id=$2")
        .bind(config_id)
        .bind(project_id)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => format_json(&json!({ "ok": true })),
        Ok(_) => tool_failure("[Errore] Configurazione non trovata o non eliminabile"),
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_run_config_launch(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let config_id = match parse_uuid(args, "config_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };

    let row = sqlx::query(
        "SELECT label, command, args, cwd, role FROM run_configurations WHERE id=$1 AND project_id=$2",
    )
    .bind(config_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return tool_failure("[Errore] Configurazione non trovata");
    };

    let label: String = row.try_get("label").unwrap_or_default();
    let command: String = row.try_get("command").unwrap_or_default();
    let run_args: Vec<String> = row.try_get::<Vec<String>, _>("args").unwrap_or_default();
    let config_cwd: Option<String> = row.try_get("cwd").ok().flatten();
    let role: Option<String> = row.try_get("role").ok().flatten();

    // Risolve la directory di lavoro
    let root_path = sqlx::query("SELECT repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|r| {
            r.try_get::<Option<String>, _>("repository_root_path")
                .ok()
                .flatten()
        })
        .unwrap_or_default();

    let cwd = match config_cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(c) => {
            let p = std::path::PathBuf::from(c);
            if p.is_absolute() {
                p
            } else {
                std::path::PathBuf::from(&root_path).join(p)
            }
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

    let kind = crate::agent_processes::kind_for_run_config_role(role.as_deref());
    if kind == "service" {
        // PUNTO UNICO anti-duplicato (regola L): come run_service e wizard
        // install, il lancio di una run config servizio ferma prima i processi
        // running dello stesso scopo (label esatta o variante simile).
        let _ = crate::agent_processes::stop_similar_running_services(db, project_id, &label).await;
    }
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
        kind,
        None,
    )
    .await
    {
        Ok(process_id) => format_json(&json!({
            "ok": true,
            "processId": process_id.to_string(),
            "label": label,
            "command": full_cmd,
        })),
        Err(e) => tool_failure(format!("[Errore avvio] {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::is_tool_failure;

    #[test]
    fn db_tool_failure_dichiara_il_fallimento_e_preserva_il_messaggio() {
        // Chiama il PRODUTTORE reale usato dai 4 siti `Err(e) => db_tool_failure(e)`
        // di questo file (list/create/update/delete): senza il marker in testa
        // questi fallimenti erano indistinguibili da un successo per
        // anti-loop/supervisore/final_gate (regola M).
        let out = db_tool_failure("relation \"run_configurations\" does not exist");
        assert!(is_tool_failure(&out));
        assert!(out.contains("relation \"run_configurations\" does not exist"));
    }

    #[test]
    fn db_tool_failure_non_raddoppia_il_marker_su_propagazione_a_catena() {
        // Un errore gia' marcato (es. inoltrato da un helper) non deve finire
        // con due marker in testa quando ripassa per lo stesso costruttore.
        let una_volta = db_tool_failure("boom");
        let due_volte = tool_failure(&una_volta);
        assert_eq!(una_volta, due_volte);
        assert_eq!(
            due_volte
                .matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
                .count(),
            1
        );
    }

    #[test]
    fn parse_uuid_invalido_avvolto_da_tool_failure_e_riconosciuto_come_fallimento() {
        // `parse_uuid` (prompt_admin.rs) e' il PRODUTTORE reale usato da ogni
        // handler di questo file per `project_id`/`config_id`: ogni ramo
        // `Err(e) => return tool_failure(e)` dipende da questo comportamento.
        // Prima del fix la stringa bare "[Errore] ..." veniva ritornata senza
        // marker e un `config_id` malformato risultava un successo agli occhi
        // di anti-loop/supervisore/final_gate.
        let args = json!({ "project_id": "non-e-un-uuid" });
        let err = parse_uuid(&args, "project_id").expect_err("input non valido deve fallire");
        let out = tool_failure(&err);
        assert!(is_tool_failure(&out));
        assert!(out.contains("project_id"));
    }

    #[test]
    fn config_id_mancante_avvolto_da_tool_failure_e_riconosciuto_come_fallimento() {
        // Stesso produttore, campo assente: copre il caso reale in cui
        // l'agente chiama update/delete/launch senza passare `config_id`.
        let args = json!({});
        let err = parse_uuid(&args, "config_id").expect_err("campo assente deve fallire");
        let out = tool_failure(&err);
        assert!(is_tool_failure(&out));
    }

    #[test]
    fn errore_avvio_processo_e_riconosciuto_come_fallimento() {
        // Copre il ramo piu' critico del file: `handle_run_config_launch`
        // sul fallimento di `spawn_agent_process`. Un lancio fallito marcato
        // come successo lascerebbe l'agente convinto che il servizio sia
        // partito. Costruzione identica al sito di ritorno reale in
        // `handle_run_config_launch` (stesso `format!` + `tool_failure`).
        let spawn_err = "porta 3000 gia' occupata".to_string();
        let out = tool_failure(format!("[Errore avvio] {spawn_err}"));
        assert!(is_tool_failure(&out));
        assert!(out.contains("porta 3000 gia' occupata"));
    }

    #[test]
    fn messaggio_letterale_di_configurazione_non_trovata_e_riconosciuto_come_fallimento() {
        // Copre i rami letterali (non provenienti da un helper) usati da
        // update/delete/launch quando la riga non esiste: `tool_failure("[Errore]
        // Configurazione non trovata")`.
        let out = tool_failure("[Errore] Configurazione non trovata");
        assert!(is_tool_failure(&out));
    }
}
