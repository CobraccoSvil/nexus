// Operazioni CRUD sui progetti: list, register, get, delete, patch default profile.

use super::*;

pub async fn list_user_projects(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    // Separazione DB: project_open_sessions e' una tabella MIGRATA (vive nel
    // DB per-progetto). A flag ON un LEFT JOIN sul meta ritornerebbe sempre
    // last_opened_at NULL. Le tabelle projects/project_members/workspaces/
    // repositories sono GLOBALI (restano nel meta). Quindi: prima leggo il set
    // di progetti + campi globali dal meta, poi risolvo last_opened_at per
    // progetto dal pool per-progetto e applico l'ordinamento in Rust.
    let rows = sqlx::query(
        r#"
        SELECT
            p.id,
            p.name,
            p.slug,
            p.owner_user_id,
            p.visibility,
            p.analyzed_at,
            p.created_at,
            w.id AS workspace_id,
            w.absolute_path,
            COALESCE(r.is_git_repo, FALSE) AS is_git_repo,
            COALESCE(r.current_branch, p.default_branch) AS current_branch,
            CASE
                WHEN p.owner_user_id = $1 THEN 'owner'
                ELSE pm.role
            END AS current_user_role
        FROM projects p
        LEFT JOIN project_members pm
            ON pm.project_id = p.id AND pm.user_id = $1
        LEFT JOIN workspaces w
            ON w.project_id = p.id AND w.is_primary = TRUE
        LEFT JOIN repositories r
            ON r.project_id = p.id
        WHERE p.owner_user_id = $1 OR pm.user_id IS NOT NULL
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Chiave di ordinamento originale: COALESCE(last_opened_at, created_at) DESC,
    // name ASC. La conserviamo insieme al summary per ordinare dopo aver risolto
    // last_opened_at dai pool per-progetto.
    let mut entries: Vec<(chrono::DateTime<chrono::Utc>, String, UserProjectSummary)> =
        Vec::with_capacity(rows.len());

    for row in rows {
        let project_id = row.get::<Uuid, _>("id");
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
        let created_at = row.get::<chrono::DateTime<chrono::Utc>, _>("created_at");

        // last_opened_at vive nel DB per-progetto (project_open_sessions migrata):
        // risolvo il pool del progetto e leggo il valore. A flag OFF l'helper
        // ritorna il meta-DB (comportamento storico preservato).
        let proj_pool =
            crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;
        let last_opened_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            r#"
            SELECT updated_at FROM project_open_sessions
            WHERE project_id = $1 AND user_id = $2
            "#,
        )
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(&proj_pool)
        .await
        .ok()
        .flatten();

        let sort_key = last_opened_at.unwrap_or(created_at);
        let name = row.get::<String, _>("name");

        let summary = UserProjectSummary {
            id: project_id.to_string(),
            name: name.clone(),
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
            last_opened_at: last_opened_at.map(|ts| ts.to_rfc3339()),
            analyzed_at,
            is_analyzed,
            nexus_ready: is_analyzed,
        };

        entries.push((sort_key, name, summary));
    }

    // Ordinamento equivalente all'ORDER BY originale:
    // COALESCE(last_opened_at, created_at) DESC, name ASC.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let projects = entries
        .into_iter()
        .map(|(_, _, summary)| summary)
        .collect::<Vec<_>>();

    Ok(Json(json!({ "projects": projects })))
}

pub async fn register_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<RegisterProjectRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let raw_input = body.absolute_path.trim();
    if raw_input.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Nome cartella mancante"));
    }
    let raw_path = if raw_input.starts_with('/') {
        PathBuf::from(raw_input)
    } else {
        // Path relativo: lo risolviamo dentro projects_base_root e creiamo
        // la cartella se non esiste, cosi' l'utente puo' creare un nuovo
        // progetto indicando solo il nome.
        let base_root = load_projects_base_root(&state.db).await?;
        let candidate = base_root.join(raw_input);
        if !candidate.exists() {
            fs::create_dir_all(&candidate).await.map_err(|e| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Impossibile creare la cartella: {e}"),
                )
            })?;
        }
        candidate
    };
    let canonical = raw_path.canonicalize().map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "La directory selezionata non esiste",
        )
    })?;
    // Forma di STORAGE della root: mai il verbatim Windows (`\\?\D:\...`) che
    // canonicalize produce — persistito, inquinava ogni path derivato (finding
    // quality, display UI, resolver testuali). Punto unico nexus_types (regola L).
    let canonical_storage = nexus_types::workspace_paths::path_for_storage(&canonical);

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
    .bind(&canonical_storage)
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
    // NB: non validare git.root_path: se il project dir e dentro un repo git piu
    // grande (es. la home dell'utente e un repo git), git.root_path e un ANTENATO
    // fuori da projects_base_root e farebbe fallire l'assert. La cartella del
    // progetto (`canonical`) e gia stata validata sopra (assert_allowed_workspace
    // a inizio funzione); il root effettivo del repository viene normalizzato a
    // `canonical` piu sotto (repo_root_path).

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
        INSERT INTO projects (id, team_id, owner_user_id, name, slug, default_branch, visibility, last_opened_by_user_id, repository_root_path)
        VALUES ($1, $2, $3, $4, $5, $6, 'private', $3, $7)
        "#,
    )
    .bind(project_id)
    .bind(team_id)
    .bind(user_id)
    .bind(&project_name)
    .bind(&slug)
    .bind(&default_branch)
    .bind(&canonical_storage)
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
    .bind(&canonical_storage)
    .execute(&mut *tx)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let remote_url = git
        .remotes
        .first()
        .map(|(_, fetch_url, _)| fetch_url.clone());

    // Fix: se la git-root rilevata e un ANTENATO del project dir (es. la home
    // dell'utente e un repo git, caso dotfiles, o projects_base_root sta dentro
    // un repo git), il progetto NON coincide con la git-root. In quel caso il
    // root del repository deve essere la cartella del progetto (`canonical`),
    // coerente con workspaces.absolute_path e projects.repository_root_path,
    // altrimenti i tool agente (write_file/run_command via
    // tool_runner_server COALESCE(repositories.root_path, ...)) opererebbero
    // nella git-root condivisa invece che nel progetto. Se invece la git-root
    // e dentro/uguale al project dir, la si mantiene.
    let repo_root_path = if git.root_path.starts_with(&canonical) {
        git.root_path.clone()
    } else {
        canonical.clone()
    };

    sqlx::query(
        r#"
        INSERT INTO repositories (id, project_id, provider, remote_url, root_path, is_git_repo, current_branch)
        VALUES ($1, $2, 'local', $3, $4, $5, $6)
        "#,
    )
    .bind(repository_id)
    .bind(project_id)
    .bind(remote_url)
    .bind(nexus_types::workspace_paths::path_for_storage(
        &repo_root_path,
    ))
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

    // ── Auto-provisioning quote risorse (PR hardening) ─────────────────────
    // Ogni nuovo progetto riceve una riga in nexus_resource_quotas con i default
    // globali. Se la tabella non esiste ancora (migrazione non applicata), skip
    // silenzioso per non bloccare la creazione del progetto.
    let _ = sqlx::query(
        "INSERT INTO nexus_resource_quotas (project_id) VALUES ($1) ON CONFLICT (project_id) DO NOTHING",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await;

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

    // Emetti evento tipizzato di creazione progetto sul dispatcher.
    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ProjectCreated {
            name: project_name.clone(),
            slug: slug.clone(),
        },
    );

    // Fix M30 + M31: auto-popola run_configurations e nexus_port_allocations
    // scansionando il filesystem del progetto registrato. Spawn-and-forget,
    // best-effort: errori solo loggati. Idempotenti via guardia/ON CONFLICT.
    {
        let db_clone = state.db.clone();
        let root_clone = canonical.clone();
        let pid = project_id;
        tokio::spawn(async move {
            crate::project_workspace::run_configs::auto_populate_run_configs(
                &db_clone,
                pid,
                &root_clone,
            )
            .await;
            crate::project_workspace::scan_ports::auto_populate_port_allocations(
                &db_clone,
                pid,
                &root_clone,
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
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi per eliminare questo progetto",
        ));
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
    // Separazione DB: agent_processes e' una tabella migrata, instradiamo la
    // lettura sul pool del progetto (a flag OFF ritorna il meta-DB, comportamento storico).
    let proj_pool = crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;
    let running_pids: Vec<i32> = sqlx::query_scalar(
        r#"
        SELECT pid FROM agent_processes
        WHERE project_id = $1
          AND status IN ('running', 'starting')
          AND pid IS NOT NULL
        "#,
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();

    // Terminazione dei figli tracciati: punto unico process_util::kill_pid
    // (Unix: SIGTERM->SIGKILL; Windows: taskkill /T /F sull'albero). Sostituisce
    // i `kill -TERM/-KILL` inline, no-op silenziosi su Windows.
    for pid in &running_pids {
        if *pid > 0 {
            crate::process_util::kill_pid(*pid as u32).await;
        }
    }
    if !running_pids.is_empty() {
        tracing::info!(
            "delete_project: terminati {} processi figli del progetto {}",
            running_pids.len(),
            project_id
        );
    }

    // Fix M35: scan /proc per processi con CWD dentro project_root non registrati
    // in agent_processes (es. dev server avviati dall'agente in run precedenti e
    // sopravvissuti a uno Stop service mal completato). Linux-only via readlink
    // /proc/{pid}/cwd. Su Windows non c'e' equivalente immediato per lo scan
    // orfani per-cwd: i figli tracciati sono gia' stati terminati sopra via
    // process_util::kill_pid, quindi il blocco resta interamente #[cfg(unix)].
    #[cfg(unix)]
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
        // Terminazione orfani via punto unico process_util::kill_pid.
        for pid in &orphan_pids {
            if *pid > 0 {
                crate::process_util::kill_pid(*pid as u32).await;
            }
        }
        if !orphan_pids.is_empty() {
            tracing::info!(
                "delete_project: terminati {} processi orfani con cwd in {} (M35)",
                orphan_pids.len(),
                root_path
            );
        }
    }

    let project_name = context.details.name.clone();
    let project_slug = context.details.slug.clone();

    // Droppa i database applicativi provisionati internamente da Nexus PRIMA del
    // DELETE: il CASCADE rimuove anche project_database_config, quindi dopo il
    // DELETE non si saprebbe piu' quali database fisici appartenevano al progetto.
    // Best-effort idempotente: errori non bloccano (vedi `projects::cleanup`).
    let db_drop = super::cleanup::drop_internal_app_databases(&state.db, project_id).await;

    // Elimina dal DB (cascade su workspaces, repositories, agent_runs, ecc.)
    sqlx::query("DELETE FROM projects WHERE id = $1 AND owner_user_id = $2")
        .bind(project_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Emetti evento tipizzato di eliminazione progetto sul dispatcher.
    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ProjectDeleted {
            name: project_name.clone(),
        },
    );

    // Cleanup propagato delle risorse esterne (Docker + systemd + Qdrant).
    // Best-effort idempotente: errori non bloccano. Risultato incluso nella
    // risposta API per tracciabilita' lato client. Vedi `projects::cleanup`.
    let external_cleanup =
        super::cleanup::cleanup_external_resources(&state.db, project_id, &project_slug).await;

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
            // Solo Unix: `chmod` non esiste su Windows, dove remove_dir_all gestisce
            // gia' l'eventuale attributo readonly rimuovendolo prima della cancellazione.
            #[cfg(unix)]
            {
                let _ = tokio::process::Command::new("chmod")
                    .args(["-R", "u+rwX", root_path.as_str()])
                    .output()
                    .await;
            }
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
        // Esito del cleanup propagato (Docker container, systemd unit, Qdrant
        // points). Sempre presente: best-effort idempotente, errori dei singoli
        // step non bloccano l'eliminazione.
        "externalCleanup": external_cleanup,
        // Esito del drop dei database applicativi interni (postgres provisionati
        // da Nexus). Best-effort: i database external non vengono toccati.
        "databaseDrop": db_drop,
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

    tracing::info!(
        "default_profile_id aggiornato per project_id={}",
        project_id
    );
    Ok(Json(
        json!({ "ok": true, "profileId": profile_id.map(|id| id.to_string()) }),
    ))
}
