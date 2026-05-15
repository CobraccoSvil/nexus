// Operazioni CRUD sui progetti: list, register, get, delete, patch default profile.

use super::*;

pub async fn list_user_projects(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let rows = sqlx::query(
        r#"
        SELECT
            p.id,
            p.name,
            p.slug,
            p.owner_user_id,
            p.visibility,
            p.analyzed_at,
            w.id AS workspace_id,
            w.absolute_path,
            COALESCE(r.is_git_repo, FALSE) AS is_git_repo,
            COALESCE(r.current_branch, p.default_branch) AS current_branch,
            CASE
                WHEN p.owner_user_id = $1 THEN 'owner'
                ELSE pm.role
            END AS current_user_role,
            pos.updated_at AS last_opened_at
        FROM projects p
        LEFT JOIN project_members pm
            ON pm.project_id = p.id AND pm.user_id = $1
        LEFT JOIN workspaces w
            ON w.project_id = p.id AND w.is_primary = TRUE
        LEFT JOIN repositories r
            ON r.project_id = p.id
        LEFT JOIN project_open_sessions pos
            ON pos.project_id = p.id AND pos.user_id = $1
        WHERE p.owner_user_id = $1 OR pm.user_id IS NOT NULL
        ORDER BY COALESCE(pos.updated_at, p.created_at) DESC, p.name ASC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let projects = rows
        .into_iter()
        .map(|row| {
            let role = row
                .try_get::<Option<String>, _>("current_user_role")
                .ok()
                .flatten()
                .unwrap_or_else(|| "viewer".to_string());
            let access = map_access(&role);
            let analyzed_at = row
                .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("analyzed_at")
                .ok()
                .flatten()
                .map(|ts| ts.to_rfc3339());
            let is_analyzed = analyzed_at.is_some();
            UserProjectSummary {
                id: row.get::<Uuid, _>("id").to_string(),
                name: row.get::<String, _>("name"),
                slug: row.get::<String, _>("slug"),
                owner_user_id: row.get::<Uuid, _>("owner_user_id").to_string(),
                current_user_role: access.current_user_role,
                can_write: access.can_write,
                can_manage_git: access.can_manage_git,
                is_shared: access.is_shared,
                visibility: row.get::<String, _>("visibility"),
                workspace_id: row
                    .try_get::<Option<Uuid>, _>("workspace_id")
                    .ok()
                    .flatten()
                    .map(|id| id.to_string()),
                root_path: row
                    .try_get::<Option<String>, _>("absolute_path")
                    .ok()
                    .flatten(),
                is_git_repo: row.get::<bool, _>("is_git_repo"),
                current_branch: row
                    .try_get::<Option<String>, _>("current_branch")
                    .ok()
                    .flatten(),
                last_opened_at: row
                    .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_opened_at")
                    .ok()
                    .flatten()
                    .map(|ts| ts.to_rfc3339()),
                analyzed_at,
                is_analyzed,
                nexus_ready: is_analyzed,
            }
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "projects": projects })))
}

pub async fn register_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<RegisterProjectRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let raw_path = PathBuf::from(body.absolute_path.trim());
    let canonical = raw_path.canonicalize().map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "La directory selezionata non esiste",
        )
    })?;

    if !canonical.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso selezionato non e' una directory",
        ));
    }

    assert_allowed_workspace(&state.db, &canonical).await?;

    if let Some(existing_project_id) = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT p.id
        FROM projects p
        INNER JOIN workspaces w ON w.project_id = p.id
        WHERE p.owner_user_id = $1
          AND w.absolute_path = $2
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(canonical.to_string_lossy().to_string())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        let context = load_project_context(&state.db, existing_project_id, user_id).await?;
        upsert_open_session(
            &state.db,
            user_id,
            &context,
            &[],
            context.details.root_path.as_deref(),
        )
        .await?;

        return Ok(Json(json!({ "project": context.details })));
    }

    let project_name = body
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .or_else(|| {
            canonical
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "Project".to_string());

    let team_id = ensure_personal_team(&state.db, user_id).await?;
    let slug = ensure_unique_slug(&state.db, user_id, &project_name).await?;
    let git = detect_git_repo(&canonical).await;
    assert_allowed_workspace(&state.db, &git.root_path).await?;

    let project_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let repository_id = Uuid::new_v4();
    let default_branch = git
        .current_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        r#"
        INSERT INTO projects (id, team_id, owner_user_id, name, slug, default_branch, visibility, last_opened_by_user_id)
        VALUES ($1, $2, $3, $4, $5, $6, 'private', $3)
        "#,
    )
    .bind(project_id)
    .bind(team_id)
    .bind(user_id)
    .bind(&project_name)
    .bind(&slug)
    .bind(&default_branch)
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        "INSERT INTO project_members (id, project_id, user_id, role) VALUES ($1, $2, $3, 'owner')",
    )
    .bind(Uuid::new_v4())
    .bind(project_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        "INSERT INTO workspaces (id, project_id, absolute_path, is_primary) VALUES ($1, $2, $3, TRUE)",
    )
    .bind(workspace_id)
    .bind(project_id)
    .bind(canonical.to_string_lossy().to_string())
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let remote_url = git
        .remotes
        .first()
        .map(|(_, fetch_url, _)| fetch_url.clone());

    sqlx::query(
        r#"
        INSERT INTO repositories (id, project_id, provider, remote_url, root_path, is_git_repo, current_branch)
        VALUES ($1, $2, 'local', $3, $4, $5, $6)
        "#,
    )
    .bind(repository_id)
    .bind(project_id)
    .bind(remote_url)
    .bind(git.root_path.to_string_lossy().to_string())
    .bind(git.is_git_repo)
    .bind(git.current_branch.clone())
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for (name, fetch_url, push_url) in &git.remotes {
        sqlx::query(
            "INSERT INTO git_remotes (repository_id, name, fetch_url, push_url) VALUES ($1, $2, $3, $4)",
        )
        .bind(repository_id)
        .bind(name)
        .bind(fetch_url)
        .bind(push_url)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    sqlx::query(
        r#"
        INSERT INTO user_project_preferences (user_id, project_id, preferences)
        VALUES ($1, $2, '{"sidebar":"Explorer"}'::jsonb)
        ON CONFLICT (user_id, project_id) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let context = load_project_context(&state.db, project_id, user_id).await?;
    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[],
        context.details.root_path.as_deref(),
    )
    .await?;

    // Fix M30 + M31: auto-popola run_configurations e nexus_port_allocations
    // scansionando il filesystem del progetto registrato. Spawn-and-forget,
    // best-effort: errori solo loggati. Idempotenti via guardia/ON CONFLICT.
    {
        let db_clone = state.db.clone();
        let root_clone = canonical.clone();
        let pid = project_id;
        tokio::spawn(async move {
            crate::project_workspace::run_configs::auto_populate_run_configs(
                &db_clone, pid, &root_clone,
            )
            .await;
            crate::project_workspace::scan_ports::auto_populate_port_allocations(
                &db_clone, pid, &root_clone,
            )
            .await;
        });
    }

    // Avvia analisi automatica in background per popolare la memoria vettoriale
    {
        let state_bg = state.clone();
        let root_bg = context.repository_root_path.clone();
        let project_id_bg = project_id;
        tokio::spawn(async move {
            let ext_counts = {
                let mut m = std::collections::BTreeMap::new();
                let mut total = 0u32;
                count_files_by_extension(&root_bg, &mut m, &mut total, 0).await;
                (m, total)
            };
            let languages = detect_languages(&ext_counts.0);
            let frameworks = detect_frameworks(&root_bg).await;
            let dependencies = read_dependencies(&root_bg).await;
            let git_info = json!({ "isGitRepo": git.is_git_repo, "branch": git.current_branch });
            let _ = index_project_bootstrap_vectors(
                &state_bg,
                project_id_bg,
                &root_bg,
                ext_counts.1,
                &languages,
                &frameworks,
                &dependencies,
                &git_info,
            )
            .await;
            let _ = index_project_code_files(&state_bg, project_id_bg, &root_bg).await;
        });
    }

    Ok(Json(json!({ "project": context.details })))
}

pub async fn get_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    Ok(Json(json!({ "project": context.details })))
}

/// DELETE /api/projects/:id?force=true
///
/// Elimina il progetto dal DB e la sua directory locale.
/// Se ci sono modifiche non committate e `force` non e' `true`,
/// risponde 409 con `{ "hasPendingChanges": true, "dirtyCount": N, "rootPath": "..." }`
/// cosi' il frontend puo' chiedere conferma all'utente.
pub async fn delete_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.access.can_manage_git && !context.access.can_write {
        return Err(api_error(StatusCode::FORBIDDEN, "Non hai permessi per eliminare questo progetto"));
    }

    let force = params.get("force").map(|v| v == "true").unwrap_or(false);

    // Controlla modifiche non committate (solo se e' un repo git)
    if !force && context.is_git_repo {
        let root = &context.repository_root_path;
        let dirty_count: usize = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(root)
            .output()
            .await
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count()
            })
            .unwrap_or(0);

        if dirty_count > 0 {
            return Ok(Json(json!({
                "hasPendingChanges": true,
                "dirtyCount": dirty_count,
                "rootPath": root.to_string_lossy(),
                "projectName": context.details.name,
            })));
        }
    }

    let root_path = context.details.root_path.clone().unwrap_or_default();

    // Fix M21: killa i processi figli registrati in agent_processes PRIMA del DELETE.
    // Senza questo step un dev server attivo (es. `npm run dev`) continua a scrivere
    // dentro node_modules/.vite e race con il rm_dir_all sotto, lasciando residui
    // su disco anche dopo che il DB e' stato pulito dal CASCADE.
    let running_pids: Vec<i32> = sqlx::query_scalar(
        r#"
        SELECT pid FROM agent_processes
        WHERE project_id = $1
          AND status IN ('running', 'starting')
          AND pid IS NOT NULL
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for pid in &running_pids {
        let _ = tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .await;
    }
    if !running_pids.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        for pid in &running_pids {
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output()
                .await;
        }
        tracing::info!(
            "delete_project: terminati {} processi figli del progetto {}",
            running_pids.len(),
            project_id
        );
    }

    // Fix M35: scan /proc per processi con CWD dentro project_root non registrati
    // in agent_processes (es. dev server avviati dall'agente in run precedenti e
    // sopravvissuti a uno Stop service mal completato). Linux-only via readlink.
    if !root_path.is_empty() {
        let mut orphan_pids: Vec<i32> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir("/proc").await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                let pid: i32 = match name_str.parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let cwd_link = format!("/proc/{}/cwd", pid);
                if let Ok(cwd) = tokio::fs::read_link(&cwd_link).await {
                    let cwd_str = cwd.to_string_lossy();
                    if cwd_str.starts_with(&root_path) && !running_pids.contains(&pid) {
                        orphan_pids.push(pid);
                    }
                }
            }
        }
        for pid in &orphan_pids {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
        }
        if !orphan_pids.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            for pid in &orphan_pids {
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .output()
                    .await;
            }
            tracing::info!(
                "delete_project: terminati {} processi orfani con cwd in {} (M35)",
                orphan_pids.len(),
                root_path
            );
        }
    }

    // Elimina dal DB (cascade su workspaces, repositories, agent_runs, ecc.)
    sqlx::query("DELETE FROM projects WHERE id = $1 AND owner_user_id = $2")
        .bind(project_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Elimina la directory locale.
    // Tentativo preliminare di chmod -R u+rwX per recuperare file readonly
    // (es. cache build dotnet/docker che a volte fissa permessi).
    // Se la rimozione fallisce, NON ritorna ok:true: ritorna errore parziale
    // con il path orfano, cosi' l'utente sa che deve pulire a mano (es. con sudo
    // se il sandbox ha creato file di altro user).
    let mut residual_dir: Option<String> = None;
    if !root_path.is_empty() {
        let path = std::path::PathBuf::from(&root_path);
        if path.exists() && path.is_dir() {
            // Tentativo soft di sistemare i permessi (ignoro errori del chmod stesso).
            let _ = tokio::process::Command::new("chmod")
                .args(["-R", "u+rwX", root_path.as_str()])
                .output()
                .await;
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                tracing::warn!(
                    "delete_project: impossibile eliminare {} ({}). DB pulito ma directory orfana.",
                    root_path,
                    e
                );
                residual_dir = Some(root_path.clone());
            }
        }
    }

    Ok(Json(json!({
        "ok": true,
        "deleted": id,
        "rootPath": root_path,
        "residualDirectory": residual_dir,
        "warning": residual_dir.as_ref().map(|p| format!(
            "Progetto eliminato dal DB ma la directory '{}' non e' stata rimossa completamente. Probabili file con ownership diversa (es. creati da container build): rimuovili a mano con 'sudo rm -rf {}'.",
            p, p
        )),
    })))
}

/// PATCH /api/projects/:id/default-profile — imposta il profilo AI di default per il progetto
pub async fn patch_project_default_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Verifica accesso
    load_project_context(&state.db, project_id, user_id).await?;

    let profile_id: Option<Uuid> = body
        .get("profileId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "auto" && *s != "default")
        .and_then(|s| Uuid::parse_str(s).ok());

    sqlx::query("UPDATE projects SET default_profile_id = $2 WHERE id = $1")
        .bind(project_id)
        .bind(profile_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tracing::info!("default_profile_id aggiornato per project_id={}", project_id);
    Ok(Json(json!({ "ok": true, "profileId": profile_id.map(|id| id.to_string()) })))
}
