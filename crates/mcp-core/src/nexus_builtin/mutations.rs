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
        Err(e) => tool_failure(format!("[Errore] list_recent_mutations fallita: {e}")),
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
        return tool_failure("[Errore] parametro 'mutation_id' obbligatorio");
    };

    match crate::file_mutations::get_mutation_full(db, pid, mid).await {
        Ok(Some(v)) => format_json(&v),
        Ok(None) => tool_failure(format!("[Errore] mutazione {mid} non trovata nel progetto")),
        Err(e) => tool_failure(format!("[Errore] get_mutation_full fallita: {e}")),
    }
}

/// Ritorna info sul branch di auto-commit della sessione corrente / progetto:
/// prefisso configurato, comandi git pronti all'uso per ispezionare/mergiare/
/// scartare l'intera sessione.
pub(super) async fn handle_session_branch_info(
    db: &PgPool,
    project_id: Uuid,
    args: &Value,
) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => Uuid::parse_str(s).unwrap_or(project_id),
        None => project_id,
    };

    let cfg = crate::session_autocommit::load_config(db).await;
    let prefix = cfg.branch_prefix.trim_end_matches('/').to_string();

    // Conta i branch nexus per il progetto leggendo la project root.
    let root_row = sqlx::query(
        "SELECT w.absolute_path FROM workspaces w \
         WHERE w.project_id = $1 AND w.is_primary = TRUE",
    )
    .bind(pid)
    .fetch_optional(db)
    .await;
    let root: Option<String> = match root_row {
        Ok(Some(r)) => r.try_get::<String, _>("absolute_path").ok(),
        _ => None,
    };

    format_json(&json!({
        "enabled": cfg.enabled,
        "branch_prefix": cfg.branch_prefix,
        "branch_pattern": format!("{prefix}/*"),
        "project_root": root,
        "commands": {
            "list_session_branches": format!("git branch --list '{prefix}/*'"),
            "list_session_log_template": format!("git log --oneline {prefix}/<short_id>"),
            "diff_full_session_template": format!("git diff HEAD..{prefix}/<short_id>"),
            "merge_session_into_current_template": format!("git merge --no-ff {prefix}/<short_id>"),
            "discard_session_template": format!("git branch -D {prefix}/<short_id>")
        },
        "hint": "Il <short_id> e' visibile nei messaggi di commit (es. 'agent: ... (session a1b2c3d4)'). Usa `git branch --list` per elencarli tutti."
    }))
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
    let root_row = sqlx::query(
        "SELECT w.absolute_path FROM workspaces w WHERE w.project_id = $1 AND w.is_primary = TRUE",
    )
    .bind(pid)
    .fetch_optional(db)
    .await;
    let root_path = match root_row {
        Ok(Some(r)) => r
            .try_get::<String, _>("absolute_path")
            .map(std::path::PathBuf::from)
            .unwrap_or_default(),
        _ => return tool_failure("[Errore] workspace primario del progetto non trovato"),
    };
    if root_path.as_os_str().is_empty() {
        return tool_failure("[Errore] project root vuota");
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
                return tool_failure("[Errore] nessuna mutazione annullabile per questo progetto")
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
            tool_failure(format!("[Errore] mutazione {mutation_id} non trovata"))
        }
        crate::file_mutations::RevertOutcome::AlreadyReverted => {
            tool_failure(format!("[Errore] mutazione {mutation_id} gia' annullata"))
        }
        crate::file_mutations::RevertOutcome::NotRevertible(reason) => {
            tool_failure(format!("[Errore] ripristino non disponibile: {reason}"))
        }
        crate::file_mutations::RevertOutcome::Conflict {
            current_sha,
            expected_sha,
        } => tool_failure(format!(
            "[Errore] conflict: il file e' stato modificato dopo la mutazione \
             (current_sha={current_sha}, expected_sha={expected_sha}). \
             Conferma con l'utente e rilancia il tool con force=true."
        )),
        crate::file_mutations::RevertOutcome::IoError(e) => tool_failure(format!("[Errore] {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_failure_dichiara_il_fallimento_sui_rami_di_errore_testuali() {
        // Chiama il PRODUTTORE reale usato dai rami di errore di questo file
        // (list/diff/revert): senza il marker in testa questi fallimenti
        // erano indistinguibili da un risultato riuscito per anti-loop/
        // supervisore/final_gate (regola M), raggiungibili dal ramo di
        // fallback `other if other.starts_with("nexus_")` in
        // `agent_tools::dispatch::execute_agent_tool`.
        let out = tool_failure(format!("[Errore] mutazione {} non trovata", 42));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        assert!(out.contains("mutazione 42 non trovata"));
    }

    #[test]
    fn tool_failure_non_raddoppia_il_marker_su_ri_wrap() {
        // Propagazione a catena: `handle_file_revert` inoltra a volte un
        // messaggio gia' costruito da un ramo interno (es. IoError che
        // avvolge un errore gia' marcato altrove); un doppio marker
        // romperebbe `trim_start_matches` lato consumatore.
        let una_volta = tool_failure("[Errore] project root vuota");
        let due_volte = tool_failure(&una_volta);
        assert_eq!(una_volta, due_volte);
        assert_eq!(
            due_volte
                .matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
                .count(),
            1
        );
    }

    #[test]
    fn revert_su_mutazione_gia_annullata_e_un_fallimento_non_un_successo() {
        // Caso critico segnalato dal task: un revert che NON ha compiuto
        // l'operazione richiesta (perche' la mutazione era gia' revertita)
        // deve dichiararsi fallito, altrimenti l'agente/il supervisore
        // resterebbero convinti che il file sia tornato allo stato
        // precedente quando non e' stato toccato affatto. Riproduce
        // esattamente il payload prodotto dal ramo
        // `RevertOutcome::AlreadyReverted` di `handle_file_revert`.
        let mutation_id: i64 = 7;
        let out = tool_failure(format!("[Errore] mutazione {mutation_id} gia' annullata"));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn revert_in_conflitto_e_un_fallimento() {
        // Stesso principio per il ramo `Conflict`: il file su disco non
        // corrisponde all'atteso, il revert si e' fermato senza scrivere
        // nulla. Deve restare distinguibile da un `Reverted` riuscito.
        let out = tool_failure(format!(
            "[Errore] conflict: il file e' stato modificato dopo la mutazione \
             (current_sha={}, expected_sha={}). \
             Conferma con l'utente e rilancia il tool con force=true.",
            "abc123", "def456"
        ));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn un_revert_riuscito_non_porta_il_marker() {
        // Controprova: il payload di successo (`RevertOutcome::Reverted`,
        // costruito con `format_json`) non deve MAI portare il marker, o un
        // revert davvero riuscito verrebbe letto come fallito a valle.
        let ok_payload = format_json(&json!({
            "ok": true,
            "reverted_mutation_id": 7,
            "new_mutation_id": 8,
            "message": "Mutazione 7 annullata",
        }));
        assert!(!nexus_types::tool_outcome::is_tool_failure(&ok_payload));
    }
}
