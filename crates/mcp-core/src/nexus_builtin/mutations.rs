//! Handler dei tool agente per il sistema file_mutations (mig 0349).
//!
//! Tutta la logica reale vive in `crate::file_mutations` (punto unico,
//! regola L). Qui solo: parsing argomenti, formattazione output testuale per
//! l'agente, mapping di errori.

use super::*;

pub(super) async fn handle_file_mutations_list(
    db: &PgPool,
    project_id: Uuid,
    args: &Value,
) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => Uuid::parse_str(s).unwrap_or(project_id),
        None => project_id,
    };
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50);

    match crate::file_mutations::list_recent_mutations(db, pid, limit).await {
        Ok(rows) => format_json(&json!({
            "count": rows.len(),
            "mutations": rows,
        })),
        Err(e) => format!("[Errore] list_recent_mutations fallita: {e}"),
    }
}

pub(super) async fn handle_file_mutation_diff(
    db: &PgPool,
    project_id: Uuid,
    args: &Value,
) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => Uuid::parse_str(s).unwrap_or(project_id),
        None => project_id,
    };
    let Some(mid) = args.get("mutation_id").and_then(Value::as_i64) else {
        return "[Errore] parametro 'mutation_id' obbligatorio".to_string();
    };

    match crate::file_mutations::get_mutation_full(db, pid, mid).await {
        Ok(Some(v)) => format_json(&v),
        Ok(None) => format!("[Errore] mutazione {mid} non trovata nel progetto"),
        Err(e) => format!("[Errore] get_mutation_full fallita: {e}"),
    }
}

pub(super) async fn handle_file_revert(
    db: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    args: &Value,
) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => Uuid::parse_str(s).unwrap_or(project_id),
        None => project_id,
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

    // Determina la project root del progetto (path-traversal-safe).
    let root_row =
        sqlx::query("SELECT w.absolute_path FROM workspaces w WHERE w.project_id = $1 AND w.is_primary = TRUE")
            .bind(pid)
            .fetch_optional(db)
            .await;
    let root_path = match root_row {
        Ok(Some(r)) => r
            .try_get::<String, _>("absolute_path")
            .map(std::path::PathBuf::from)
            .unwrap_or_default(),
        _ => return "[Errore] workspace primario del progetto non trovato".to_string(),
    };
    if root_path.as_os_str().is_empty() {
        return "[Errore] project root vuota".to_string();
    }

    // Risolvi mutation_id: esplicito, oppure "ultima annullabile".
    let mutation_id = if let Some(mid) = args.get("mutation_id").and_then(Value::as_i64) {
        mid
    } else {
        let last: Option<i64> = sqlx::query_scalar(
            r#"SELECT id FROM file_mutations
                WHERE project_id = $1
                  AND reverted_at IS NULL
                  AND op <> 'reverted'
                  AND before_content IS NOT NULL
                ORDER BY created_at DESC, id DESC
                LIMIT 1"#,
        )
        .bind(pid)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        match last {
            Some(m) => m,
            None => {
                return "[Errore] nessuna mutazione annullabile per questo progetto".to_string()
            }
        }
    };

    match crate::file_mutations::revert_mutation(
        db,
        pid,
        &root_path,
        Some(user_id),
        None,
        mutation_id,
        force,
    )
    .await
    {
        crate::file_mutations::RevertOutcome::Reverted { new_mutation_id } => {
            // Notifica i pannelli del progetto via SSE.
            if let Some(path) = sqlx::query_scalar::<_, String>(
                "SELECT file_path FROM file_mutations WHERE id = $1",
            )
            .bind(new_mutation_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            {
                let _ = nexus_events::dispatcher::emit_global(
                    pid,
                    nexus_events::event::ProjectEvent::FileChanged {
                        path,
                        op: "modified".to_string(),
                    },
                );
            }
            format_json(&json!({
                "ok": true,
                "reverted_mutation_id": mutation_id,
                "new_mutation_id": new_mutation_id,
                "message": format!("Mutazione {mutation_id} annullata"),
            }))
        }
        crate::file_mutations::RevertOutcome::NotFound => {
            format!("[Errore] mutazione {mutation_id} non trovata")
        }
        crate::file_mutations::RevertOutcome::AlreadyReverted => {
            format!("[Errore] mutazione {mutation_id} gia' annullata")
        }
        crate::file_mutations::RevertOutcome::NotRevertible(reason) => {
            format!("[Errore] ripristino non disponibile: {reason}")
        }
        crate::file_mutations::RevertOutcome::Conflict {
            current_sha,
            expected_sha,
        } => format!(
            "[Errore] conflict: il file e' stato modificato dopo la mutazione \
             (current_sha={current_sha}, expected_sha={expected_sha}). \
             Conferma con l'utente e rilancia il tool con force=true."
        ),
        crate::file_mutations::RevertOutcome::IoError(e) => {
            format!("[Errore] {e}")
        }
    }
}
