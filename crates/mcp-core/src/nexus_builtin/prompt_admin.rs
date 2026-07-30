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

/// Costruisce l'esito FALLITO di una query DB in questo file: marker piu'
/// messaggio leggibile (contratto `nexus_types::tool_outcome`). Estrae la
/// ripetizione dei 4 siti `Err(e) => format!("[Errore DB] {e}")`: senza
/// marker questi fallimenti erano indistinguibili da un successo per
/// anti-loop/supervisore/final_gate (regola M).
fn db_tool_failure(e: impl std::fmt::Display) -> String {
    tool_failure(format!("[Errore DB] {e}"))
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
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_prompt_template_update(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'key' obbligatorio"),
    };
    let content = match args.get("content").and_then(Value::as_str) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => return tool_failure("[Errore] Parametro 'content' obbligatorio"),
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
        Ok(None) => tool_failure("[Errore] Aggiornamento fallito"),
        Err(e) => db_tool_failure(e),
    }
}

// ---------------------------------------------------------------------------
// Handler: admin_settings
// ---------------------------------------------------------------------------

pub(super) async fn handle_admin_setting_get(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'key' obbligatorio"),
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
        Ok(None) => tool_failure(format!("[Non trovato] Setting '{}' non esiste", key)),
        Err(e) => db_tool_failure(e),
    }
}

pub(super) async fn handle_admin_setting_update(db: &PgPool, args: &Value) -> String {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'key' obbligatorio"),
    };
    let value = match args.get("value").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => return tool_failure("[Errore] Parametro 'value' obbligatorio"),
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
        Err(e) => db_tool_failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_tool_failure_dichiara_il_fallimento_e_preserva_il_messaggio() {
        // Chiama il PRODUTTORE reale usato dai 4 rami `Err(e)` dei quattro
        // handler di questo file: senza marker questi fallimenti erano
        // indistinguibili da un successo per anti-loop/supervisore/
        // final_gate (regola M), raggiungibili dal ramo di fallback
        // `other if other.starts_with("nexus_")` in
        // `agent_tools::dispatch::execute_agent_tool`.
        let out = db_tool_failure("connessione al DB persa");
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        assert!(out.contains("[Errore DB] connessione al DB persa"));
    }

    #[test]
    fn validazione_parametro_obbligatorio_e_un_fallimento() {
        // Stessa forma letterale usata nei rami di validazione di
        // `handle_prompt_template_update` / `handle_admin_setting_get` /
        // `handle_admin_setting_update` per key/content/value mancanti.
        let out = tool_failure("[Errore] Parametro 'key' obbligatorio");
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn aggiornamento_senza_riga_ritornata_e_un_fallimento() {
        // Ramo `Ok(None)` di `handle_prompt_template_update`: la query non
        // ha restituito errore ma nemmeno la riga RETURNING attesa, quindi
        // l'operazione richiesta (scrivere il template) non e' avvenuta.
        let out = tool_failure("[Errore] Aggiornamento fallito");
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn setting_non_trovato_e_un_fallimento() {
        // Ramo `Ok(None)` di `handle_admin_setting_get`: la risorsa
        // richiesta non esiste e la chiamata si ferma li' (vedi CONTESTO,
        // criterio "risorsa non trovata e l'intera chiamata si ferma li'").
        let out = tool_failure(format!("[Non trovato] Setting '{}' non esiste", "foo.bar"));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        assert!(out.contains("foo.bar"));
    }

    #[test]
    fn successo_con_payload_json_non_e_un_fallimento() {
        // Controllo di non regressione: i rami di successo (format_json)
        // di questo file non devono mai essere marcati come falliti, anche
        // quando il payload contiene la parola "error" o simili come dato.
        let out = format_json(&json!({ "ok": true, "key": "k", "value": "v" }));
        assert!(!nexus_types::tool_outcome::is_tool_failure(&out));
    }
}
