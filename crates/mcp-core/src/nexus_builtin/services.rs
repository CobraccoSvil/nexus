//! Handler per il gruppo `service_control` del server Nexus Builtin.
//! Gestisce stato e controllo dei servizi systemd associati al progetto.

use super::*;

pub(super) async fn handle_service_status(db: &PgPool, project_id: Uuid) -> String {
    let slug = match get_project_slug(db, project_id).await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let prefix = format!("{}-", slug);

    let out = match tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return format!("[Errore] systemctl non disponibile: {}", e),
    };

    let mut services: Vec<serde_json::Value> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let unit = cols[0].trim_start_matches('●').trim();
        if !unit.starts_with(&prefix) || !unit.ends_with(".service") {
            continue;
        }
        let short = unit
            .strip_prefix(&prefix)
            .unwrap_or(unit)
            .strip_suffix(".service")
            .unwrap_or(unit);
        services.push(serde_json::json!({
            "unit":  unit,
            "short": short,
            "state": cols[2],
            "sub":   cols[3],
        }));
    }

    if services.is_empty() {
        return format!("Nessun servizio systemd trovato con prefisso '{}'.\nAssicurati che i servizi siano installati come unità --user.", prefix);
    }

    serde_json::json!({ "slug": slug, "services": services }).to_string()
}

pub(super) async fn handle_service_control(db: &PgPool, project_id: Uuid, args: &Value) -> String {
    let slug = match get_project_slug(db, project_id).await {
        Ok(s) => s,
        Err(e) => return e,
    };
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
    // Costruisce il nome unit accettando sia il nome corto che il nome completo
    let svc_name = if service.starts_with(&format!("{}-", slug)) {
        format!("{}.service", service)
    } else {
        format!("{}-{}.service", slug, service)
    };

    let out = tokio::process::Command::new("systemctl")
        .args(["--user", &action, &svc_name])
        .output()
        .await;
    match out {
        Ok(o) => {
            let ok = o.status.success();
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let msg = if ok {
                format!("OK: {} → {} completato.", svc_name, action)
            } else {
                format!("ERRORE: {} → {} fallito.\n{}", svc_name, action, stderr)
            };
            serde_json::json!({ "ok": ok, "unit": svc_name, "action": action, "stdout": stdout, "stderr": stderr, "message": msg }).to_string()
        }
        Err(e) => format!("[Errore di sistema] {}", e),
    }
}
