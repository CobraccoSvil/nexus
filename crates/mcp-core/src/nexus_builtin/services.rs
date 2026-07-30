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
        Err(e) => return tool_failure(e),
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
        Err(e) => return tool_failure(e),
    };
    let slug = name.to_lowercase().replace([' ', '_'], "-");
    let root_path = std::path::PathBuf::from(root.unwrap_or_default());

    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return tool_failure("[Errore] Parametro 'service' obbligatorio"),
    };
    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => {
            return tool_failure("[Errore] Parametro 'action' obbligatorio (start|stop|restart)")
        }
    };
    if service.contains('/') || service.contains("..") {
        return tool_failure("[Errore] Nome servizio non valido");
    }
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return tool_failure(format!("[Errore] Azione non valida: {}", action));
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

    control_outcome_payload(&outcome, &short, &action)
}

/// Compone il payload JSON dell'esito di start/stop/restart e lo marca come
/// fallimento del tool quando l'azione non ha agito davvero.
///
/// Esito da segnale strutturato (regola M): `acted` dice se l'azione e'
/// avvenuta davvero, non il parsing dello stdout di un comando. Il tool e' un
/// MUTATORE: se non ha agito (servizio non trovato, spawn fallito, DB non
/// disponibile...) l'operazione richiesta NON e' stata compiuta e va marcata
/// come fallimento, altrimenti anti-loop/supervisore leggono "ok": false come
/// un successo e rilanciano l'azione in loop.
fn control_outcome_payload(
    outcome: &service_manager::ServiceActionOutcome,
    short: &str,
    action: &str,
) -> String {
    let payload = serde_json::json!({
        "ok": outcome.acted,
        "service": short,
        "action": action,
        "message": outcome.message,
    })
    .to_string();
    if outcome.acted {
        payload
    } else {
        tool_failure(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parametri_e_validazioni_mancanti_dichiarano_fallimento() {
        // Stessi letterali usati dai rami di validazione di
        // `handle_service_control`: senza il marker questi errori (parametro
        // mancante, nome/azione non validi) erano indistinguibili da un esito
        // riuscito per anti-loop/supervisore/final_gate (regola M).
        assert!(nexus_types::tool_outcome::is_tool_failure(&tool_failure(
            "[Errore] Parametro 'service' obbligatorio"
        )));
        assert!(nexus_types::tool_outcome::is_tool_failure(&tool_failure(
            "[Errore] Parametro 'action' obbligatorio (start|stop|restart)"
        )));
        assert!(nexus_types::tool_outcome::is_tool_failure(&tool_failure(
            "[Errore] Nome servizio non valido"
        )));
        assert!(nexus_types::tool_outcome::is_tool_failure(&tool_failure(
            format!("[Errore] Azione non valida: {}", "unknown")
        )));
    }

    #[test]
    fn progetto_non_trovato_dichiara_fallimento() {
        // `project_name_and_root` e' condivisa (non va toccata): il punto di
        // ritorno finale di entrambi gli handler deve avvolgere la vecchia
        // convenzione testuale "[Errore] ...".
        let out = tool_failure("[Errore] Progetto non trovato".to_string());
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn azione_che_non_agisce_e_un_fallimento_del_mutatore() {
        // Chiama il PRODUTTORE reale di ServiceActionOutcome (service_manager):
        // `noop` copre sia "servizio non trovato" sia "spawn/systemctl
        // fallito" — in entrambi i casi l'azione richiesta (start/stop/
        // restart) non e' stata compiuta, e il tool deve dichiararlo, non
        // limitarsi a un campo "ok": false dentro un payload altrimenti letto
        // come successo dal solo marker in testa.
        let esito = service_manager::ServiceActionOutcome::noop("avvio fallito: spawn error");
        let out = control_outcome_payload(&esito, "api", "start");
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn azione_riuscita_non_e_un_fallimento() {
        let esito = service_manager::ServiceActionOutcome::acted("servizio 'api' avviato");
        let out = control_outcome_payload(&esito, "api", "start");
        assert!(!nexus_types::tool_outcome::is_tool_failure(&out));
        let parsed: Value =
            serde_json::from_str(&out).expect("payload e' JSON valido quando l'azione riesce");
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn wrap_del_fallimento_e_idempotente() {
        // Propagazione a catena: `control_outcome_payload` chiama
        // `tool_failure` sullo stesso punto unico usato dal resto del file,
        // che non raddoppia il marker su un payload gia' marcato.
        let esito = service_manager::ServiceActionOutcome::noop("servizio 'api' non trovato");
        let out = control_outcome_payload(&esito, "api", "stop");
        let due_volte = tool_failure(&out);
        assert_eq!(out, due_volte);
        assert_eq!(
            due_volte
                .matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
                .count(),
            1
        );
    }
}
