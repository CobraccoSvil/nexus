//! Handler per i gruppi `prompt_template` e `admin_settings` del server Nexus Builtin.
//! Include anche le funzioni helper `parse_uuid` e `format_json`.

use super::*;

// ---------------------------------------------------------------------------
// Helpers condivisi
// ---------------------------------------------------------------------------

pub(super) fn parse_uuid(args: &Value, field: &str) -> Result<Uuid, String> {
    let s = args.get(field).and_then(Value::as_str).unwrap_or("");
    Uuid::parse_str(s)
        .map_err(|_| format!("[Errore] Parametro '{}' deve essere un UUID valido", field))
}

pub(super) fn format_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

// ---------------------------------------------------------------------------
// Handler: prompt_template
// ---------------------------------------------------------------------------

pub(super) async fn handle_prompt_template_list(db: &PgPool, args: &Value) -> String {
    let category_filter = args.get("category").and_then(Value::as_str);

    let result = if let Some(cat) = category_filter.as_ref() {
        sqlx::query(
            "SELECT key, category, title, is_active, version, updated_by, updated_at
             FROM nexus_prompt_templates WHERE category=$1 ORDER BY category, key",
        )
        .bind(cat)
        .fetch_all(db)
        .await
    } else {
        sqlx::query(
            "SELECT key, category, title, is_active, version, updated_by, updated_at
             FROM nexus_prompt_templates ORDER BY category, key",
        )
        .fetch_all(db)
        .await
    };

    match result {
        Ok(rows) => {
            let templates: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "key": r.try_get::<String, _>("key").unwrap_or_default(),
                        "category": r.try_get::<String, _>("category").unwrap_or_default(),
                        "title": r.try_get::<String, _>("title").unwrap_or_default(),
                        "isActive": r.try_get::<bool, _>("is_active").unwrap_or(true),
                        "version": r.try_get::<i32, _>("version").unwrap_or(1),
                        "updatedBy": r.try_get::<String, _>("updated_by").unwrap_or_default(),
                    })
                })
                .collect();
            format_json(&json!({ "templates": templates, "count": templates.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_prompt_template_update(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return "[Errore] Parametro 'key' obbligatorio".to_string(),
    };
    let content = match args.get("content").and_then(Value::as_str) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => return "[Errore] Parametro 'content' obbligatorio".to_string(),
    };
    let change_note: Option<String> = args
        .get("change_note")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Salva history e aggiorna
    let current = sqlx::query("SELECT id, version FROM nexus_prompt_templates WHERE key=$1")
        .bind(&key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if let Some(cur) = &current {
        let tmpl_id: i32 = cur.try_get("id").unwrap_or(0);
        let _ = sqlx::query(
            "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note)
             SELECT id, content, version, 'nexus_agent', $2 FROM nexus_prompt_templates WHERE id=$1"
        )
        .bind(tmpl_id)
        .bind(&change_note)
        .execute(db)
        .await;
    }

    let result = if current.is_some() {
        sqlx::query(
            "UPDATE nexus_prompt_templates SET content=$1, version=version+1, updated_by='nexus_agent', updated_at=NOW()
             WHERE key=$2 RETURNING key, version"
        )
        .bind(&content)
        .bind(&key)
        .fetch_optional(db)
        .await
    } else {
        sqlx::query(
            "INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by)
             VALUES ($1, 'system', $1, $2, 'nexus_agent')
             RETURNING key, version",
        )
        .bind(&key)
        .bind(&content)
        .fetch_optional(db)
        .await
    };

    match result {
        Ok(Some(r)) => {
            let version: i32 = r.try_get("version").unwrap_or(1);
            format_json(&json!({ "ok": true, "key": key, "version": version }))
        }
        Ok(None) => "[Errore] Aggiornamento fallito".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

// ---------------------------------------------------------------------------
// Handler: admin_settings
// ---------------------------------------------------------------------------

pub(super) async fn handle_admin_setting_get(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return "[Errore] Parametro 'key' obbligatorio".to_string(),
    };
    match sqlx::query("SELECT key, value, description FROM settings WHERE key=$1")
        .bind(&key)
        .fetch_optional(db)
        .await
    {
        Ok(Some(r)) => format_json(&json!({
            "key": r.try_get::<String, _>("key").unwrap_or_default(),
            "value": r.try_get::<String, _>("value").unwrap_or_default(),
            "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
        })),
        Ok(None) => format!("[Non trovato] Setting '{}' non esiste", key),
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_admin_setting_update(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return "[Errore] Parametro 'key' obbligatorio".to_string(),
    };
    let value = match args.get("value").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => return "[Errore] Parametro 'value' obbligatorio".to_string(),
    };
    match sqlx::query(
        "INSERT INTO settings (key, value) VALUES ($1,$2)
         ON CONFLICT (key) DO UPDATE SET value=$2, updated_at=NOW()",
    )
    .bind(&key)
    .bind(&value)
    .execute(db)
    .await
    {
        Ok(_) => format_json(&json!({ "ok": true, "key": key, "value": value })),
        Err(e) => format!("[Errore DB] {e}"),
    }
}
