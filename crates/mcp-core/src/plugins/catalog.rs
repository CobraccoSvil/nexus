use super::*;
use chrono::DateTime;

pub async fn list_plugin_catalog(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT
            c.id,
            c.slug,
            c.name,
            c.description,
            c.plugin_type,
            c.transport,
            c.http_url,
            c.stdio_command,
            c.stdio_args,
            c.required_secret_refs,
            c.optional_secret_refs,
            c.default_scope,
            c.allowed_commands,
            c.default_tool_policy,
            c.metadata,
            c.is_allowlisted,
            c.enabled,
            COALESCE(
                json_agg(
                    json_build_object(
                        'id', r.id,
                        'version', r.version,
                        'changelog', r.changelog,
                        'isStable', r.is_stable,
                        'createdAt', r.created_at
                    )
                    ORDER BY r.created_at DESC
                ) FILTER (WHERE r.id IS NOT NULL),
                '[]'::json
            ) AS releases
        FROM plugin_catalog_items c
        LEFT JOIN plugin_releases r ON r.catalog_item_id = c.id
        WHERE c.enabled = TRUE
        GROUP BY c.id
        ORDER BY c.name
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "slug": row.try_get::<String, _>("slug").unwrap_or_default(),
                "name": row.try_get::<String, _>("name").unwrap_or_default(),
                "description": row.try_get::<String, _>("description").unwrap_or_default(),
                "pluginType": row.try_get::<String, _>("plugin_type").unwrap_or_else(|_| "mcp".to_string()),
                "transport": row.try_get::<String, _>("transport").unwrap_or_else(|_| "http".to_string()),
                "httpUrl": row.try_get::<Option<String>, _>("http_url").unwrap_or(None),
                "stdioCommand": row.try_get::<Option<String>, _>("stdio_command").unwrap_or(None),
                "stdioArgs": row.try_get::<Value, _>("stdio_args").unwrap_or(json!([])),
                "requiredSecretRefs": row.try_get::<Value, _>("required_secret_refs").unwrap_or(json!([])),
                "optionalSecretRefs": row.try_get::<Value, _>("optional_secret_refs").unwrap_or(json!([])),
                "defaultScope": row.try_get::<String, _>("default_scope").unwrap_or_else(|_| "global".to_string()),
                "allowedCommands": row.try_get::<Value, _>("allowed_commands").unwrap_or(json!([])),
                "defaultToolPolicy": row.try_get::<Value, _>("default_tool_policy").unwrap_or(json!({"mode":"allowlist","tools":[],"blockedTools":[] })),
                "metadata": row.try_get::<Value, _>("metadata").unwrap_or(json!({})),
                "isAllowlisted": row.try_get::<bool, _>("is_allowlisted").unwrap_or(false),
                "enabled": row.try_get::<bool, _>("enabled").unwrap_or(true),
                "releases": row.try_get::<Value, _>("releases").unwrap_or(json!([])),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "items": items })))
}

pub async fn list_installed_plugins(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    if let Err(err) = cleanup_plugin_and_adapter_duplicates(&state.db).await {
        eprintln!("plugin dedup cleanup failed: {err}");
    }

    let rows = sqlx::query(
        r#"
        SELECT
            pi.id,
            pi.catalog_item_id,
            pi.release_id,
            pi.installed_by_user_id,
            pi.project_id,
            pi.scope,
            pi.name,
            pi.enabled,
            pi.config,
            pi.secret_bindings,
            pi.health_status,
            pi.last_health_message,
            pi.last_tested_at,
            pi.created_at,
            pi.updated_at,
            c.slug,
            c.name AS catalog_name,
            c.description AS catalog_description,
            c.transport,
            pr.version,
            ms.id AS mcp_server_id,
            pol.mode AS policy_mode,
            pol.tools AS policy_tools,
            pol.blocked_tools AS policy_blocked_tools
        FROM plugin_instances pi
        JOIN plugin_catalog_items c ON c.id = pi.catalog_item_id
        LEFT JOIN plugin_releases pr ON pr.id = pi.release_id
        LEFT JOIN LATERAL (
            SELECT id
            FROM mcp_servers
            WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC
            LIMIT 1
        ) ms ON TRUE
        LEFT JOIN LATERAL (
            SELECT mode, tools, blocked_tools
            FROM plugin_instance_tool_policies
            WHERE plugin_instance_id = pi.id
            ORDER BY updated_at DESC NULLS LAST, id DESC
            LIMIT 1
        ) pol ON TRUE
        WHERE
            pi.scope = 'global'
            OR pi.installed_by_user_id = $1
            OR (
                pi.scope = 'project'
                AND EXISTS (
                    SELECT 1
                    FROM projects p
                    LEFT JOIN project_members pm
                      ON pm.project_id = p.id AND pm.user_id = $1
                    WHERE p.id = pi.project_id
                      AND (p.owner_user_id = $1 OR pm.user_id IS NOT NULL)
                )
            )
        ORDER BY pi.created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items = rows
        .iter()
        .map(|row| {
            let secret_bindings = row.try_get::<Value, _>("secret_bindings").unwrap_or(json!({}));
            let has_secret_bindings = secret_bindings
                .as_object()
                .map(|obj| !obj.is_empty())
                .unwrap_or(false);
            json!({
                "id": row.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "catalogItemId": row.try_get::<Uuid, _>("catalog_item_id").ok().map(|v| v.to_string()),
                "releaseId": row.try_get::<Option<Uuid>, _>("release_id").unwrap_or(None).map(|v| v.to_string()),
                "version": row.try_get::<Option<String>, _>("version").unwrap_or(None),
                "slug": row.try_get::<String, _>("slug").unwrap_or_default(),
                "catalogName": row.try_get::<String, _>("catalog_name").unwrap_or_default(),
                "catalogDescription": row.try_get::<String, _>("catalog_description").unwrap_or_default(),
                "transport": row.try_get::<String, _>("transport").unwrap_or_else(|_| "http".to_string()),
                "scope": row.try_get::<String, _>("scope").unwrap_or_else(|_| "global".to_string()),
                "projectId": row.try_get::<Option<Uuid>, _>("project_id").unwrap_or(None).map(|v| v.to_string()),
                "name": row.try_get::<String, _>("name").unwrap_or_default(),
                "enabled": row.try_get::<bool, _>("enabled").unwrap_or(true),
                "healthStatus": row.try_get::<String, _>("health_status").unwrap_or_else(|_| "unknown".to_string()),
                "lastHealthMessage": row.try_get::<Option<String>, _>("last_health_message").unwrap_or(None),
                "lastTestedAt": row.try_get::<Option<DateTime<Utc>>, _>("last_tested_at").unwrap_or(None).map(|v| v.to_rfc3339()),
                "mcpServerId": row.try_get::<Option<Uuid>, _>("mcp_server_id").unwrap_or(None).map(|v| v.to_string()),
                "toolPolicy": {
                    "mode": row.try_get::<Option<String>, _>("policy_mode").unwrap_or(Some("all".to_string())),
                    "tools": row.try_get::<Value, _>("policy_tools").unwrap_or(json!([])),
                    "blockedTools": row.try_get::<Value, _>("policy_blocked_tools").unwrap_or(json!([])),
                },
                "secretBindingsMasked": has_secret_bindings,
                "createdAt": row.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
                "updatedAt": row.try_get::<DateTime<Utc>, _>("updated_at").ok().map(|v| v.to_rfc3339()),
                "canManage": can_manage_instance(row, user_id, &claims.role),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "items": items })))
}
