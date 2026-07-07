//! Handler per il gruppo `service_control` del server Nexus Builtin.
//! Gestisce stato e controllo dei "servizi del progetto" delegando al punto unico
//! `service_manager` (regola L): su Windows sono processi gestiti in
//! `agent_processes`, su Linux unit `systemd --user`. Prima questi handler
//! chiamavano direttamente `systemctl` e su Windows erano ciechi (l'agente
//! concludeva che nulla girava e lanciava duplicati del dev server).

use super::*;

use crate::project_workspace::service_manager::{self, ServiceBackend};

/// Nome + root del progetto (contesto minimo per il ServiceManager). Ritorna un
/// messaggio di errore gia' formattato in caso di progetto assente.
async fn project_name_and_root(
    db: &PgPool,
    project_id: Uuid,
) -> Result<(String, Option<String>), String> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT name, repository_root_path FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("[DB] {e}"))?;
    row.ok_or_else(|| "[Errore] Progetto non trovato".to_string())
}

pub(super) async fn handle_service_status(db: &PgPool, project_id: Uuid) -> String {
    let (name, root) = match project_name_and_root(db, project_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let slug = name.to_lowercase().replace([' ', '_'], "-");
    let root_path = std::path::PathBuf::from(root.unwrap_or_default());

    let ctx = service_manager::ServiceContext {
        db,
        // Contesto tool agente: nessun registry porte (non e' un handler HTTP con
        // AppState). Sufficiente per list/status; lo start di un web service non
        // iniettera' PORT (il flusso run_service/run_config lo gestisce).
        port_registry: None,
        project_id,
        slug: &slug,
        project_root: &root_path,
    };

    let services: Vec<serde_json::Value> = service_manager::active()
        .list(&ctx)
        .await
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "service": e.label,
                "state": e.state,
                "managed_by": e.managed_by,
            })
        })
        .collect();

    if services.is_empty() {
        return format!(
            "Nessun servizio del progetto '{slug}' risulta configurato o attivo. \
             Usa il pannello Run (+ Configura) o lo strumento run_service per crearne uno."
        );
    }

    serde_json::json!({ "slug": slug, "services": services }).to_string()
}

pub(super) async fn handle_service_control(db: &PgPool, project_id: Uuid, args: &Value) -> String {
    let (name, root) = match project_name_and_root(db, project_id).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let slug = name.to_lowercase().replace([' ', '_'], "-");
    let root_path = std::path::PathBuf::from(root.unwrap_or_default());

    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return "[Errore] Parametro 'service' obbligatorio".to_string(),
    };
    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return "[Errore] Parametro 'action' obbligatorio (start|stop|restart)".to_string(),
    };
    if service.contains('/') || service.contains("..") {
        return "[Errore] Nome servizio non valido".to_string();
    }
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return format!("[Errore] Azione non valida: {}", action);
    }

    // Nome corto: accetta sia il corto sia il nome completo ({slug}-x.service).
    let short = service
        .strip_prefix(&format!("{slug}-"))
        .unwrap_or(&service)
        .strip_suffix(".service")
        .unwrap_or(&service)
        .to_string();

    let ctx = service_manager::ServiceContext {
        db,
        port_registry: None,
        project_id,
        slug: &slug,
        project_root: &root_path,
    };
    let mgr = service_manager::active();
    let outcome = match action.as_str() {
        "start" => mgr.start(&ctx, &short).await,
        "stop" => mgr.stop(&ctx, &short).await,
        _ => mgr.restart(&ctx, &short).await,
    };

    // Esito da segnale strutturato (regola M): `acted` dice se l'azione e' avvenuta
    // davvero, non il parsing dello stdout di un comando.
    serde_json::json!({
        "ok": outcome.acted,
        "service": short,
        "action": action,
        "message": outcome.message,
    })
    .to_string()
}
