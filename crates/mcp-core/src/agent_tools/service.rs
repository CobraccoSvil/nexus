//! Tool servizio: avvio processi long-running, lettura output, stop, build immagine progetto.

use super::*;
use std::collections::HashMap;

/// Heuristica: il comando avvia un server web/long-running che ha bisogno di
/// una porta TCP? Riconosce:
/// - script comuni: `next dev|start`, `vite`, `webpack-dev-server`, `astro dev`
/// - framework Python: `gunicorn`, `uvicorn`, `flask run`, `django runserver`
/// - Node generici: `node server.js`, `npm run dev|start|serve`
/// - Rust/Go/.NET dev server: `cargo run`, `go run`, `dotnet run`, `dotnet watch`
///
/// In caso di dubbio (es. `make foo`), NON inietta PORT: l'agente puo' chiamare
/// `request_port` esplicitamente e includere la porta nel comando.
fn looks_like_web_service(command: &str) -> bool {
    let lc = command.to_lowercase();
    // Lista di token che indicano "sto avviando un server web"
    const WEB_TOKENS: &[&str] = &[
        "next dev",
        "next start",
        "vite",
        "vite dev",
        "vite preview",
        "webpack-dev-server",
        "webpack serve",
        "astro dev",
        "astro start",
        "astro preview",
        "nuxt dev",
        "nuxt start",
        "svelte-kit dev",
        "ng serve",            // Angular
        "react-scripts start", // CRA
        "expo start",          // React Native web
        "remix dev",
        "gunicorn",
        "uvicorn",
        "hypercorn",
        "daphne",
        "flask run",
        "django runserver",
        "manage.py runserver",
        "rails server",
        "rails s ",
        "sinatra",
        "node server",
        "node app",
        "node index",
        "node main",
        "ts-node server",
        "tsx server",
        "deno serve",
        "bun --hot",
        "bun run dev",
        "bun run start",
        "cargo run",
        "cargo watch",
        "go run",
        "dotnet run",
        "dotnet watch",
        "php -S",
        "ruby -run",
        "live-server",
        "http-server",
        "browser-sync",
        // Make/script wrapper noti
        "npm run dev",
        "npm run start",
        "npm run serve",
        "pnpm dev",
        "pnpm start",
        "pnpm serve",
        "yarn dev",
        "yarn start",
        "yarn serve",
    ];
    WEB_TOKENS.iter().any(|t| lc.contains(t))
}

/// Cerca nella combinazione stdout+stderr un pattern di porta TCP in ascolto.
/// Riconosce output di Next.js, Vite, Express, Flask, Django, ecc.
/// Ritorna la prima porta trovata (4-5 cifre, range 1024-65535).
fn detect_port_from_output(stdout: &str, stderr: &str) -> Option<i32> {
    let combined = format!("{}\n{}", stdout, stderr);
    // Pattern frequenti: "localhost:3002", "0.0.0.0:3000", "port 5173",
    // "Local: http://localhost:3002", "listening on :8080"
    let re = regex::Regex::new(
        r"(?i)(?:localhost|127\.0\.0\.1|0\.0\.0\.0|::)[:\s]+(\d{4,5})|(?:port|porta)\s+(\d{4,5})|Local:\s+https?://[^:]+:(\d{4,5})"
    ).ok()?;
    for cap in re.captures_iter(&combined) {
        let port_str = cap.get(1).or(cap.get(2)).or(cap.get(3))?;
        if let Ok(p) = port_str.as_str().parse::<i32>() {
            if (1024..=65535).contains(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// Avvia un servizio/processo long-running direttamente sul server.
/// L'output viene catturato nel DB e mostrato nel pannello Output dell'IDE.
pub(super) async fn tool_run_service(ctx: &AgentToolContext, input: &Value, kind: &str) -> String {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return "[Errore: parametro 'command' mancante]".to_string(),
    };
    if command.trim().is_empty() {
        return "[Errore: comando vuoto]".to_string();
    }

    let label = input
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("Service")
        .to_string();

    // Resolve working directory
    let work_dir = if let Some(sub) = input.get("working_dir").and_then(Value::as_str) {
        if !sub.is_empty() {
            match resolve_relative_path(&ctx.root_path, sub) {
                Ok(p) => p,
                Err(e) => {
                    return format!(
                        "[Errore percorso: {}]",
                        e.1["error"].as_str().unwrap_or("path error")
                    )
                }
            }
        } else {
            ctx.root_path.clone()
        }
    } else {
        ctx.root_path.clone()
    };

    // ── Anti-duplicato systemd: se esiste un servizio systemd attivo del
    //    progetto (taskboard-backend-dev.service, ecc.) con scopo simile,
    //    REFUSE invece di lanciare un duplicato. L'agente AI deve usare
    //    quello esistente, non bruciare risorse.
    //    Tipologia: deriva dal command (vite/express/etc) o dal label.
    let label_lower_top = label.to_lowercase();
    let command_lower_top = command.to_lowercase();
    let kind_hint = if command_lower_top.contains("vite")
        || command_lower_top.contains("svelte")
        || label_lower_top.contains("frontend")
        || label_lower_top.contains("vite")
        || label_lower_top.contains("react")
        || label_lower_top.contains("svelte")
        || label_lower_top.contains("nuxt")
        || label_lower_top.contains("astro")
    {
        Some("frontend")
    } else if command_lower_top.contains("express")
        || command_lower_top.contains("fastify")
        || command_lower_top.contains("uvicorn")
        || command_lower_top.contains("gunicorn")
        || command_lower_top.contains("rails")
        || command_lower_top.contains("django")
        || command_lower_top.contains(" run ") && command_lower_top.contains("backend")
        || label_lower_top.contains("backend")
        || label_lower_top.contains("api")
    {
        Some("backend")
    } else {
        None
    };
    if let Some(kind) = kind_hint {
        // ── A. Cerca tra le allocazioni del progetto label compatibili ──
        let rows = sqlx::query_as::<_, (i32, String)>(
            "SELECT port, label FROM nexus_port_allocations WHERE project_id = $1",
        )
        .bind(ctx.project_id)
        .fetch_all(&*ctx.db)
        .await
        .unwrap_or_default();
        for (port, alloc_label) in &rows {
            let al = alloc_label.to_lowercase();
            let matches = match kind {
                "frontend" => {
                    al.contains("frontend")
                        || al.contains("vite")
                        || al.contains("react")
                        || al.contains("svelte")
                        || al.contains("ui")
                        || al.contains("nuxt")
                        || al.contains("astro")
                        || al.contains("web")
                        || al.contains("client")
                        || al.contains("dev server")
                }
                "backend" => {
                    al.contains("backend")
                        || al.contains("api")
                        || (al.contains("server")
                            && !al.contains("frontend")
                            && !al.contains("dev"))
                }
                _ => false,
            };
            if matches {
                return format!(
                    "[Errore: servizio '{}' di tipo {} gia' attivo sulla porta {}. \
                     Riusalo invece di crearne uno nuovo (puoi accedere a http://localhost:{}). \
                     Se vuoi davvero riavviarlo usa `service_restart` con label='{}'.]",
                    alloc_label, kind, port, port, alloc_label
                );
            }
        }

        // ── B. Cerca tra i servizi systemd persistenti del progetto ──
        // I servizi systemd hanno label come "backend-dev"/"frontend-dev"
        // ma l'allocazione di porta puo' avere label generica ("Service").
        // Query diretta systemctl: list-units --user --state=active per
        // unit con prefisso slug del progetto.
        let project_slug_q =
            sqlx::query_scalar::<_, String>("SELECT slug FROM projects WHERE id = $1")
                .bind(ctx.project_id)
                .fetch_optional(&*ctx.db)
                .await
                .ok()
                .flatten();
        if let Some(slug) = project_slug_q {
            let slug_norm = slug.to_lowercase().replace([' ', '_'], "-");
            let systemd_output = tokio::process::Command::new("systemctl")
                .args([
                    "--user",
                    "list-units",
                    "--type=service",
                    "--state=active",
                    "--no-legend",
                    "--no-pager",
                ])
                .output()
                .await;
            if let Ok(out) = systemd_output {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let prefix = format!("{}-", slug_norm);
                for line in stdout.lines() {
                    let unit = line
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches('●')
                        .trim();
                    if !unit.starts_with(&prefix) || !unit.ends_with(".service") {
                        continue;
                    }
                    let short = unit
                        .strip_prefix(&prefix)
                        .unwrap_or(unit)
                        .strip_suffix(".service")
                        .unwrap_or(unit)
                        .to_lowercase();
                    let matches = match kind {
                        "frontend" => {
                            short.contains("frontend")
                                || short.contains("vite")
                                || short.contains("ui")
                                || short.contains("web")
                                || short.contains("client")
                        }
                        "backend" => {
                            short.contains("backend")
                                || short.contains("api")
                                || short.contains("server")
                        }
                        _ => false,
                    };
                    if matches {
                        return format!(
                            "[Errore: servizio systemd '{}' di tipo {} gia' attivo per questo progetto. \
                             Usalo invece di lanciarne uno nuovo. Per riavviarlo usa il pulsante 'restart' \
                             nel pannello Run & Debug, oppure systemctl --user restart {}. \
                             Se hai bisogno di vedere lo stato del servizio, prima usa list_active_services.]",
                            unit, kind, unit
                        );
                    }
                }
            }
        }
    }

    // ── Deduplicazione servizi: kill processi simili + cleanup orfani ────────
    // L'agente AI spesso usa label leggermente diverse per lo stesso servizio
    // ("Backend Taskboard", "Backend API", "Taskboard Backend"). Per evitare
    // duplicati, confrontiamo con similarita' normalizzata oltre che esatta.
    if let Ok(existing) = crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        let label_lower = label.to_lowercase();
        let label_words: std::collections::HashSet<&str> = label_lower.split_whitespace().collect();

        for proc in existing
            .iter()
            .filter(|p| p.status == "running" || p.status == "starting")
        {
            let proc_lower = proc.label.to_lowercase();
            let proc_words: std::collections::HashSet<&str> =
                proc_lower.split_whitespace().collect();

            // Match esatto o similarita': almeno una parola significativa in comune
            // (escludiamo parole generiche come "service", "server", "run")
            let dominated = proc.label == label || {
                const GENERIC: &[&str] = &["service", "server", "run", "dev", "start"];
                let meaningful_common = label_words
                    .intersection(&proc_words)
                    .filter(|w| !GENERIC.contains(w) && w.len() > 2)
                    .count();
                meaningful_common > 0
            };

            if dominated {
                tracing::info!(
                    old_label = %proc.label,
                    new_label = %label,
                    proc_id = %proc.id,
                    "run_service: kill processo simile prima di riavvio"
                );
                let _ = crate::agent_processes::stop_process(&ctx.db, proc.id).await;
            }
        }

        // Cleanup porte allocate per processi morti di questo progetto
        cleanup_dead_process_ports(&ctx.db, ctx.project_id, &existing).await;
    }

    // ── Quota container (PR hardening) ────────────────────────────────────
    // Solo per kind="service": i tool agente short-lived non contano contro
    // la quota container del progetto.
    if kind == "service" {
        if let Err(reason) =
            crate::security::quotas::check_can_start_container(&ctx.db, ctx.project_id).await
        {
            crate::security::record_audit(
                crate::security::AuditEntry::blocked(
                    ctx.project_id,
                    "container_create",
                    "container",
                )
                .with_resource(label.clone())
                .with_details(serde_json::json!({"reason": reason, "command": command})),
            );
            return format!("[Quota raggiunta: {}]", reason);
        }
        // Quota RAM pre-avvio (governance container/memory_quota): blocca se i
        // servizi gia' attivi del progetto saturano max_memory_mb.
        if crate::security::resource_governance::policy(&ctx.db, "container", "memory_quota")
            .await
            .enabled
        {
            if let Err(reason) =
                crate::security::quotas::check_can_use_memory(&ctx.db, ctx.project_id).await
            {
                crate::security::record_audit(
                    crate::security::AuditEntry::blocked(
                        ctx.project_id,
                        "container_quota_blocked",
                        "container",
                    )
                    .with_resource(label.clone())
                    .with_details(serde_json::json!({"reason": reason})),
                );
                return format!("[Quota memoria raggiunta: {}]", reason);
            }
        }
    }

    // ── Strato 1 hardening: auto-inject PORT per servizi web ────────────────
    // Se il comando avvia un server (next dev, vite, gunicorn, ecc.) Nexus
    // alloca automaticamente una porta nel bucket del progetto e la inietta
    // come PORT env. Cosi' il servizio non bindera' sulla porta hardcoded
    // (es. next dev → 3000 → conflitto con web-ide Nexus).
    let mut env_overrides: Option<HashMap<String, String>> = None;
    if looks_like_web_service(&command) {
        match crate::project_workspace::find_or_allocate_port(
            &ctx.db,
            &ctx.port_registry,
            ctx.project_id,
            &label,
        )
        .await
        {
            Ok(alloc) => {
                let mut env = HashMap::new();
                env.insert("PORT".to_string(), alloc.port.to_string());
                env.insert("HOST".to_string(), "0.0.0.0".to_string());
                env_overrides = Some(env);
                tracing::info!(
                    port = alloc.port,
                    label = %label,
                    mode = alloc.mode,
                    "run_service: PORT auto-allocato per servizio web"
                );
            }
            Err(e) => {
                return format!("[Errore allocazione porta per servizio '{}': {}]", label, e);
            }
        }
    }

    match crate::agent_processes::spawn_agent_process(
        &ctx.db,
        ctx.project_id,
        ctx.session_id,
        &label,
        &command,
        &work_dir.to_string_lossy(),
        Some(ctx.root_path.clone()),
        env_overrides,
        crate::sandbox::sandbox_enabled(),
        kind,
        None,
    )
    .await
    {
        Ok(process_id) => {
            // Wait a few seconds for initial output
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Read initial output
            match crate::agent_processes::read_process_output(&ctx.db, process_id, 4000).await {
                Ok(info) => {
                    // Auto-detect porta dall'output del servizio e registra
                    // in nexus_port_allocations per il pannello Porte.
                    let detected_port = detect_port_from_output(&info.stdout, &info.stderr);
                    if let Some(port) = detected_port {
                        let _ = sqlx::query(
                            "INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode) \
                             VALUES ($1, $2, $3, 'auto') ON CONFLICT (port) DO UPDATE SET \
                             project_id = $1, label = $3, updated_at = NOW()"
                        )
                        .bind(ctx.project_id)
                        .bind(port)
                        .bind(&label)
                        .execute(&*ctx.db)
                        .await;
                        nexus_events::dispatcher::emit(
                            &ctx.project_channels,
                            ctx.project_id,
                            nexus_events::event::ProjectEvent::PortAllocated {
                                port,
                                label: label.clone(),
                                pid: info.pid,
                            },
                        );
                    }
                    // Dispatcher: notifica avvio servizio → pannello Servizi aggiorna LED
                    nexus_events::dispatcher::emit(
                        &ctx.project_channels,
                        ctx.project_id,
                        nexus_events::event::ProjectEvent::ServiceStarted {
                            name: label.clone(),
                            port: detected_port,
                            pid: info.pid,
                        },
                    );
                    let mut msg = format!(
                        "Servizio '{}' avviato (process_id: {}, pid: {}, status: {})\n",
                        label,
                        process_id,
                        info.pid
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "?".into()),
                        info.status,
                    );
                    if !info.stdout.is_empty() {
                        msg.push_str(&format!("\nSTDOUT:\n{}", info.stdout));
                    }
                    if !info.stderr.is_empty() {
                        msg.push_str(&format!("\nSTDERR:\n{}", info.stderr));
                    }
                    if info.stdout.is_empty() && info.stderr.is_empty() {
                        msg.push_str("\n(Nessun output ancora. Usa read_service_output per controllare dopo qualche secondo.)");
                    }
                    msg
                }
                Err(e) => format!(
                    "Servizio '{}' avviato (process_id: {}), ma errore lettura output: {}",
                    label, process_id, e
                ),
            }
        }
        Err(e) => format!("[Errore avvio servizio '{}': {}]", label, e),
    }
}

/// Rilascia porte allocate (dynamic) il cui processo e' morto.
/// Controlla `agent_processes` per processi non-running di questo progetto
/// e rimuove le porte allocate che non hanno piu' un processo attivo.
async fn cleanup_dead_process_ports(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    processes: &[crate::agent_processes::ProcessSummary],
) {
    // Raccogli le label dei processi ancora attivi
    let active_labels: std::collections::HashSet<String> = processes
        .iter()
        .filter(|p| p.status == "running" || p.status == "starting")
        .map(|p| p.label.clone())
        .collect();

    // Prendi le porte allocate dinamicamente per questo progetto
    let rows = sqlx::query_as::<_, (i32, String)>(
        "SELECT port, label FROM nexus_port_allocations \
         WHERE project_id = $1 AND allocation_mode = 'dynamic'",
    )
    .bind(project_id)
    .fetch_all(db)
    .await;

    if let Ok(allocations) = rows {
        for (port, alloc_label) in allocations {
            // Se nessun processo attivo corrisponde a questa allocazione, rilasciala
            if !active_labels.contains(&alloc_label) {
                // Verifica anche che la porta non sia effettivamente in uso (bind test)
                let port_in_use = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
                    .await
                    .is_err();
                if !port_in_use {
                    let _ = sqlx::query(
                        "DELETE FROM nexus_port_allocations WHERE project_id = $1 AND port = $2",
                    )
                    .bind(project_id)
                    .bind(port)
                    .execute(db)
                    .await;
                    tracing::info!(
                        port = port,
                        label = %alloc_label,
                        "cleanup: porta dinamica rilasciata (processo morto)"
                    );
                }
            }
        }
    }
}

/// Legge l'output di un servizio avviato con run_service
pub(super) async fn tool_read_service_output(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = input
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("");

    if process_id_str.is_empty() {
        // Se non specificato, leggi l'ultimo processo del progetto
        let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
            Ok(r) => r,
            Err(e) => return format!("[Errore: {}]", e),
        };
        if rows.is_empty() {
            return "Nessun servizio avviato per questo progetto.".to_string();
        }
        let last = &rows[0];
        match crate::agent_processes::read_process_output(&ctx.db, last.id, 4000).await {
            Ok(info) => format_process_output(&info),
            Err(e) => format!("[Errore lettura output: {}]", e),
        }
    } else {
        let process_id = match Uuid::parse_str(process_id_str) {
            Ok(id) => id,
            Err(_) => return "[Errore: process_id non valido]".to_string(),
        };
        match crate::agent_processes::read_process_output(&ctx.db, process_id, 4000).await {
            Ok(info) => format_process_output(&info),
            Err(e) => format!("[Errore lettura output: {}]", e),
        }
    }
}

/// Ferma un servizio avviato con run_service
pub(super) async fn tool_stop_service(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = match input.get("process_id").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'process_id' mancante]".to_string(),
    };
    let process_id = match Uuid::parse_str(process_id_str) {
        Ok(id) => id,
        Err(_) => return "[Errore: process_id non valido]".to_string(),
    };
    match crate::agent_processes::stop_process(&ctx.db, process_id).await {
        Ok(msg) => {
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::ServiceStopped {
                    name: format!("process:{}", process_id),
                },
            );
            msg
        }
        Err(e) => format!("[Errore stop servizio: {}]", e),
    }
}

pub(super) async fn tool_build_project_image(ctx: &AgentToolContext) -> String {
    use crate::sandbox::build_project_service_image;
    match build_project_service_image(ctx.project_id, &ctx.root_path, &ctx.root_path).await {
        Ok(tag) => format!("Immagine Docker progetto buildata con successo: {}. I servizi avviati con run_service useranno questa immagine.", tag),
        Err(e) => format!("[Errore build immagine: {}]", e),
    }
}

/// Riavvia un servizio: ferma tutti i processi con la stessa label,
/// poi li riesegue con lo stesso comando. Attende output iniziale.
pub(super) async fn tool_service_restart(ctx: &AgentToolContext, input: &Value) -> String {
    let label = match input.get("label").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'label' obbligatorio]".to_string(),
    };

    // Cerca il processo esistente con questa label per recuperare il comando
    let existing = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return format!("[Errore lista processi: {}]", e),
    };

    let matching: Vec<_> = existing.iter().filter(|p| p.label == label).collect();
    if matching.is_empty() {
        return format!(
            "[Errore: nessun servizio trovato con label '{}'. Usa run_service per avviarlo.]",
            label
        );
    }

    // Recupera il comando originale dal processo piu' recente con questa label
    let original_command = matching[0].command.clone();

    // Leggi working_dir dal record completo via DB
    let work_dir_row = sqlx::query("SELECT working_dir FROM agent_processes WHERE id = $1")
        .bind(matching[0].id)
        .fetch_optional(&*ctx.db)
        .await;

    let work_dir = match work_dir_row {
        Ok(Some(row)) => {
            let wd: String = row.try_get("working_dir").unwrap_or_default();
            if wd.is_empty() {
                ctx.root_path.to_string_lossy().to_string()
            } else {
                wd
            }
        }
        _ => ctx.root_path.to_string_lossy().to_string(),
    };

    // Ferma tutti i processi attivi con questa label
    for proc in matching
        .iter()
        .filter(|p| p.status == "running" || p.status == "starting")
    {
        let _ = crate::agent_processes::stop_process(&ctx.db, proc.id).await;
    }

    // Breve pausa per garantire che le porte siano liberate
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Riavvia con lo stesso comando
    let restart_input = serde_json::json!({
        "command": original_command,
        "label": label,
        "working_dir": work_dir,
    });

    let result = tool_run_service(ctx, &restart_input, "service").await;
    format!("Servizio '{}' riavviato.\n{}", label, result)
}

/// Legge le ultime N righe di output di un servizio, con opzione di attesa
/// per catturare output aggiuntivo (simula follow per X secondi).
pub(super) async fn tool_tail_service_logs(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = input
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let max_chars = input
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(8000) as usize;
    let follow_secs = input
        .get("follow_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(60);

    // Risolvi process_id: specifico oppure ultimo del progetto
    let process_id = if process_id_str.is_empty() {
        let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
            Ok(r) => r,
            Err(e) => return format!("[Errore: {}]", e),
        };
        if rows.is_empty() {
            return "Nessun servizio avviato per questo progetto.".to_string();
        }
        rows[0].id
    } else {
        match Uuid::parse_str(process_id_str) {
            Ok(id) => id,
            Err(_) => return "[Errore: process_id non valido]".to_string(),
        }
    };

    if follow_secs == 0 {
        return match crate::agent_processes::read_process_output(&ctx.db, process_id, max_chars)
            .await
        {
            Ok(info) => format_process_output(&info),
            Err(e) => format!("[Errore lettura output: {}]", e),
        };
    }

    // Modalita' follow: polleggia ogni 2 secondi
    let mut combined_output = String::new();
    let mut last_stdout_len: usize = 0;
    let mut last_stderr_len: usize = 0;

    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < follow_secs {
        match crate::agent_processes::read_process_output(&ctx.db, process_id, max_chars).await {
            Ok(info) => {
                if info.stdout.len() > last_stdout_len {
                    combined_output.push_str(&info.stdout[last_stdout_len..]);
                    last_stdout_len = info.stdout.len();
                }
                if info.stderr.len() > last_stderr_len {
                    if !combined_output.is_empty() && !combined_output.ends_with('\n') {
                        combined_output.push('\n');
                    }
                    combined_output
                        .push_str(&format!("[STDERR] {}", &info.stderr[last_stderr_len..]));
                    last_stderr_len = info.stderr.len();
                }
                if info.status != "running" && info.status != "starting" {
                    combined_output.push_str(&format!(
                        "\n--- Processo terminato (status: {}, exit_code: {}) ---",
                        info.status,
                        info.exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".into())
                    ));
                    break;
                }
            }
            Err(e) => {
                combined_output.push_str(&format!("\n[Errore lettura: {}]", e));
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    if combined_output.is_empty() {
        "(Nessun output durante il periodo di follow)".to_string()
    } else {
        combined_output
    }
}

/// Lista i servizi/processi attivi per il progetto corrente.
pub(super) async fn tool_list_active_services(ctx: &AgentToolContext, _input: &Value) -> String {
    let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return format!("[Errore: {}]", e),
    };

    if rows.is_empty() {
        return "Nessun servizio registrato per questo progetto.".to_string();
    }

    let mut output = String::new();
    let mut running_count = 0;
    let mut stopped_count = 0;

    for proc in &rows {
        let status_icon = match proc.status.as_str() {
            "running" | "starting" => {
                running_count += 1;
                "[ATTIVO]"
            }
            "stopped" | "exited" | "finished" => {
                stopped_count += 1;
                "[FERMO]"
            }
            _ => {
                stopped_count += 1;
                "[?]"
            }
        };

        output.push_str(&format!(
            "{} {} (id: {}, pid: {}, status: {}",
            status_icon,
            proc.label,
            proc.id,
            proc.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            proc.status,
        ));

        if let Some(code) = proc.exit_code {
            output.push_str(&format!(", exit: {}", code));
        }

        output.push_str(&format!(", avviato: {})\n", proc.created_at));
        output.push_str(&format!("  cmd: {}\n\n", proc.command));
    }

    format!(
        "Servizi progetto: {} attivi, {} fermi (ultimi 20)\n\n{}",
        running_count, stopped_count, output
    )
}
