use std::collections::HashSet;

use axum::extract::Path as AxumPath;

use super::*;
use crate::chat_learning::ensure_project_access;

pub async fn install_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<InstallPluginRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let catalog = get_catalog_by_install_request(&state.db, &body).await?;
    let scope = normalize_scope(body.scope.as_deref().or(Some(&catalog.default_scope)))?;
    if scope == "global" && claims.role != "admin" {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Solo admin puo' installare plugin globali",
        ));
    }

    let project_id = if scope == "project" {
        let raw_project_id = body.project_id.as_deref().ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "projectId richiesto per scope project",
            )
        })?;
        let parsed = Uuid::parse_str(raw_project_id)
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "projectId non valido"))?;
        ensure_project_access(&state.db, user_id, parsed).await?;
        Some(parsed)
    } else {
        None
    };

    if let Some((existing_id, existing_scope, existing_project_id, existing_owner_id)) =
        find_duplicate_instance_anywhere(&state.db, catalog.id, &catalog.slug).await?
    {
        let mut details = format!("scope={existing_scope}");
        if let Some(pid) = existing_project_id {
            details.push_str(&format!(", projectId={pid}"));
        }
        if let Some(uid) = existing_owner_id {
            details.push_str(&format!(", ownerUserId={uid}"));
        }
        return Err(api_error(
            StatusCode::CONFLICT,
            format!(
                "Plugin gia' installato (instance: {}, {}). Usa aggiorna/disabilita/migra invece di reinstallare.",
                existing_id, details
            ),
        ));
    }

    let required_secret_keys = parse_string_array(&catalog.required_secret_refs);
    if !required_secret_keys.is_empty() {
        let mut missing = Vec::new();
        for key in required_secret_keys {
            if resolve_secret_value(&state.db, &key).await.is_none() {
                missing.push(key);
            }
        }
        if !missing.is_empty() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "Chiavi mancanti per attivare il plugin: {}. Configurale in Admin > Settings > Plugin MCP.",
                    missing.join(", ")
                ),
            ));
        }
    }

    let config = body.config.clone().unwrap_or_else(|| json!({}));
    let secret_bindings = body.secret_bindings.clone().unwrap_or_else(|| json!({}));

    let release_row = if let Some(version) = body.version.as_deref() {
        sqlx::query(
            "SELECT id, version FROM plugin_releases WHERE catalog_item_id = $1 AND version = $2",
        )
        .bind(catalog.id)
        .bind(version)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        None
    };
    let release_id = release_row
        .as_ref()
        .and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .or(catalog.release_id);
    let release_version = release_row
        .as_ref()
        .and_then(|row| row.try_get::<String, _>("version").ok())
        .or(catalog.release_version.clone())
        .unwrap_or_else(|| "1.0.0".to_string());

    let runtime_command = config
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(catalog.stdio_command.clone());

    if catalog.transport == "stdio" {
        let allowlisted_commands: HashSet<String> = parse_string_array(&catalog.allowed_commands)
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();
        if !allowlisted_commands.is_empty() {
            let candidate = runtime_command.clone().unwrap_or_default().to_lowercase();
            if !allowlisted_commands.contains(&candidate) {
                return Err(api_error(
                    StatusCode::FORBIDDEN,
                    "Command stdio non consentito per questo plugin",
                ));
            }
        }
    }

    let instance_name = body
        .name
        .clone()
        .unwrap_or_else(|| format!("{} ({release_version})", catalog.name));

    let plugin_instance_row = sqlx::query(
        r#"
        INSERT INTO plugin_instances
            (catalog_item_id, release_id, installed_by_user_id, project_id, scope, name, enabled, config, secret_bindings)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, $7, $8)
        RETURNING id
        "#,
    )
    .bind(catalog.id)
    .bind(release_id)
    .bind(user_id)
    .bind(project_id)
    .bind(&scope)
    .bind(&instance_name)
    .bind(&config)
    .bind(&secret_bindings)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let plugin_instance_id: Uuid = plugin_instance_row.try_get("id").unwrap_or(Uuid::nil());

    let policy_mode = catalog
        .default_tool_policy
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("allowlist")
        .to_string();
    let policy_tools = catalog
        .default_tool_policy
        .get("tools")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let policy_blocked = catalog
        .default_tool_policy
        .get("blockedTools")
        .cloned()
        .unwrap_or_else(|| json!([]));

    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_instance_tool_policies
            (plugin_instance_id, mode, tools, blocked_tools, updated_by_user_id)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (plugin_instance_id)
        DO UPDATE SET
            mode = EXCLUDED.mode,
            tools = EXCLUDED.tools,
            blocked_tools = EXCLUDED.blocked_tools,
            updated_by_user_id = EXCLUDED.updated_by_user_id,
            updated_at = NOW()
        "#,
    )
    .bind(plugin_instance_id)
    .bind(&policy_mode)
    .bind(&policy_tools)
    .bind(&policy_blocked)
    .bind(user_id)
    .execute(&state.db)
    .await;

    let config_url = config
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(catalog.http_url.clone());
    let config_args = config
        .get("args")
        .cloned()
        .unwrap_or_else(|| catalog.stdio_args.clone());
    let config_headers = config.get("headers").cloned().unwrap_or_else(|| json!({}));
    let config_env = config.get("envVars").cloned().unwrap_or_else(|| json!({}));

    let mcp_server_row = sqlx::query(
        r#"
        INSERT INTO mcp_servers
            (plugin_instance_id, user_id, project_id, name, description, transport, url, command, args, env_vars, headers, enabled, scope)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, TRUE, $12)
        RETURNING id
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .bind(project_id)
    .bind(&instance_name)
    .bind(Some(catalog.description.clone()))
    .bind(&catalog.transport)
    .bind(config_url)
    .bind(runtime_command)
    .bind(config_args)
    .bind(config_env)
    .bind(config_headers)
    .bind(&scope)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mcp_server_id: Uuid = mcp_server_row.try_get("id").unwrap_or(Uuid::nil());

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        project_id,
        "install",
        "ok",
        Some(format!(
            "Plugin {} installato ({})",
            catalog.slug, release_version
        )),
        json!({
            "catalogItemId": catalog.id.to_string(),
            "releaseId": release_id.map(|v| v.to_string()),
            "scope": scope,
            "mcpServerId": mcp_server_id.to_string(),
            "requiredSecretRefs": catalog.required_secret_refs,
            "optionalSecretRefs": catalog.optional_secret_refs,
        }),
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "pluginInstanceId": plugin_instance_id.to_string(),
        "mcpServerId": mcp_server_id.to_string(),
        "name": instance_name,
        "slug": catalog.slug,
        "version": release_version,
    })))
}

pub async fn update_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
    Json(body): Json<UpdatePluginRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;

    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::FORBIDDEN, "Plugin non modificabile"));
    }

    let catalog_item_id: Uuid = existing
        .try_get("catalog_item_id")
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Catalog item non valido"))?;
    let release_row = sqlx::query(
        "SELECT id, version, config_patch FROM plugin_releases WHERE catalog_item_id=$1 AND version=$2",
    )
    .bind(catalog_item_id)
    .bind(&body.version)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(release_row) = release_row else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Versione plugin non trovata",
        ));
    };
    let release_id: Uuid = release_row.try_get("id").unwrap_or(Uuid::nil());
    let config_patch: Value = release_row.try_get("config_patch").unwrap_or(json!({}));

    let mut merged_config: Value = existing.try_get("config").unwrap_or(json!({}));
    if let (Some(target), Some(patch)) = (merged_config.as_object_mut(), config_patch.as_object()) {
        for (k, v) in patch {
            target.insert(k.clone(), v.clone());
        }
    }

    let transport: String = existing
        .try_get("transport")
        .unwrap_or_else(|_| "http".to_string());
    let url = merged_config
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(existing.try_get("http_url").unwrap_or(None));
    let command = merged_config
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(existing.try_get("stdio_command").unwrap_or(None));
    let args = merged_config
        .get("args")
        .cloned()
        .unwrap_or_else(|| existing.try_get("stdio_args").unwrap_or(json!([])));
    let headers = merged_config
        .get("headers")
        .cloned()
        .unwrap_or_else(|| existing.try_get("headers").unwrap_or(json!({})));
    let env_vars = merged_config
        .get("envVars")
        .cloned()
        .unwrap_or_else(|| existing.try_get("env_vars").unwrap_or(json!({})));

    sqlx::query(
        "UPDATE plugin_instances SET release_id=$2, config=$3, updated_at=NOW() WHERE id=$1",
    )
    .bind(plugin_instance_id)
    .bind(release_id)
    .bind(&merged_config)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mcp_server_id: Uuid = existing
        .try_get("mcp_server_id")
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin senza adapter MCP collegato"))?;
    sqlx::query(
        r#"
        UPDATE mcp_servers
        SET transport=$2, url=$3, command=$4, args=$5, headers=$6, env_vars=$7, updated_at=NOW()
        WHERE id=$1
        "#,
    )
    .bind(mcp_server_id)
    .bind(transport)
    .bind(url)
    .bind(command)
    .bind(args)
    .bind(headers)
    .bind(env_vars)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        existing.try_get("project_id").unwrap_or(None),
        "update",
        "ok",
        Some(format!(
            "Plugin aggiornato alla versione {}",
            body.version
        )),
        json!({ "version": body.version }),
    )
    .await;

    Ok(Json(json!({ "ok": true, "version": body.version })))
}

pub async fn uninstall_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;

    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Plugin non disinstallabile",
        ));
    }

    let mcp_server_id = existing
        .try_get::<Option<Uuid>, _>("mcp_server_id")
        .unwrap_or(None);
    let project_id = existing
        .try_get::<Option<Uuid>, _>("project_id")
        .unwrap_or(None);
    let plugin_name = existing
        .try_get::<String, _>("name")
        .unwrap_or_else(|_| "Plugin".to_string());
    let plugin_slug = existing
        .try_get::<String, _>("slug")
        .unwrap_or_else(|_| "unknown".to_string());

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(server_id) = mcp_server_id {
        sqlx::query("DELETE FROM mcp_servers WHERE id = $1")
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    sqlx::query("DELETE FROM plugin_instances WHERE id = $1")
        .bind(plugin_instance_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(
        &state.db,
        None,
        Some(user_id),
        project_id,
        "uninstall",
        "ok",
        Some(format!(
            "Plugin disinstallato: {} ({})",
            plugin_name, plugin_slug
        )),
        json!({
            "pluginInstanceId": plugin_instance_id.to_string(),
            "mcpServerId": mcp_server_id.map(|v| v.to_string()),
            "slug": plugin_slug,
        }),
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "pluginInstanceId": plugin_instance_id.to_string(),
    })))
}

pub async fn toggle_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
    Json(body): Json<TogglePluginRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::FORBIDDEN, "Plugin non modificabile"));
    }

    sqlx::query("UPDATE plugin_instances SET enabled=$2, updated_at=NOW() WHERE id=$1")
        .bind(plugin_instance_id)
        .bind(body.enabled)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Ok(mcp_server_id) = existing.try_get::<Uuid, _>("mcp_server_id") {
        let _ = sqlx::query("UPDATE mcp_servers SET enabled=$2, updated_at=NOW() WHERE id=$1")
            .bind(mcp_server_id)
            .bind(body.enabled)
            .execute(&state.db)
            .await;
    }

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        existing.try_get("project_id").unwrap_or(None),
        "toggle",
        "ok",
        Some(format!(
            "Plugin {}",
            if body.enabled {
                "abilitato"
            } else {
                "disabilitato"
            }
        )),
        json!({ "enabled": body.enabled }),
    )
    .await;

    Ok(Json(json!({ "ok": true, "enabled": body.enabled })))
}

pub async fn migrate_legacy_mcp_server(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(server_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let server_id = Uuid::parse_str(&server_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Server MCP id non valido"))?;

    let row = sqlx::query(
        r#"
        SELECT
            id,
            user_id,
            project_id,
            scope,
            plugin_instance_id,
            name,
            description,
            transport,
            url,
            command,
            args,
            env_vars,
            headers
        FROM mcp_servers
        WHERE id = $1
        "#,
    )
    .bind(server_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Server MCP legacy non trovato",
        ));
    };

    let owner_user_id: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let scope: String = row
        .try_get("scope")
        .unwrap_or_else(|_| "user".to_string())
        .to_lowercase();
    let can_manage =
        owner_user_id == Some(user_id) || (scope == "global" && claims.role == "admin");
    if !can_manage {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Server MCP non gestibile dall'utente corrente",
        ));
    }

    if let Some(plugin_instance_id) = row
        .try_get::<Option<Uuid>, _>("plugin_instance_id")
        .unwrap_or(None)
    {
        return Ok(Json(json!({
            "ok": true,
            "alreadyMigrated": true,
            "pluginInstanceId": plugin_instance_id.to_string(),
        })));
    }

    let transport: String = row
        .try_get("transport")
        .unwrap_or_else(|_| "http".to_string());
    let url: Option<String> = row.try_get("url").unwrap_or(None);
    let command: Option<String> = row.try_get("command").unwrap_or(None);
    let args: Value = row.try_get("args").unwrap_or(json!([]));
    let catalog_slug =
        detect_legacy_catalog_slug(&transport, url.as_deref(), command.as_deref(), &args)
            .ok_or_else(|| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "Questo MCP legacy non e' mappabile automaticamente a un plugin del catalogo curato",
                )
            })?;

    let catalog = get_catalog_by_install_request(
        &state.db,
        &InstallPluginRequest {
            catalog_item_id: None,
            slug: Some(catalog_slug.to_string()),
            version: None,
            scope: Some(scope.clone()),
            project_id: row
                .try_get::<Option<Uuid>, _>("project_id")
                .unwrap_or(None)
                .map(|v| v.to_string()),
            name: None,
            config: None,
            secret_bindings: None,
        },
    )
    .await?;

    let project_id: Option<Uuid> = row.try_get("project_id").unwrap_or(None);
    if let Some((existing_id, _, _, _)) =
        find_duplicate_instance_anywhere(&state.db, catalog.id, &catalog.slug).await?
    {
        sqlx::query(
            "UPDATE mcp_servers SET plugin_instance_id = $2, updated_at = NOW() WHERE id = $1",
        )
        .bind(server_id)
        .bind(existing_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        return Ok(Json(json!({
            "ok": true,
            "linkedExisting": true,
            "pluginInstanceId": existing_id.to_string(),
            "slug": catalog.slug,
        })));
    }

    let release_row = sqlx::query(
        r#"
        SELECT id, version
        FROM plugin_releases
        WHERE catalog_item_id = $1 AND is_stable = true
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(catalog.id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let release_id = release_row
        .as_ref()
        .and_then(|r| r.try_get::<Uuid, _>("id").ok())
        .or(catalog.release_id);
    let release_version = release_row
        .as_ref()
        .and_then(|r| r.try_get::<String, _>("version").ok())
        .or(catalog.release_version.clone())
        .unwrap_or_else(|| "1.0.0".to_string());

    let env_vars: Value = row.try_get("env_vars").unwrap_or(json!({}));
    let headers: Value = row.try_get("headers").unwrap_or(json!({}));
    let config = json!({
        "url": url,
        "command": command,
        "args": args,
        "envVars": env_vars,
        "headers": headers,
    });
    let name: String = row
        .try_get("name")
        .unwrap_or_else(|_| catalog.name.clone());

    let plugin_row = sqlx::query(
        r#"
        INSERT INTO plugin_instances
            (catalog_item_id, release_id, installed_by_user_id, project_id, scope, name, enabled, config, secret_bindings)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, $7, '{}'::jsonb)
        RETURNING id
        "#,
    )
    .bind(catalog.id)
    .bind(release_id)
    .bind(user_id)
    .bind(project_id)
    .bind(&scope)
    .bind(&name)
    .bind(&config)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let plugin_instance_id: Uuid = plugin_row.try_get("id").unwrap_or(Uuid::nil());

    let policy_mode = catalog
        .default_tool_policy
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("allowlist")
        .to_string();
    let policy_tools = catalog
        .default_tool_policy
        .get("tools")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let policy_blocked = catalog
        .default_tool_policy
        .get("blockedTools")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_instance_tool_policies
            (plugin_instance_id, mode, tools, blocked_tools, updated_by_user_id)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (plugin_instance_id)
        DO UPDATE SET
            mode = EXCLUDED.mode,
            tools = EXCLUDED.tools,
            blocked_tools = EXCLUDED.blocked_tools,
            updated_by_user_id = EXCLUDED.updated_by_user_id,
            updated_at = NOW()
        "#,
    )
    .bind(plugin_instance_id)
    .bind(policy_mode)
    .bind(policy_tools)
    .bind(policy_blocked)
    .bind(user_id)
    .execute(&state.db)
    .await;

    sqlx::query("UPDATE mcp_servers SET plugin_instance_id = $2, updated_at = NOW() WHERE id = $1")
        .bind(server_id)
        .bind(plugin_instance_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        project_id,
        "migrate_legacy",
        "ok",
        Some(format!(
            "Migrazione MCP legacy completata ({catalog_slug} {release_version})"
        )),
        json!({
            "legacyMcpServerId": server_id.to_string(),
            "catalogSlug": catalog_slug,
            "releaseId": release_id.map(|v| v.to_string()),
        }),
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "linkedExisting": false,
        "pluginInstanceId": plugin_instance_id.to_string(),
        "slug": catalog_slug,
    })))
}
