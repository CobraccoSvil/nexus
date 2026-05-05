use axum::extract::Path as AxumPath;
use chrono::DateTime;

use super::*;
use crate::mcp_client;

pub async fn test_plugin(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::FORBIDDEN, "Plugin non testabile"));
    }

    let resolution = build_plugin_resolution(&state.db, plugin_instance_id, user_id).await?;

    let (success, tool_count, error_message, tools_payload) =
        match mcp_client::list_tools(&resolution.config).await {
            Ok(tools) => {
                for tool in &tools {
                    let schema =
                        serde_json::to_value(&tool.input_schema).unwrap_or_else(|_| json!({}));
                    let _ = sqlx::query(
                        r#"
                    INSERT INTO mcp_server_tools (server_id, tool_name, description, input_schema, discovered_at)
                    VALUES ($1, $2, $3, $4, NOW())
                    ON CONFLICT (server_id, tool_name)
                    DO UPDATE SET description=$3, input_schema=$4, discovered_at=NOW()
                    "#,
                    )
                    .bind(resolution.mcp_server_id)
                    .bind(&tool.name)
                    .bind(&tool.description)
                    .bind(schema)
                    .execute(&state.db)
                    .await;
                }

                let policy_row = sqlx::query(
                    "SELECT mode, tools FROM plugin_instance_tool_policies WHERE plugin_instance_id = $1",
                )
                .bind(plugin_instance_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
                if let Some(policy_row) = policy_row {
                    let mode: String = policy_row
                        .try_get("mode")
                        .unwrap_or_else(|_| "all".to_string());
                    let current_tools: Value = policy_row.try_get("tools").unwrap_or(json!([]));
                    let current_count = current_tools.as_array().map(|a| a.len()).unwrap_or(0);
                    if mode == "allowlist" && current_count == 0 {
                        let discovered = tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
                        let _ = sqlx::query(
                            "UPDATE plugin_instance_tool_policies SET tools = $2, updated_at = NOW() WHERE plugin_instance_id = $1",
                        )
                        .bind(plugin_instance_id)
                        .bind(json!(discovered))
                        .execute(&state.db)
                        .await;
                    }
                }

                let payload = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect::<Vec<_>>();
                (true, payload.len() as i32, None, payload)
            }
            Err(error) => {
                let raw_error = error.to_string();
                let mut msg = format_compact_error(&raw_error);
                if resolution.plugin_slug.eq_ignore_ascii_case("figma-http")
                    && raw_error.contains("HTTP 401")
                {
                    let token_hint = resolve_secret_value(&state.db, "figma_oauth_token").await;
                    msg = if token_hint.as_deref().map(is_figma_pat).unwrap_or(false) {
                        "MCP Figma ha rifiutato la connessione remota (401). Il token PAT figd_ funziona con REST API ma può non essere valido sul remote MCP: in Nexus è attivo fallback stdio `figma-developer-mcp`. Verifica token e riesegui il test."
                            .to_string()
                    } else {
                        "MCP Figma ha rifiutato la connessione remota (401). Verifica OAuth app Figma (client_id/client_secret), scope `mcp:connect` e callback."
                            .to_string()
                    };
                }
                (false, 0, Some(msg.clone()), Vec::new())
            }
        };

    let _ = sqlx::query(
        r#"
        INSERT INTO plugin_instance_health_runs
            (plugin_instance_id, tested_by_user_id, success, tool_count, error_message, details)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(plugin_instance_id)
    .bind(user_id)
    .bind(success)
    .bind(tool_count)
    .bind(error_message.clone())
    .bind(json!({
        "mcpServerId": resolution.mcp_server_id.to_string(),
        "mcpServerName": resolution.mcp_server_name,
    }))
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        UPDATE plugin_instances
        SET health_status = $2,
            last_health_message = $3,
            last_tested_at = NOW(),
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(plugin_instance_id)
    .bind(if success { "ok" } else { "error" })
    .bind(error_message.clone())
    .execute(&state.db)
    .await;

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        existing.try_get("project_id").unwrap_or(None),
        "test",
        if success { "ok" } else { "error" },
        error_message.clone(),
        json!({
            "toolCount": tool_count,
            "mcpServerId": resolution.mcp_server_id.to_string(),
        }),
    )
    .await;

    Ok(Json(json!({
        "success": success,
        "toolCount": tool_count,
        "error": error_message,
        "tools": tools_payload,
    })))
}

pub async fn get_plugin_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;

    let health_runs = sqlx::query(
        r#"
        SELECT id, success, tool_count, error_message, details, created_at
        FROM plugin_instance_health_runs
        WHERE plugin_instance_id = $1
        ORDER BY created_at DESC
        LIMIT 20
        "#,
    )
    .bind(plugin_instance_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let runs = health_runs
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "success": row.try_get::<bool, _>("success").unwrap_or(false),
                "toolCount": row.try_get::<i32, _>("tool_count").unwrap_or(0),
                "errorMessage": row.try_get::<Option<String>, _>("error_message").unwrap_or(None),
                "details": row.try_get::<Value, _>("details").unwrap_or(json!({})),
                "createdAt": row.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({
        "pluginInstanceId": plugin_instance_id.to_string(),
        "status": existing.try_get::<String, _>("health_status").unwrap_or_else(|_| "unknown".to_string()),
        "lastMessage": existing.try_get::<Option<String>, _>("last_health_message").unwrap_or(None),
        "lastTestedAt": existing.try_get::<Option<DateTime<Utc>>, _>("last_tested_at").unwrap_or(None).map(|v| v.to_rfc3339()),
        "runs": runs,
    })))
}

pub async fn update_plugin_tool_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(plugin_instance_id): AxumPath<String>,
    Json(body): Json<UpdateToolPolicyRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let plugin_instance_id = Uuid::parse_str(&plugin_instance_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Plugin id non valido"))?;
    let existing = resolve_plugin_instance_for_user(&state.db, plugin_instance_id, user_id).await?;
    if !can_manage_instance(&existing, user_id, &claims.role) {
        return Err(api_error(StatusCode::FORBIDDEN, "Plugin non modificabile"));
    }

    let mode = body.mode.trim().to_lowercase();
    if !matches!(mode.as_str(), "allowlist" | "denylist" | "all") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Mode non valido: usa allowlist, denylist o all",
        ));
    }

    let tools_json = json!(body.tools.unwrap_or_default());
    let blocked_tools_json = json!(body.blocked_tools.unwrap_or_default());
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
    .bind(&mode)
    .bind(&tools_json)
    .bind(&blocked_tools_json)
    .bind(user_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    write_plugin_audit(
        &state.db,
        Some(plugin_instance_id),
        Some(user_id),
        existing.try_get("project_id").unwrap_or(None),
        "tool_policy_update",
        "ok",
        Some("Policy tool aggiornata".to_string()),
        json!({ "mode": mode, "tools": tools_json, "blockedTools": blocked_tools_json }),
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "mode": mode,
        "tools": tools_json,
        "blockedTools": blocked_tools_json,
    })))
}
