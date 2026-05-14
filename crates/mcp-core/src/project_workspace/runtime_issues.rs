//! Fix M10: API per `project_runtime_issues` — errori runtime catturati dai tool agente
//! (run_command exit != 0, browser-check console errors, ecc.) accessibili al frontend
//! per essere mostrati nei pannelli con bottone "Risolvi con Nexus".
//!
//! Endpoint:
//! - GET    /api/projects/:id/runtime-issues  — lista issue aperte (con filtri)
//! - POST   /api/projects/:id/runtime-issues  — INSERT nuova (chiamata da hook tool)
//! - PATCH  /api/projects/:id/runtime-issues/:issue_id  — aggiorna status (open/in_progress/resolved)
//! - DELETE /api/projects/:id/runtime-issues/:issue_id  — rimuove (cleanup manuale)

use super::*;

pub async fn list_runtime_issues(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let status_filter = params.get("status").map(|s| s.as_str()).unwrap_or("open");
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50)
        .min(200);

    let rows = sqlx::query(
        r#"
        SELECT id, source, severity, message, details, tool_name, command, exit_code,
               status, fingerprint, run_id, step_id, created_at, updated_at, resolved_at
          FROM project_runtime_issues
         WHERE project_id = $1
           AND ($2 = 'all' OR status = $2)
         ORDER BY created_at DESC
         LIMIT $3
        "#,
    )
    .bind(project_id)
    .bind(status_filter)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {}", e)))?;

    let issues: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "source": r.get::<String, _>("source"),
                "severity": r.get::<String, _>("severity"),
                "message": r.get::<String, _>("message"),
                "details": r.try_get::<Option<String>, _>("details").ok().flatten(),
                "tool_name": r.try_get::<Option<String>, _>("tool_name").ok().flatten(),
                "command": r.try_get::<Option<String>, _>("command").ok().flatten(),
                "exit_code": r.try_get::<Option<i32>, _>("exit_code").ok().flatten(),
                "status": r.get::<String, _>("status"),
                "fingerprint": r.try_get::<Option<String>, _>("fingerprint").ok().flatten(),
                "run_id": r.try_get::<Option<Uuid>, _>("run_id").ok().flatten().map(|u| u.to_string()),
                "step_id": r.try_get::<Option<Uuid>, _>("step_id").ok().flatten().map(|u| u.to_string()),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
                "resolved_at": r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("resolved_at").ok().flatten().map(|t| t.to_rfc3339()),
            })
        })
        .collect();

    Ok(Json(json!({
        "issues": issues,
        "count": issues.len(),
        "status_filter": status_filter,
    })))
}

pub async fn create_runtime_issue(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let source = body
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'source' obbligatorio"))?;
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'message' obbligatorio"))?;
    let severity = body.get("severity").and_then(Value::as_str).unwrap_or("error");
    let details = body.get("details").and_then(Value::as_str);
    let tool_name = body.get("tool_name").and_then(Value::as_str);
    let command = body.get("command").and_then(Value::as_str);
    let exit_code = body.get("exit_code").and_then(Value::as_i64).map(|i| i as i32);

    // Fingerprint per dedup: sha1(message + command)
    let fingerprint = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(message.as_bytes());
        if let Some(c) = command {
            h.update(b"|");
            h.update(c.as_bytes());
        }
        format!("{:x}", h.finalize())[..16].to_string()
    };

    let row = sqlx::query(
        r#"
        INSERT INTO project_runtime_issues
          (project_id, source, severity, message, details, tool_name, command, exit_code, fingerprint, status)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'open')
        ON CONFLICT (project_id, fingerprint) WHERE fingerprint IS NOT NULL DO UPDATE
          SET updated_at = NOW(), status = CASE WHEN project_runtime_issues.status = 'resolved' THEN 'open' ELSE project_runtime_issues.status END
        RETURNING id
        "#,
    )
    .bind(project_id)
    .bind(source)
    .bind(severity)
    .bind(message)
    .bind(details)
    .bind(tool_name)
    .bind(command)
    .bind(exit_code)
    .bind(&fingerprint)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB insert: {}", e)))?;

    Ok(Json(json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "fingerprint": fingerprint,
        "ok": true,
    })))
}

pub async fn update_runtime_issue(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, issue_id_str)): AxumPath<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let issue_id = Uuid::parse_str(&issue_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Issue id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let new_status = body
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'status' obbligatorio"))?;

    if !["open", "in_progress", "resolved"].contains(&new_status) {
        return Err(api_error(StatusCode::BAD_REQUEST, "Status non valido"));
    }

    let resolved_at_sql = if new_status == "resolved" {
        "NOW()"
    } else {
        "NULL"
    };

    let sql = format!(
        r#"
        UPDATE project_runtime_issues
           SET status = $1, updated_at = NOW(), resolved_at = {}
         WHERE id = $2 AND project_id = $3
         RETURNING id
        "#,
        resolved_at_sql
    );

    let row = sqlx::query(&sql)
        .bind(new_status)
        .bind(issue_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {}", e)))?;

    if row.is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Issue non trovata"));
    }

    Ok(Json(json!({"ok": true, "status": new_status})))
}
