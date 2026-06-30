//! Handler per i gruppi `project` e `profile` del server Nexus Builtin.

use super::*;

// ---------------------------------------------------------------------------
// Handler: project
// ---------------------------------------------------------------------------

pub(super) async fn handle_project_list(db: &PgPool, user_id: Uuid) -> String {
    match sqlx::query(
        "SELECT p.id, p.name, p.repository_root_path, p.created_at,
                pa.can_write, pa.role as member_role
         FROM projects p
         JOIN project_access pa ON pa.project_id = p.id
         WHERE pa.user_id = $1 AND p.deleted_at IS NULL
         ORDER BY p.updated_at DESC NULLS LAST, p.created_at DESC
         LIMIT 50",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            let projects: Vec<Value> = rows.iter().map(|r| json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "path": r.try_get::<Option<String>, _>("repository_root_path").unwrap_or(None),
                "role": r.try_get::<String, _>("member_role").unwrap_or_default(),
            })).collect();
            format_json(&json!({ "projects": projects, "count": projects.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_project_analyze(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    // Avvia un'analisi di base del progetto (aggiorna timestamp analysis)
    match sqlx::query("UPDATE projects SET analyzed_at=NOW() WHERE id=$1 RETURNING id")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(_)) => format_json(&json!({
            "ok": true,
            "message": "Analisi avviata. Usa 'nexus_project_list' per verificare lo stato.",
            "project_id": project_id.to_string()
        })),
        Ok(None) => "[Errore] Progetto non trovato".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_project_quality_scan(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    match sqlx::query("SELECT id, name, repository_root_path FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(r)) => {
            let name: String = r.try_get("name").unwrap_or_default();
            format_json(&json!({
                "ok": true,
                "project": name,
                "message": "Per avviare una scansione completa, usa run_command con il comando nexus-quality-scan oppure usa il pannello Ottimizzazione nell'IDE.",
                "tip": "I risultati esistenti sono disponibili con nexus_project_quality_findings."
            }))
        }
        Ok(None) => "[Errore] Progetto non trovato".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_project_quality_findings(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let severity_filter = args
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("all");

    let query_str = if severity_filter == "all" {
        "SELECT id, rule_key, severity, file_path, line_number, message, is_false_positive, created_at
         FROM project_quality_findings
         WHERE project_id=$1 AND is_false_positive=false
         ORDER BY severity DESC, created_at DESC
         LIMIT 100"
    } else {
        "SELECT id, rule_key, severity, file_path, line_number, message, is_false_positive, created_at
         FROM project_quality_findings
         WHERE project_id=$1 AND severity=$2 AND is_false_positive=false
         ORDER BY created_at DESC
         LIMIT 100"
    };

    let query = if severity_filter == "all" {
        sqlx::query(query_str).bind(project_id)
    } else {
        sqlx::query(query_str)
            .bind(project_id)
            .bind(severity_filter)
    };

    match query.fetch_all(db).await {
        Ok(rows) => {
            let findings: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                        "rule": r.try_get::<String, _>("rule_key").unwrap_or_default(),
                        "severity": r.try_get::<String, _>("severity").unwrap_or_default(),
                        "file": r.try_get::<Option<String>, _>("file_path").unwrap_or(None),
                        "line": r.try_get::<Option<i32>, _>("line_number").unwrap_or(None),
                        "message": r.try_get::<String, _>("message").unwrap_or_default(),
                    })
                })
                .collect();
            format_json(&json!({ "findings": findings, "count": findings.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

// ---------------------------------------------------------------------------
// Handler: profile
// ---------------------------------------------------------------------------

pub(super) async fn handle_profile_list(db: &PgPool, user_id: Uuid) -> String {
    match sqlx::query(
        "SELECT id, name, avatar_emoji, description, is_default, default_provider, default_model
         FROM user_profiles WHERE user_id=$1 ORDER BY is_default DESC, name ASC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            let profiles: Vec<Value> = rows.iter().map(|r| json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "emoji": r.try_get::<Option<String>, _>("avatar_emoji").unwrap_or(None),
                "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
                "isDefault": r.try_get::<bool, _>("is_default").unwrap_or(false),
                "provider": r.try_get::<Option<String>, _>("default_provider").unwrap_or(None),
                "model": r.try_get::<Option<String>, _>("default_model").unwrap_or(None),
            })).collect();
            format_json(&json!({ "profiles": profiles, "count": profiles.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_profile_delete(db: &PgPool, user_id: Uuid, args: &Value) -> String {
    let profile_id = match parse_uuid(args, "profile_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    match sqlx::query("DELETE FROM user_profiles WHERE id=$1 AND user_id=$2")
        .bind(profile_id)
        .bind(user_id)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => format_json(&json!({ "ok": true })),
        Ok(_) => {
            "[Errore] Profilo non trovato o non eliminabile (non appartiene all'utente)".to_string()
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_profile_set_default(db: &PgPool, user_id: Uuid, args: &Value) -> String {
    let profile_id = match parse_uuid(args, "profile_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    // Rimuove il default corrente e imposta quello nuovo
    let _ = sqlx::query("UPDATE user_profiles SET is_default=false WHERE user_id=$1")
        .bind(user_id)
        .execute(db)
        .await;

    match sqlx::query("UPDATE user_profiles SET is_default=true WHERE id=$1 AND user_id=$2")
        .bind(profile_id)
        .bind(user_id)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            format_json(&json!({ "ok": true, "default": profile_id.to_string() }))
        }
        Ok(_) => "[Errore] Profilo non trovato o non modificabile".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}
