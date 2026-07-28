use super::*;

/// Rileva se `systemctl --user` non e' riuscito a contattare il manager utente
/// (bus D-Bus non raggiungibile). In WSL e nei container il manager
/// `user@<uid>.service` puo' restare inactive: in quel caso `systemctl --user`
/// esce con codice != 0 e stderr contiene "Failed to connect to bus" /
/// "Connection refused". Va distinto da "zero servizi configurati": con il bus
/// giu' stdout e' vuoto e il chiamante, senza questo check, mostrerebbe il
/// messaggio fuorviante "Nessun servizio trovato" anche quando i file .service
/// esistono. Vedi ADR 0022.
pub(super) fn user_manager_unavailable(output: &std::process::Output) -> bool {
    if output.status.success() {
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    stderr.contains("failed to connect to bus")
        || stderr.contains("connection refused")
        || stderr.contains("failed to get d-bus connection")
        || stderr.contains("refusing to operate")
}

/// Suggerimento operativo mostrato quando il manager systemd utente e' giu'.
/// Espone `USER_MANAGER_HINT` per il nuovo `service_manager` senza duplicarne
/// il testo (regola L).
pub(super) fn user_manager_hint() -> &'static str {
    USER_MANAGER_HINT
}

/// Suggerimento operativo mostrato in UI quando il manager utente e' giu'.
/// Niente uid hardcoded: `$(id -u)` viene risolto dalla shell dell'utente.
const USER_MANAGER_HINT: &str = "Il manager systemd utente non e' attivo \
(tipico in WSL). Nexus gestisce comunque i servizi in modalita' detached: \
sono elencati e avviabili qui sotto. Per usare systemd: `sudo systemctl start \
user@$(id -u)` oppure `wsl --shutdown`. I file .service restano in \
~/.config/systemd/user/.";

/// Estrae la riga `ExecStart=` da un contenuto di unit file systemd.
pub(super) fn unit_exec_start(content: &str) -> String {
    content
        .lines()
        .find_map(|l| l.trim().strip_prefix("ExecStart="))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Estrae `WorkingDirectory=` da un unit file.
pub(super) fn unit_working_dir(content: &str) -> String {
    content
        .lines()
        .find_map(|l| l.trim().strip_prefix("WorkingDirectory="))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Estrae le coppie `Environment=KEY=VAL` da un unit file (una per riga, forma
/// usata dal wizard install).
pub(super) fn unit_env_map(content: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for l in content.lines() {
        if let Some(rest) = l.trim().strip_prefix("Environment=") {
            let rest = rest.trim().trim_matches('"');
            if let Some((k, v)) = rest.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
            }
        }
    }
    m
}

/// Vero se esiste un processo che esegue `exec_start` (avviato dal fallback
/// detached, vedi `spawn_detached_service`). Usa lo stesso criterio di match
/// (pgrep -f sull'ExecStart) di pkill usato per fermarlo.
pub(super) async fn detached_process_running(exec_start: &str) -> bool {
    if exec_start.trim().is_empty() {
        return false;
    }
    tokio::process::Command::new("pgrep")
        .args(["-f", exec_start])
        .output()
        .await
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Vero se l'ExecStart del servizio e' un comando `docker compose ... up`: un
/// one-shot che avvia i container in background e POI ESCE. Per questi servizi
/// lo stato del processo wrapper (pgrep sull'ExecStart) e' sempre "non in
/// esecuzione" anche quando i container sono Up -> il pannello mostrerebbe
/// "dead" pur essendo lo stack attivo. Vanno valutati via `docker compose ps`.
pub(super) fn is_docker_compose_service(exec_start: &str) -> bool {
    let e = exec_start.to_lowercase();
    (e.contains("docker compose") || e.contains("docker-compose")) && e.contains(" up")
}

/// Stato REALE di un servizio docker-compose: conta i container in esecuzione
/// via `docker compose ps -q --status running` nella working directory del
/// servizio (dove risiede il compose file; il project name di default e' il nome
/// della dir, quindi intercetta gli stessi container avviati dall'ExecStart anche
/// con override `-f`). Ritorna true se almeno un container e' running. Cosi' il
/// pannello riflette lo stato dei container, non quello del wrapper one-shot
/// (`up -d` esce subito). Se docker non e' raggiungibile, ritorna false (come il
/// comportamento pre-fix: nessun falso positivo).
pub(super) async fn docker_compose_running(working_dir: &str) -> bool {
    if working_dir.trim().is_empty() {
        return false;
    }
    tokio::process::Command::new("docker")
        .args(["compose", "ps", "-q", "--status", "running"])
        .current_dir(working_dir)
        .output()
        .await
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .is_some()
        })
        .unwrap_or(false)
}

/// Nomi dei service del docker-compose del progetto che hanno container in
/// esecuzione, via `docker compose ps --services --status running` nella root del
/// progetto. Usato per ALLINEARE i servizi systemd per-componente (es. un
/// `backend.service` che lancerebbe `npm run dev` sull'host) allo stato reale del
/// container omonimo: se il progetto e' containerizzato, il backend/frontend sono
/// forniti dai container di docker-compose, non dal processo host. Senza questo
/// allineamento il pannello mostra "backend dead / frontend dead" mentre il
/// servizio docker-compose e' "running" — incongruenza (i servizi rappresentano
/// gli stessi container). Set vuoto se docker non e' raggiungibile o non c'e'
/// compose.
pub(super) async fn docker_compose_active_services(
    project_root: &std::path::Path,
) -> std::collections::HashSet<String> {
    let out = tokio::process::Command::new("docker")
        .args(["compose", "ps", "--services", "--status", "running"])
        .current_dir(project_root)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => std::collections::HashSet::new(),
    }
}

/// Vero se `fname` e' un file unit appartenente al progetto `slug`
/// (`{slug}-*.service`). Criterio UNICO (regola L) di appartenenza di una unit a
/// un progetto: usato sia dall'enumerazione dei servizi gestiti
/// (`list_services_fallback`) sia dal marking dei candidati gia' installati nel
/// wizard (`mark_existing_services`), cosi' le due viste non divergono mai.
pub(super) fn is_project_unit_file(fname: &str, slug: &str) -> bool {
    fname.starts_with(&format!("{slug}-")) && fname.ends_with(".service")
}

/// Nomi dei file unit del progetto presenti su disco in
/// `~/.config/systemd/user/{slug}-*.service`. PUNTO UNICO (regola L) per "quali
/// unit del progetto ESISTONO", indipendente dal bus systemd --user: in
/// WSL/detached `systemctl --user list-unit-files` fallisce, ma i file unit
/// restano su disco ed e' QUESTA la fonte che il pannello usa gia' (via
/// `list_services_fallback`) per elencarli come gestiti. Il wizard la usa per non
/// ri-offrire l'installazione di servizi gia' configurati. Set vuoto se la dir
/// non esiste.
pub(super) async fn project_unit_files_on_disk(slug: &str) -> std::collections::HashSet<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
    let dir = format!("{home}/.config/systemd/user");
    let mut units = std::collections::HashSet::new();
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(r) => r,
        Err(_) => return units,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let fname = entry.file_name().to_string_lossy().to_string();
        if is_project_unit_file(&fname, slug) {
            units.insert(fname);
        }
    }
    units
}

/// PID radice dei servizi DETACHED del progetto (avviati da
/// `spawn_detached_service`, senza MainPID systemd) mappati al loro `short`.
/// PUNTO UNICO (regola L): trova il processo wrapper di ogni unit via `pgrep -f`
/// sull'ExecStart -- lo stesso criterio di `detached_process_running` e del pkill
/// di stop -- riusando i file unit su disco (`project_unit_files_on_disk`).
/// `detect_project_ports` usa questi PID come seed della propagazione
/// pid->service: il processo che apre la porta e' un DISCENDENTE del wrapper
/// (pnpm -> nodemon/vite -> node), raggiunto dalla BFS gia' presente. Senza
/// questo seed, in WSL/detached tutte le porte risultano `service=null` e la UI
/// non mostra il link al servizio.
pub(super) async fn detached_service_root_pids(slug: &str) -> Vec<(u32, String)> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
    let dir = format!("{home}/.config/systemd/user");
    let mut out = Vec::new();
    for fname in project_unit_files_on_disk(slug).await {
        let short = fname
            .strip_prefix(&format!("{slug}-"))
            .unwrap_or(&fname)
            .strip_suffix(".service")
            .unwrap_or(&fname)
            .to_string();
        let content = tokio::fs::read_to_string(format!("{dir}/{fname}"))
            .await
            .unwrap_or_default();
        let exec_start = unit_exec_start(&content);
        if exec_start.trim().is_empty() {
            continue;
        }
        if let Ok(o) = tokio::process::Command::new("pgrep")
            .args(["-f", &exec_start])
            .output()
            .await
        {
            if o.status.success() {
                for line in String::from_utf8_lossy(&o.stdout).lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        out.push((pid, short.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Elenca i servizi del progetto SENZA systemd --user (manager `user@<uid>`
/// dead, tipico in WSL). Legge i file unit in `~/.config/systemd/user/{slug}-*.
/// service` e ne deduce lo stato dal processo detached (pgrep sull'ExecStart).
/// Cosi' il pannello funziona anche quando il bus systemd utente e' giu', senza
/// richiedere sudo (fix definitivo, regola H: niente dipendenza dal manager
/// fragile di WSL).
///
/// `project_root` serve per allineare i servizi per-componente ai container
/// docker-compose omonimi (vedi `docker_compose_active_services`).
pub(super) async fn list_services_fallback(
    slug: &str,
    project_root: &std::path::Path,
) -> Vec<serde_json::Value> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
    let dir = format!("{home}/.config/systemd/user");
    let prefix = format!("{slug}-");
    let mut services: Vec<serde_json::Value> = Vec::new();
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(r) => r,
        Err(_) => return services,
    };
    // Una sola interrogazione a docker per tutto il batch di servizi.
    let compose_services = docker_compose_active_services(project_root).await;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !is_project_unit_file(&fname, slug) {
            continue;
        }
        let short = fname
            .strip_prefix(&prefix)
            .unwrap_or(&fname)
            .strip_suffix(".service")
            .unwrap_or(&fname)
            .to_string();
        let content = tokio::fs::read_to_string(entry.path())
            .await
            .unwrap_or_default();
        let exec_start = unit_exec_start(&content);
        // Determinazione dello stato con 3 casi, dal piu' specifico al generico:
        // 1) il servizio E' il wrapper docker-compose -> stato dai container;
        // 2) il servizio CORRISPONDE a un service del compose attivo (es.
        //    backend/frontend) -> e' gestito dal container omonimo, stato = running
        //    (managed_by="docker-compose"): evita "dead" mentre il container gira;
        // 3) altrimenti servizio host detached -> pgrep sull'ExecStart.
        let (running, managed_by) = if is_docker_compose_service(&exec_start) {
            (
                docker_compose_running(&unit_working_dir(&content)).await,
                "docker-compose",
            )
        } else if compose_services.contains(&short) {
            (true, "docker-compose")
        } else {
            (detached_process_running(&exec_start).await, "detached")
        };
        services.push(json!({
            "unit":       fname,
            "short":      short,
            "state":      if running { "active" } else { "inactive" },
            "sub":        if running { "running" } else { "dead" },
            "managed_by": managed_by,
        }));
    }
    services.sort_by(|a, b| a["short"].as_str().cmp(&b["short"].as_str()));
    services
}

/// PUNTO UNICO (regola L) della derivazione dello "slug di servizio" di un
/// progetto a partire dal suo NOME. E' la formula usata da TUTTI i call site
/// Windows (pannello Servizi, wizard install, allocazione porte) per costruire
/// il nome dell'unit `{slug}-{label}.service`. NON coincide con `projects.slug`
/// (slugify + suffisso di unicita' `-N`): usare `projects.slug` qui creerebbe
/// unit divergenti (diagnosi orfane, readiness saltata). Chi costruisce un unit
/// deve derivare lo slug SOLO da qui.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn project_service_slug(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

/// PUNTO UNICO (regola L) della costruzione del nome unit di un servizio di
/// progetto: `{slug}-{label}.service`. Usato dal pannello (`list_services_windows`)
/// e dall'observer (`collect_units`) cosi' i due lati producono lo STESSO unit,
/// che deve anche combaciare con `nexus_port_allocations.service_unit`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn service_unit_name(slug: &str, label: &str) -> String {
    format!("{slug}-{label}.service")
}

/// Nome unit di un servizio per chi ha in mano solo il `project_id` (i tool
/// agente): legge il NOME del progetto e passa dai due punti unici
/// `project_service_slug` + `service_unit_name`, cosi' l'unit a cui viene legata
/// l'allocazione porta e' lo STESSO che il pannello ricostruisce dalla label del
/// processo. `None` se il progetto non e' leggibile: senza nome non esiste unit
/// da dichiarare, e inventarne uno legherebbe la porta a un'identita' fantasma.
pub(crate) async fn project_service_unit(
    db: &sqlx::PgPool,
    project_id: Uuid,
    label: &str,
) -> Option<String> {
    let name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()?;
    Some(service_unit_name(&project_service_slug(&name), label))
}

/// Voci del pannello Servizi Windows a partire dalle righe storiche di
/// agent_processes (label, status, created_at — ordinate per label,
/// created_at DESC). Funzione pura testabile (punto unico, regola L):
/// - per ogni label conta la riga PIU' RECENTE (stato corrente del servizio);
/// - una label running/starting e' sempre visibile (va potuta fermare);
/// - una label MORTA e' nascosta se generica ("Service": tentativo storico,
///   non un servizio) o se superseded da una label simile con storia piu'
///   recente ("frontend-dev" morta dopo che "frontend" l'ha sostituita).
/// Ritorna (label, running) ordinate per label.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn visible_windows_services(
    rows: &[(String, String, chrono::DateTime<chrono::Utc>)],
) -> Vec<(String, bool)> {
    // Riga piu' recente per label: rows e' gia' ordinata (label, created_at DESC),
    // quindi la prima occorrenza di ogni label e' la sua riga piu' recente.
    let mut seen = std::collections::HashSet::new();
    let mut latest: Vec<(&str, bool, chrono::DateTime<chrono::Utc>)> = Vec::new();
    for (label, status, created_at) in rows {
        if !seen.insert(label.as_str()) {
            continue;
        }
        let running = matches!(status.as_str(), "running" | "starting");
        latest.push((label.as_str(), running, *created_at));
    }
    let mut visible: Vec<(String, bool)> = latest
        .iter()
        .filter(|(label, running, created_at)| {
            if *running {
                return true;
            }
            if crate::agent_processes::is_generic_service_label(label) {
                return false;
            }
            !latest.iter().any(|(other, _, other_created)| {
                other != label
                    && other_created > created_at
                    && crate::agent_processes::similar_service_labels(label, other)
            })
        })
        .map(|(label, running, _)| ((*label).to_string(), *running))
        .collect();
    visible.sort_by(|a, b| a.0.cmp(&b.0));
    visible
}

/// Riconciliazione stato servizi (pura, testabile). Regola M: "running" implica un
/// processo VIVO E DI IDENTITA' CONFERMATA; la stringa `status` in DB puo' restare
/// stale (un processo muore senza aggiornare la riga: crash, kill esterno, riavvio
/// host) e su Windows il PID puo' essere stato RICICLATO dal SO su un processo
/// estraneo. Una riga running/starting il cui pid non e' piu' vivo o e' stato
/// riciclato viene corretta a "stopped". Ritorna le righe corrette (per
/// `visible_windows_services`) e i pid morti da persistere come stopped. Punto
/// unico della regola (regola L).
///
/// Il predicato `alive_confirmed(pid, started_at)` incapsula liveness + identita':
/// il caller Windows inietta `process_alive && pid_identity_confirmed` (lo STESSO
/// criterio di `windows_pid_state` nell'observer), cosi' il pannello Servizi e il
/// pannello Problemi non divergono piu' sotto riciclo PID (prima il pannello
/// Servizi mostrava 'running' un PID riciclato che l'observer marcava 'failed').
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn reconcile_dead_service_rows(
    rows: Vec<(
        String,
        String,
        Option<i32>,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
    )>,
    alive_confirmed: impl Fn(i32, Option<chrono::DateTime<chrono::Utc>>) -> bool,
) -> (
    Vec<(String, String, chrono::DateTime<chrono::Utc>)>,
    Vec<i32>,
) {
    let mut dead_pids = Vec::new();
    let reconciled = rows
        .into_iter()
        .map(|(label, status, pid, created, started_at)| {
            let running = matches!(status.as_str(), "running" | "starting");
            let alive = pid
                .map(|p| p > 0 && alive_confirmed(p, started_at))
                .unwrap_or(false);
            if running && !alive {
                if let Some(p) = pid {
                    if p > 0 {
                        dead_pids.push(p);
                    }
                }
                (label, "stopped".to_string(), created)
            } else {
                (label, status, created)
            }
        })
        .collect();
    (reconciled, dead_pids)
}

/// Windows: elenca i servizi di progetto come processi gestiti (agent_processes
/// kind='service'), nella STESSA shape di list_services_fallback. Su Windows non
/// esistono unit systemd: l'install (install_service_windows) registra i servizi
/// qui. Voci calcolate da visible_windows_services (dedup per label, voci
/// fantasma nascoste). Riconcilia (e persiste) le righe stale prima del display.
#[cfg(windows)]
pub(super) async fn list_services_windows(
    db: &sqlx::PgPool,
    project_id: Uuid,
    slug: &str,
) -> Vec<serde_json::Value> {
    // Separazione DB per-progetto: agent_processes e' tabella migrata, instrada
    // sul pool del progetto. DB progetto non disponibile -> lista vuota con WARN
    // (display best-effort; niente fallback al meta: li' la tabella e' vuota).
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "list_services_windows: DB progetto non disponibile, elenco servizi vuoto"
            );
            return Vec::new();
        }
    };
    let rows: Vec<(
        String,
        String,
        Option<i32>,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT label, status, pid, created_at, started_at FROM agent_processes \
             WHERE project_id = $1 AND kind = 'service' \
             ORDER BY label, created_at DESC",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();

    // Self-heal: una riga running/starting con pid morto O RICICLATO diventa
    // 'stopped' sia nel display sia in DB, cosi' il pannello Servizi non mostra
    // 'running' un processo defunto/estraneo (stato stale) e resta COERENTE col
    // pannello Problemi (l'observer usa lo stesso criterio in windows_pid_state).
    // Il predicato combina liveness (process_alive: intercetta il processo uscito
    // con handle ancora aperto) e identita' (pid_identity_confirmed: intercetta il
    // PID riciclato dal SO su un processo estraneo).
    let (reconciled, dead_pids) = reconcile_dead_service_rows(rows, |p, started| {
        crate::process_util::process_alive(p as u32)
            && crate::process_util::pid_identity_confirmed(
                p as u32,
                started.map(|t: chrono::DateTime<chrono::Utc>| t.timestamp()),
            )
    });
    if !dead_pids.is_empty() {
        let _ = sqlx::query(
            "UPDATE agent_processes SET status = 'stopped', stopped_at = now() \
             WHERE project_id = $1 AND kind = 'service' \
               AND status IN ('running', 'starting') AND pid = ANY($2)",
        )
        .bind(project_id)
        .bind(&dead_pids)
        .execute(&proj_pool)
        .await;
        tracing::info!(
            project_id = %project_id,
            count = dead_pids.len(),
            "list_services_windows: servizi con pid morto riconciliati a stopped"
        );
    }

    visible_windows_services(&reconciled)
        .into_iter()
        .map(|(label, running)| {
            json!({
                "unit":       service_unit_name(slug, &label),
                "short":      label,
                "state":      if running { "active" } else { "inactive" },
                "sub":        if running { "running" } else { "dead" },
                "managed_by": "windows",
            })
        })
        .collect()
}

/// Vero se il servizio e' in crash-loop: momentaneamente `active` ma con
/// `NRestarts > 2` (es. `dotnet run` che impiega ~40s a buildare prima di fallire).
/// Non applicabile se gia' `is_failing`. Estratto da `get_project_services_status`.
async fn service_is_crash_looping(unit: &str, active: &str, is_failing: bool) -> bool {
    if is_failing || active != "active" {
        return false;
    }
    // Controlla NRestarts: se > 2, il servizio sta ciclando
    tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit, "--property=NRestarts"])
        .output()
        .await
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            s.trim()
                .strip_prefix("NRestarts=")
                .and_then(|v| v.parse::<u32>().ok())
        })
        .is_some_and(|n| n > 2)
}

/// Se il servizio `unit` e' failing o in crash-loop, legge il journal e annota su
/// `entry` `last_error`/`suggestion`/`error_kind` (+ `crash_loop`). Diagnosi via
/// punto unico DB (`diagnose_log_db`) con fallback euristico. Estratto da
/// `get_project_services_status` per tenerla sotto soglia (comportamento invariato).
async fn annotate_service_diagnosis(
    db: &sqlx::PgPool,
    unit: &str,
    active: &str,
    sub: &str,
    root: &std::path::Path,
    entry: &mut serde_json::Value,
) {
    // Rileva anche servizi momentaneamente "active" ma con NRestarts elevato.
    let is_failing = (active == "activating" && sub == "auto-restart") || active == "failed";
    let is_crash_looping = service_is_crash_looping(unit, active, is_failing).await;

    if !is_failing && !is_crash_looping {
        return;
    }
    let Ok(journal) = tokio::process::Command::new("journalctl")
        .args(["--user", "-u", unit, "--no-pager", "-n", "20", "-o", "cat"])
        .output()
        .await
    else {
        return;
    };
    let log = String::from_utf8_lossy(&journal.stdout).to_string();
    // Punto unico (regola L): prova prima i pattern configurabili in
    // nexus_dev_diagnostics (DB, hot-reload); fallback all'euristica hardcoded
    // come rete di sicurezza se nessun pattern DB matcha.
    let (err, sugg, kind) =
        match crate::agent_tools::dev_diagnostics::diagnose_log_db(db, &log).await {
            Some((desc, fix, cat)) => (desc, fix, cat),
            None => {
                let d = diagnose_service_failure(&log, unit, root);
                (d.error, d.suggestion, d.kind.to_string())
            }
        };
    entry["last_error"] = json!(err);
    entry["suggestion"] = json!(sugg);
    entry["error_kind"] = json!(kind);
    if is_crash_looping && !is_failing {
        entry["crash_loop"] = json!(true);
    }
}

// ── POST /api/projects/:id/services/:service/:action ─────────────────────────
// service: "backend" | "brain" | "frontend"
// action:  "start" | "stop" | "restart"

/// Elenca tutti i servizi systemd --user il cui nome inizia con `{slug}-`.
/// Nessun hardcoding: il progetto può avere quanti servizi vuole.
/// Su Windows (niente systemd) usa il ramo dedicato (list_services_windows).
#[cfg_attr(windows, allow(unreachable_code))]
pub async fn get_project_services_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Su Windows non c'e' systemd: i servizi sono processi gestiti registrati in
    // agent_processes (install_service_windows). Elencali da li' invece di
    // invocare systemctl (che non esiste -> hang/500 -> pannello "Caricamento...").
    #[cfg(windows)]
    {
        let services = list_services_windows(&state.db, project_id, &slug).await;
        return Ok(Json(json!({
            "services": services,
            "slug": slug,
            "manager_unavailable": true,
            "manager_mode": "windows",
            "manager_hint": "Su Windows i servizi di progetto sono processi gestiti (niente systemd).",
        })));
    }

    // `systemctl --user list-units --type=service --all --no-legend --no-pager`
    // restituisce righe: "  UNIT  LOAD  ACTIVE  SUB  DESCRIPTION"
    let out = tokio::process::Command::new("systemctl")
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
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Distinzione critica (ADR 0022): bus systemd utente irraggiungibile vs zero
    // servizi. Senza questo check il frontend mostrerebbe "Nessun servizio trovato"
    // anche quando il manager `user@<uid>` e' semplicemente inactive (tipico WSL).
    if user_manager_unavailable(&out) {
        // Fix definitivo (regola H): invece di mostrare solo il warning, elenca
        // i servizi dai file unit + stato dei processi detached. Il pannello
        // resta funzionale senza systemd --user e senza sudo.
        let services = list_services_fallback(&slug, &context.root_path).await;
        return Ok(Json(json!({
            "services": services,
            "slug": slug,
            "manager_unavailable": true,
            "manager_mode": "detached",
            "manager_hint": USER_MANAGER_HINT,
        })));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut services =
        parse_systemctl_services(&state.db, &stdout, &slug, &context.root_path).await;

    // Merge con gli unit FILE su disco (servizi non caricati nel manager).
    merge_missing_unit_files(&mut services, &slug, &context.root_path).await;

    Ok(Json(json!({
        "services": services,
        "slug": slug,
        "manager_unavailable": false,
    })))
}

/// Parsa lo stdout di `systemctl --user list-units` filtrando le unit del progetto
/// (`{slug}-*.service`) e ne costruisce le voci JSON, annotando la diagnosi per
/// quelle failing/crash-loop. Estratto da `get_project_services_status`
/// (comportamento invariato).
async fn parse_systemctl_services(
    db: &sqlx::PgPool,
    stdout: &str,
    slug: &str,
    root_path: &std::path::Path,
) -> Vec<serde_json::Value> {
    let prefix = format!("{}-", slug);
    let mut services: Vec<serde_json::Value> = Vec::new();
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let unit = cols[0].trim_start_matches('●').trim();
        if !unit.starts_with(&prefix) || !unit.ends_with(".service") {
            continue;
        }
        let active = cols[2]; // "active" | "inactive" | "failed" | ...
        let sub = cols[3]; // "running" | "exited" | "dead" | ...
                           // nome corto: rimuove il prefisso slug e il suffisso .service
        let short = unit
            .strip_prefix(&prefix)
            .unwrap_or(unit)
            .strip_suffix(".service")
            .unwrap_or(unit);

        let mut entry = json!({
            "unit":   unit,
            "short":  short,
            "state":  active,
            "sub":    sub,
        });

        // Se il servizio e' in crash-loop o failed, annota la diagnosi dal journal.
        annotate_service_diagnosis(db, unit, active, sub, root_path, &mut entry).await;

        services.push(entry);
    }
    services
}

/// Unisce a `services` gli unit file su disco non gia' elencati da systemctl:
/// `systemctl --user list-units` puo' NON elencare servizi installati quando il
/// manager utente era giu' all'install (tipico WSL: il file .service esiste in
/// ~/.config/systemd/user/ ma non e' caricato nel manager). Cosi' il pannello
/// mostra SEMPRE tutti i servizi del progetto. Riordina per `short`. Estratto da
/// `get_project_services_status` (comportamento invariato).
async fn merge_missing_unit_files(
    services: &mut Vec<serde_json::Value>,
    slug: &str,
    root_path: &std::path::Path,
) {
    let known: std::collections::HashSet<String> = services
        .iter()
        .filter_map(|s| s.get("unit").and_then(|u| u.as_str()).map(String::from))
        .collect();
    for fb in list_services_fallback(slug, root_path).await {
        let unit = fb
            .get("unit")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        if !unit.is_empty() && !known.contains(&unit) {
            services.push(fb);
        }
    }
    services.sort_by(|a, b| a["short"].as_str().cmp(&b["short"].as_str()));
}

/// Controlla un servizio del progetto: start | stop | restart.
/// Il parametro `service` è il nome corto (senza prefisso slug e senza .service),
/// ad es. "backend", "api-v2", "worker-email".
/// Il backend verifica che il nome dell'unità risultante inizi con `{slug}-`.
pub async fn control_project_service(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path((id, service, action)): axum::extract::Path<(String, String, String)>,
) -> ApiResult {
    // Dispatch per piattaforma (regola L): su Windows i servizi sono processi gestiti
    // in agent_processes -> start = ri-spawn, stop = taskkill, restart = stop+start.
    #[cfg(windows)]
    {
        control_project_service_windows(state, claims, id, service, action).await
    }
    #[cfg(not(windows))]
    {
        control_project_service_systemd(state, claims, id, service, action).await
    }
}

/// STOP esplicito di una label servizio Windows: taskkill /T /F dei soli processi
/// `running` di QUELLA label e marca `stopped` in agent_processes. Non tocca gli
/// altri servizi. Estratto da `control_project_service_windows` (comportamento invariato).
#[cfg(windows)]
async fn stop_windows_service_label(proj_pool: &sqlx::PgPool, project_id: Uuid, short: &str) {
    let running: Vec<(Option<i32>,)> = sqlx::query_as(
        "SELECT pid FROM agent_processes \
         WHERE project_id = $1 AND label = $2 AND kind = 'service' AND status = 'running'",
    )
    .bind(project_id)
    .bind(short)
    .fetch_all(proj_pool)
    .await
    .unwrap_or_default();
    for (pid,) in running {
        if let Some(p) = pid {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/PID", &p.to_string(), "/T", "/F"])
                .output()
                .await;
        }
    }
    let _ = sqlx::query(
        "UPDATE agent_processes SET status = 'stopped', stopped_at = now() \
         WHERE project_id = $1 AND label = $2 AND kind = 'service' AND status = 'running'",
    )
    .bind(project_id)
    .bind(short)
    .execute(proj_pool)
    .await;
}

/// Alloca (o riusa) la porta stabile del bucket per un web service Windows e la
/// ritorna come env `PORT`/`HOST` da iniettare nello spawn. Collega l'allocazione
/// al service_unit (preservazione dal GC, fix f0057b0). Ritorna None per i worker
/// non-web o se l'allocazione fallisce (avvio senza PORT iniettato). Estratto da
/// `control_project_service_windows` per tenerla sotto soglia (comportamento invariato).
#[cfg(windows)]
async fn allocate_web_service_port_env(
    state: &AppState,
    project_id: Uuid,
    slug: &str,
    short: &str,
    command: &str,
) -> Option<std::collections::HashMap<String, String>> {
    // Gate sull'euristica web-service (stessa di run_service, regola L): un
    // worker non-web resta invariato, senza PORT iniettato.
    if !crate::agent_tools::service::looks_like_web_service(command) {
        return None;
    }
    // Instrada il servizio managed sul percorso ALLOCA+INIETTA (regola L, riuso
    // di find_or_allocate) invece del detect-path: Nexus assegna la porta stabile
    // del bucket PRIMA dello spawn e la inietta come env PORT/HOST, cosi' il
    // servizio non sceglie piu' una porta propria che poi verrebbe soltanto
    // "rilevata" (allocation_mode='auto' con service_unit NULL -> rilasciata dal
    // GC -> drift 31792->31798, incidente Beaty-Book).
    match super::find_or_allocate_port(&state.db, &state.port_registry, project_id, short).await {
        Ok(alloc) => {
            // Unit dal punto unico (regola L): ricopiare qui il `format!` faceva
            // vivere la formula in due posti, e il commento di `service_unit_name`
            // chiede proprio che questo valore combaci con quello del pannello.
            let unit_name = service_unit_name(slug, short);
            super::allocate_port::link_allocation_to_service_unit(
                &state.db, project_id, short, &unit_name,
            )
            .await;
            tracing::info!(
                port = alloc.port,
                label = %short,
                mode = alloc.mode,
                "control_service_windows: PORT alloc+iniettato e service_unit collegato"
            );
            let mut env = std::collections::HashMap::new();
            env.insert("PORT".to_string(), alloc.port.to_string());
            env.insert("HOST".to_string(), "0.0.0.0".to_string());
            Some(env)
        }
        Err(e) => {
            tracing::warn!(
                label = %short,
                "control_service_windows: find_or_allocate fallita ({e}); avvio senza PORT iniettato"
            );
            None
        }
    }
}

/// Windows: start/stop/restart di un servizio di progetto (processo in agent_processes).
#[cfg(windows)]
async fn control_project_service_windows(
    state: AppState,
    claims: Claims,
    id: String,
    service: String,
    action: String,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    // Slug dal punto unico (regola L): la formula ricopiata a mano qui era la
    // stessa di `project_service_slug`, e due copie della stessa formula sono due
    // unit divergenti al primo ritocco.
    let slug = project_service_slug(&context.details.name);

    if service.contains('/') || service.contains("..") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome servizio non valido",
        ));
    }
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("Azione non valida: {action}"),
        ));
    }
    // nome corto: rimuovi prefisso "{slug}-" e suffisso ".service" se presenti.
    let short = service
        .strip_prefix(&format!("{slug}-"))
        .unwrap_or(&service)
        .strip_suffix(".service")
        .unwrap_or(&service)
        .to_string();

    // Separazione DB per-progetto: agent_processes e' migrata, instrada le query
    // di questo handler sul pool del progetto (errore tipizzato 503/404 se non
    // disponibile). Risolto una volta sola e riusato dalle 3 query sotto (stesso
    // project_id).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;

    // STOP esplicito: taskkill dei soli processi running di QUESTA label (lo
    // stop richiesto dall'utente non deve toccare gli altri servizi). Per
    // start/restart la parte di stop e' delegata al punto unico piu' sotto,
    // che copre anche le varianti simili della stessa label.
    if action == "stop" {
        stop_windows_service_label(&proj_pool, project_id, &short).await;
    }

    // START (anche seconda parte di RESTART): ri-spawn dalla definizione piu' recente.
    if action == "start" || action == "restart" {
        start_windows_service(&state, &context, &proj_pool, project_id, &slug, &short).await?;
    }

    Ok(Json(json!({
        "ok": true,
        "service": short,
        "action": action,
        "manager_mode": "windows-process",
    })))
}

/// Avvia (o riavvia) un servizio Windows: ferma le varianti duplicate della label,
/// carica la definizione piu' recente da agent_processes, alloca la porta del
/// bucket per i web service e fa lo spawn. Estratto da
/// `control_project_service_windows` per tenerla sotto soglia (comportamento invariato).
#[cfg(windows)]
async fn start_windows_service(
    state: &AppState,
    context: &crate::projects::ProjectContext,
    proj_pool: &sqlx::PgPool,
    project_id: Uuid,
    slug: &str,
    short: &str,
) -> Result<(), ApiError> {
    // PUNTO UNICO anti-duplicato (regola L): ferma la label esatta E le
    // varianti dello stesso scopo ("frontend-dev" quando riavvii "frontend")
    // prima dello spawn. Senza questo, start/restart dal pannello accumulava
    // server duplicati sulla stessa codebase.
    let _ =
        crate::agent_processes::stop_similar_running_services(&state.db, project_id, short).await;
    let def: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT command, working_dir FROM agent_processes \
         WHERE project_id = $1 AND label = $2 AND kind = 'service' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(short)
    .fetch_optional(proj_pool)
    .await
    .ok()
    .flatten();
    let (command, working_dir) = def.ok_or_else(|| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("Servizio '{short}' non trovato"),
        )
    })?;
    let cwd = working_dir
        .filter(|w| !w.trim().is_empty())
        .unwrap_or_else(|| context.root_path.to_string_lossy().to_string());

    let port_env = allocate_web_service_port_env(state, project_id, slug, short, &command).await;

    crate::agent_processes::spawn_agent_process(
        &state.db,
        project_id,
        None,
        short,
        &command,
        &cwd,
        Some(context.root_path.clone()),
        port_env, // porta del bucket iniettata come PORT/HOST per i web service (alloca+inietta)
        false,    // niente sandbox Docker su Windows
        "service",
        None,
    )
    .await
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Avvio fallito: {e}"),
        )
    })?;
    Ok(())
}

#[cfg(not(windows))]
async fn control_project_service_systemd(
    state: AppState,
    claims: Claims,
    id: String,
    service: String,
    action: String,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Sicurezza: il service name non può contenere '/' o '..' e deve iniziare col prefisso slug
    if service.contains('/') || service.contains("..") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome servizio non valido",
        ));
    }

    // Costruisce il nome unit: se il chiamante manda già il nome completo (slug-xxx) lo usa,
    // altrimenti lo antepone.
    let svc_name = if service.starts_with(&format!("{}-", slug)) {
        format!("{}.service", service)
    } else {
        format!("{}-{}.service", slug, service)
    };

    let systemctl_action = match action.as_str() {
        "start" | "stop" | "restart" => action.as_str(),
        other => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!("Azione non valida: {}", other),
            ))
        }
    };

    // Pre-check: prima di start/restart, libera le porte occupate da processi estranei
    let mut freed_ports: Vec<serde_json::Value> = Vec::new();
    if systemctl_action == "start" || systemctl_action == "restart" {
        freed_ports = free_ports_for_unit(&svc_name).await;
        if !freed_ports.is_empty() {
            tracing::info!(
                "Pre-start {}: liberate {} porte occupate",
                svc_name,
                freed_ports.len()
            );
        }
    }

    let out = tokio::process::Command::new("systemctl")
        .args(["--user", systemctl_action, &svc_name])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Fix definitivo (regola H): se il manager systemd utente e' giu' (WSL),
    // gestisci start/stop/restart in modalita' detached leggendo l'unit file,
    // senza richiedere sudo. Coerente con l'avvio del wizard install.
    if user_manager_unavailable(&out) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
        let unit_path = format!("{home}/.config/systemd/user/{svc_name}");
        let content = tokio::fs::read_to_string(&unit_path)
            .await
            .unwrap_or_default();
        let exec_start = unit_exec_start(&content);
        if exec_start.is_empty() {
            return Ok(Json(json!({
                "ok": false,
                "unit": svc_name,
                "action": systemctl_action,
                "stderr": format!("unit file assente o senza ExecStart: {unit_path}"),
                "manager_mode": "detached",
            })));
        }
        let cwd = {
            let w = unit_working_dir(&content);
            if w.is_empty() {
                context.root_path.to_string_lossy().to_string()
            } else {
                w
            }
        };
        let env_map = unit_env_map(&content);
        let (ok, msg) = match systemctl_action {
            "stop" => {
                let _ = tokio::process::Command::new("pkill")
                    .args(["-f", &exec_start])
                    .output()
                    .await;
                (true, "fermato (detached)".to_string())
            }
            // start e restart: spawn_detached_service e' idempotente (fa pkill
            // del precedente prima di riavviare), quindi copre entrambi.
            _ => {
                match super::wizard::spawn_detached_service(&svc_name, &cwd, &env_map, &exec_start)
                    .await
                {
                    Ok(log) => (true, format!("avviato (detached), log={log}")),
                    Err(e) => (false, e),
                }
            }
        };
        if ok {
            let evt = match systemctl_action {
                "start" => nexus_events::event::ProjectEvent::ServiceStarted {
                    name: svc_name.clone(),
                    port: None,
                    pid: None,
                },
                "stop" => nexus_events::event::ProjectEvent::ServiceStopped {
                    name: svc_name.clone(),
                },
                _ => nexus_events::event::ProjectEvent::ServiceRestarted {
                    name: svc_name.clone(),
                },
            };
            nexus_events::dispatcher::emit(&state.project_channels, project_id, evt);
        }
        return Ok(Json(json!({
            "ok": ok,
            "unit": svc_name,
            "action": systemctl_action,
            "stdout": msg,
            "manager_mode": "detached",
            "freed_ports": freed_ports,
        })));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let ok = out.status.success();

    if ok {
        let evt = match systemctl_action {
            "start" => nexus_events::event::ProjectEvent::ServiceStarted {
                name: svc_name.clone(),
                port: None,
                pid: None,
            },
            "stop" => nexus_events::event::ProjectEvent::ServiceStopped {
                name: svc_name.clone(),
            },
            "restart" => nexus_events::event::ProjectEvent::ServiceRestarted {
                name: svc_name.clone(),
            },
            _ => unreachable!("validated above"),
        };
        nexus_events::dispatcher::emit(&state.project_channels, project_id, evt);
    }

    Ok(Json(json!({
        "ok":     ok,
        "unit":   svc_name,
        "action": systemctl_action,
        "stdout": stdout,
        "stderr": stderr,
        "freed_ports": freed_ports,
    })))
}

/// Riavvia un'unita' systemd di progetto per NOME COMPLETO (es.
/// "beauty-book-backend.service"), best-effort, senza passare per l'handler HTTP.
/// Usato dall'auto-remediation (dopo che il debugger ha applicato un fix) per
/// chiudere il loop rileva->ripara->RIAVVIA->verifica: l'observer al ciclo
/// successivo vede il nuovo stato. Condivide gli helper di
/// `control_project_service` (free_ports_for_unit, fallback detached WSL) — punto
/// unico a livello di helper (regola L).
pub async fn restart_project_unit(state: &AppState, project_id: Uuid, unit: &str) {
    use super::service_manager::{self, ServiceBackend};

    if unit.contains('/') || unit.contains("..") || !unit.ends_with(".service") {
        tracing::warn!(unit = %unit, "restart_project_unit: nome unit non valido, skip");
        return;
    }

    // Carica nome + root del progetto per costruire il contesto del ServiceManager
    // (chiamata interna senza user: query diretta su projects).
    let proj: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT name, repository_root_path FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let (name, root_opt) = match proj {
        Some(p) => p,
        None => {
            tracing::warn!(unit = %unit, %project_id, "restart_project_unit: progetto non trovato, skip");
            return;
        }
    };
    let slug = project_service_slug(&name);
    let root = std::path::PathBuf::from(root_opt.unwrap_or_default());
    let short = unit
        .strip_prefix(&format!("{slug}-"))
        .unwrap_or(unit)
        .strip_suffix(".service")
        .unwrap_or(unit);

    // Punto unico (regola L): delega al ServiceManager della piattaforma (Windows:
    // agent_processes; Linux: systemctl --user + fallback detached, con pre-free
    // porte incapsulato nel backend). L'evento ServiceRestarted si emette SOLO se
    // l'azione e' avvenuta davvero (outcome.acted, regola M): chiude il bug per cui
    // su Windows, dove systemctl e' assente, la funzione non riavviava nulla ma
    // emetteva comunque ServiceRestarted, mentendo al loop di auto-remediation.
    let ctx = service_manager::ServiceContext {
        db: &state.db,
        port_registry: Some(&state.port_registry),
        project_id,
        slug: &slug,
        project_root: &root,
    };
    let outcome = service_manager::active().restart(&ctx, short).await;
    if outcome.acted {
        nexus_events::dispatcher::emit(
            &state.project_channels,
            project_id,
            nexus_events::event::ProjectEvent::ServiceRestarted {
                name: unit.to_string(),
            },
        );
        tracing::info!(unit = %unit, msg = %outcome.message, "restart_project_unit: riavvio effettuato (auto-remediation)");
    } else {
        tracing::warn!(unit = %unit, msg = %outcome.message, "restart_project_unit: riavvio NON effettuato, nessun evento emesso");
    }
}

// ── POST /api/projects/:id/services/restart-all ─────────────────────────────
/// Riavvia in batch tutti i `{slug}-*.service` del progetto.
pub async fn restart_all_project_services(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    use super::service_manager::{self, ServiceBackend};

    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = project_service_slug(&context.details.name);

    // Punto unico (regola L): enumera e riavvia i servizi del progetto tramite il
    // ServiceManager della piattaforma. Su Windows usa agent_processes (prima
    // ritornava HTTP 500 perche' `systemctl` non esiste); su Linux usa i file unit
    // + systemctl con pre-free porte incapsulato nel backend. L'esito per servizio
    // e' `acted` (segnale strutturato, regola M), non lo stderr di un comando.
    let ctx = service_manager::ServiceContext {
        db: &state.db,
        port_registry: Some(&state.port_registry),
        project_id,
        slug: &slug,
        project_root: &context.root_path,
    };
    let mgr = service_manager::active();
    let mut results = Vec::new();
    for entry in mgr.list(&ctx).await {
        let outcome = mgr.restart(&ctx, &entry.label).await;
        results.push(json!({
            "unit": entry.id,
            "short": entry.label,
            "ok": outcome.acted,
            "message": outcome.message,
        }));
    }
    Ok(Json(json!({ "slug": slug, "restarted": results })))
}

// ── POST /api/projects/:id/services/cleanup-ports ───────────────────────────
/// Termina i processi che occupano porte rilevate per il progetto MA non sono
/// gestiti da systemd `{slug}-*.service` (porte "orfane" o conflittuali).
/// Body opzionale: { "ports": [3002, 5215, ...] } per limitare l'azione.
/// Vero se il listener `(pid, port)` NON va MAI terminato dal cleanup porte di un
/// progetto (anti-suicidio, regola E):
/// - `pid` 0 o 1: NON terminabili. `kill -TERM 0` colpirebbe l'INTERO process
///   group di mcp-core (suicidio di gruppo), `kill 1` e' init. I container Docker
///   pubblicano le porte senza un PID visibile a `ss` -> il parser le riporta con
///   pid 0: vanno saltate, non killate.
/// - `pid == own_pid`: mcp-core stesso.
/// - porta riservata Nexus (mcp-core 4000, microservizi 40xx, brain 50051,
///   gateway 4060, ...). In WSL `systemctl --user` non popola `protected_pids`,
///   quindi questa e' l'unica barriera che impedisce al reset porte di uccidere
///   il core (e gli altri servizi) dal pannello.
pub(super) fn is_protected_nexus_listener(pid: u32, port: u16, own_pid: u32) -> bool {
    pid == 0 || pid == 1 || pid == own_pid || NEXUS_RESERVED_PORTS.contains(&port)
}

/// PUNTO UNICO (regola L) dell'enumerazione delle unit systemd `--user` di un
/// progetto: `systemctl --user list-units` e filtro dei nomi che iniziano con
/// `prefix` (tipicamente `{slug}-`). Ritorna la lista dei nomi unit; vuota se il
/// comando non e' eseguibile (il chiamante decide se e' un errore o un fallback).
async fn list_project_service_units(prefix: &str) -> Vec<String> {
    let list_out = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await;
    let Ok(list_out) = list_out else {
        return Vec::new();
    };
    String::from_utf8_lossy(&list_out.stdout)
        .lines()
        .filter_map(|line| {
            let unit = line.split_whitespace().next()?;
            unit.starts_with(prefix).then(|| unit.to_string())
        })
        .collect()
}

/// MainPID (> 0) letto dallo stdout di `systemctl --user show <unit>
/// --property=MainPID`. None se assente o non valido. Punto unico del parsing
/// `MainPID=` (regola L).
fn parse_main_pid_stdout(stdout: &str) -> Option<u32> {
    stdout.lines().find_map(|line| {
        line.strip_prefix("MainPID=")
            .and_then(|val| val.trim().parse::<u32>().ok())
            .filter(|&pid| pid > 0)
    })
}

/// Termina i listener non protetti tra quelli passati, rispettando la whitelist
/// `target_ports` (vuota = tutte) e l'anti-suicidio Nexus. Ritorna `(killed,
/// skipped)` come liste di oggetti JSON. Estratto da `cleanup_project_ports`
/// (comportamento invariato).
async fn kill_unprotected_listeners(
    listening: Vec<(u16, u32, String)>,
    target_ports: &std::collections::HashSet<u16>,
    protected_pids: &std::collections::HashSet<u32>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let mut killed = Vec::new();
    let mut skipped = Vec::new();
    for (port, pid, program) in listening {
        // Se è stata data una whitelist di porte, applica il filtro
        if !target_ports.is_empty() && !target_ports.contains(&port) {
            continue;
        }
        // ANTI-SUICIDIO (regola E): mai terminare mcp-core ne' altra
        // infrastruttura Nexus. Senza questo guard, in WSL (dove protected_pids
        // resta vuoto perche' `systemctl --user` non e' attivo) un reset porte
        // uccideva mcp-core stesso sulla 4000 -> "il core muore" dal pannello.
        if is_protected_nexus_listener(pid, port, std::process::id()) {
            skipped.push(json!({ "port": port, "pid": pid, "program": program, "reason": "protetto (infrastruttura Nexus o PID non terminabile)" }));
            continue;
        }
        if protected_pids.contains(&pid) {
            skipped.push(json!({ "port": port, "pid": pid, "program": program, "reason": "protetto (servizio del progetto)" }));
            continue;
        }
        // Terminazione via punto unico cross-platform (regola L): su Unix
        // esegue TERM+KILL con attesa e ricontrollo liveness incapsulati; su
        // Windows fa taskkill /T /F. Il precedente `kill` inline era no-op su
        // Windows (comando inesistente) -> porte mai liberate dal pannello.
        crate::process_util::kill_pid(pid).await;
        killed.push(json!({ "port": port, "pid": pid, "program": program }));
    }
    (killed, skipped)
}

/// Porte target del reset: quelle elencate nel body, oppure l'insieme vuoto
/// (= "tutte quelle rilevate", filtro disattivato) se il body manca o non ha un
/// array `ports` valido. Estratta da `cleanup_project_ports`; comportamento
/// invariato, inclusa la troncatura `as u16` dei valori fuori range.
fn parse_target_ports(body: Option<axum::Json<serde_json::Value>>) -> std::collections::HashSet<u16> {
    match body {
        Some(axum::Json(b)) => b
            .get("ports")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64())
                    .map(|n| n as u16)
                    .collect()
            })
            .unwrap_or_default(),
        None => std::collections::HashSet::new(),
    }
}

/// PUNTO UNICO (regola L) dell'espansione di un insieme di PID a tutti i loro
/// discendenti: BFS sull'albero `children` (parent -> figli). Un pid gia'
/// presente non viene riaccodato, quindi la BFS termina anche con cicli o pid
/// riciclati. Usata da entrambi i rami di `cleanup_project_ports` (Windows e
/// systemd), che prima ne tenevano due copie identiche: cambia solo il modo di
/// costruire `children`, non l'espansione.
fn expand_pids_with_descendants(
    pids: &mut std::collections::HashSet<u32>,
    children: &std::collections::HashMap<u32, Vec<u32>>,
) {
    let mut queue: std::collections::VecDeque<u32> = pids.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &c in kids {
                if pids.insert(c) {
                    queue.push_back(c);
                }
            }
        }
    }
}

/// Windows: pid vivi dei servizi del progetto (`agent_processes`, kind='service')
/// piu' tutti i discendenti risalendo l'albero processi Win32
/// (`windows_process_parents`). Estratta da `cleanup_project_ports`;
/// comportamento invariato (solo pid > 0 e vivi secondo il punto unico
/// `process_alive`, poi espansione ai discendenti).
#[cfg(windows)]
async fn collect_windows_protected_pids(
    state: &AppState,
    project_id: Uuid,
) -> Result<std::collections::HashSet<u32>, ApiError> {
    let mut protected_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    // DB progetto non disponibile -> propaga (503): con protected_pids vuoto
    // il reset ucciderebbe i servizi legittimi del progetto.
    let proj_pool = crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let svc_pids: Vec<(Option<i32>,)> = sqlx::query_as(
        "SELECT pid FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service' \
           AND status IN ('running', 'starting') AND pid IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();
    for (pid,) in svc_pids {
        if let Some(p) = pid {
            if p > 0 && crate::process_util::process_alive(p as u32) {
                protected_pids.insert(p as u32);
            }
        }
    }
    // Espansione ai discendenti: mappa parent->children (Win32 invertita).
    let child_to_parent = windows_process_parents().await;
    let mut parent_to_children: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::new();
    for (child, parent) in &child_to_parent {
        parent_to_children.entry(*parent).or_default().push(*child);
    }
    expand_pids_with_descendants(&mut protected_pids, &parent_to_children);
    Ok(protected_pids)
}

pub async fn cleanup_project_ports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    body: Option<axum::Json<serde_json::Value>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Porte target: dal body o, se assente, tutte quelle rilevate nel progetto
    let target_ports: std::collections::HashSet<u16> = parse_target_ports(body);

    // PID protetti: i servizi del progetto (e i loro discendenti) non vanno mai
    // uccisi dal reset porte. La sorgente e' platform-specific (punto unico a
    // livello di concern, regola L): su Windows i servizi sono processi gestiti in
    // agent_processes; su Linux sono i MainPID delle unit systemd `{slug}-*`.
    let mut protected_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();

    #[cfg(windows)]
    {
        protected_pids.extend(collect_windows_protected_pids(&state, project_id).await?);
    }

    #[cfg(not(windows))]
    {
        // Linux: MainPID dei servizi systemd `{slug}-*` del progetto (PID protetti).
        let prefix = format!("{}-", slug);
        let list_out = tokio::process::Command::new("systemctl")
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
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let list_str = String::from_utf8_lossy(&list_out.stdout);
        let units: Vec<String> = list_str
            .lines()
            .filter_map(|line| {
                let unit = line.split_whitespace().next()?;
                if unit.starts_with(&prefix) {
                    Some(unit.to_string())
                } else {
                    None
                }
            })
            .collect();

        for unit in &units {
            let show_out = tokio::process::Command::new("systemctl")
                .args(["--user", "show", unit, "--property=MainPID"])
                .output()
                .await
                .ok();
            if let Some(o) = show_out {
                let s = String::from_utf8_lossy(&o.stdout);
                for line in s.lines() {
                    if let Some(val) = line.strip_prefix("MainPID=") {
                        if let Ok(pid) = val.trim().parse::<u32>() {
                            if pid > 0 {
                                protected_pids.insert(pid);
                            }
                        }
                    }
                }
            }
        }

        // Espande PID protetti con tutti i discendenti (BFS) per non ucciderli per sbaglio.
        // Scan /proc sincrono -> spawn_blocking per non bloccare il runtime tokio.
        let children: std::collections::HashMap<u32, Vec<u32>> =
            tokio::task::spawn_blocking(|| {
                let mut map: std::collections::HashMap<u32, Vec<u32>> =
                    std::collections::HashMap::new();
                if let Ok(proc_entries) = std::fs::read_dir("/proc") {
                    for entry in proc_entries.flatten() {
                        let n = entry.file_name();
                        let s = n.to_string_lossy();
                        if let Ok(pid) = s.parse::<u32>() {
                            if let Ok(content) =
                                std::fs::read_to_string(format!("/proc/{}/status", pid))
                            {
                                for line in content.lines() {
                                    if let Some(rest) = line.strip_prefix("PPid:") {
                                        if let Ok(ppid) = rest.trim().parse::<u32>() {
                                            map.entry(ppid).or_default().push(pid);
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                map
            })
            .await
            .unwrap_or_default();
        expand_pids_with_descendants(&mut protected_pids, &children);
    }

    // Trova tutti i processi che ascoltano sulle porte e killa quelli non protetti.
    // Punto unico (regola L): il ServiceManager della piattaforma fornisce le terne
    // (porta, pid, programma) — su Windows via Get-NetTCPConnection, su Linux via
    // ss/proc. Prima usava solo ss/proc -> lista vuota su Windows -> il reset non
    // liberava nulla.
    let listening: Vec<(u16, u32, String)> = {
        use super::service_manager::ServiceBackend;
        super::service_manager::active()
            .listening_ports()
            .await
            .into_iter()
            .map(|l| (l.port, l.pid, l.program))
            .collect()
    };

    let (killed, skipped) =
        kill_unprotected_listeners(listening, &target_ports, &protected_pids).await;

    // Rilascia (DB + cache registry) le allocazioni delle porte EFFETTIVAMENTE
    // liberate, tramite il punto unico PortRegistryCache::release (regola L).
    // Senza, la sezione "Porte allocate" (nexus_port_allocations) continuava a
    // elencarle dopo il kill: dal pannello sembrava "il reset non aggiorna nulla".
    // NON tocca le porte "skipped" (servizi del progetto / infrastruttura
    // protetta), che non vengono killate.
    for entry in &killed {
        if let Some(p) = entry.get("port").and_then(|v| v.as_u64()) {
            let _ = state.port_registry.release(p as u16).await;
        }
    }

    Ok(Json(json!({
        "slug": slug,
        "killed": killed,
        "skipped": skipped,
    })))
}

pub async fn get_project_ports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let project_root = context.root_path.to_string_lossy().to_string();
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Storico: il Fix M37 iniettava anche le allocazioni non-live per non mostrare
    // "Nessuna porta rilevata"; il FIX 3b (scelta utente) ha RIMOSSO quell'iniezione
    // -> vedi il contratto attuale sotto. NON reintrodurre l'injection delle riserve.
    let mut ports = detect_project_ports(&project_root, &slug, project_id, &state.db).await;

    // Il pannello "Porte" mostra SOLO le porte realmente in ascolto (probe live).
    // Le allocazioni-riserva (servizio configurato ma FERMO) NON compaiono qui:
    // vivono nella sezione "Porte allocate" del pannello Run & Debug (endpoint
    // /port-allocations, fonte port_registry). Mostrare le riserve mescolate alle
    // live confondeva ("porte strane che non puntano ai servizi attivi").
    //
    // Marchiamo ogni voce come `live` per chiarezza del contratto e arricchiamo il
    // `service` mancante con la mappa AUTORITATIVA port->label di
    // nexus_port_allocations (regola L): cosi' il pannello Servizi aggancia sempre
    // il link al servizio corretto anche quando l'albero processi non risolve.
    for p in ports.iter_mut() {
        if let Some(obj) = p.as_object_mut() {
            obj.insert("live".to_string(), json!(true));
        }
    }
    let alloc_label_by_port: std::collections::HashMap<i32, String> =
        sqlx::query_as::<_, (i32, String)>(
            "SELECT port, COALESCE(label, '') FROM nexus_port_allocations \
             WHERE project_id = $1 AND COALESCE(label, '') <> ''",
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    assign_service_from_allocations(&mut ports, &alloc_label_by_port);

    // VISTA UNIFICATA (regola L): il pannello Porte mostra TUTTE le porte del
    // progetto — quelle realmente in ascolto (probe live sopra) E quelle
    // registrate ma ferme (registro allocazioni, fonte autoritativa di "cosa
    // appartiene al progetto"). Ogni voce porta `live` (in ascolto ora) e
    // `allocated` (nel registro): il frontend le distingue con un badge
    // attivo/fermo invece di due liste separate (endpoint /port-allocations
    // resta per la CRUD). Sostituisce il "FIX 3b" che nascondeva le riserve.
    let allocations = state.port_registry.ports_for_project(&project_id).await;
    let alloc_view: Vec<(i64, String, String)> = allocations
        .iter()
        .map(|a| (a.port as i64, a.label.clone(), a.allocation_mode.clone()))
        .collect();
    let ports = merge_ports_view(ports, &alloc_view);

    Ok(Json(json!({ "ports": ports })))
}

/// Pura (regola L / regola O, testabile su ogni piattaforma): fonde le porte
/// LIVE (probe, `live=true`, con pid/url) con il REGISTRO delle allocazioni
/// (`allocs` = (port, label, mode)) in un'unica vista ordinata per porta.
///
/// Contratto di ogni voce:
///   - `allocated`: la porta e' nel registro del progetto;
///   - `live`: la porta e' realmente in ascolto ORA;
///   - `allocation_mode`: dal registro (null se non allocata);
///   - `url`/`pid`/`state`: presenti SOLO se live (una porta ferma non ha un
///     endpoint raggiungibile — il pannello Servizi non deve linkarla).
///
/// Tre casi: allocata+live (arricchisce la voce live), allocata-ferma (voce
/// nuova senza url), live-non-allocata (rara: resta, `allocated=false`).
pub(super) fn merge_ports_view(
    live: Vec<serde_json::Value>,
    allocs: &[(i64, String, String)],
) -> Vec<serde_json::Value> {
    let mut live_by_port = index_live_by_port(live);
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut emitted: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 1. Ogni allocazione: arricchisce la voce live se c'e', altrimenti voce ferma.
    for (port, label, mode) in allocs {
        if !emitted.insert(*port) {
            continue;
        }
        match live_by_port.remove(port) {
            Some(mut v) => {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("allocated".to_string(), json!(true));
                    obj.insert("allocation_mode".to_string(), json!(mode));
                }
                out.push(v);
            }
            None => out.push(stopped_port_entry(*port, label, mode)),
        }
    }

    // 2. Porte live non allocate (rare): restano, allocated=false.
    for (port, v) in live_by_port {
        if emitted.insert(port) {
            out.push(v);
        }
    }

    // Ordine stabile per porta: la UI non deve lampeggiare tra i polling.
    out.sort_by_key(|v| v.get("port").and_then(serde_json::Value::as_i64).unwrap_or(0));
    out
}

/// Indicizza le voci live per porta, marcandole `live=true`/`allocated=false`
/// (l'allocazione viene poi impostata dal merge se la porta e' nel registro).
fn index_live_by_port(
    live: Vec<serde_json::Value>,
) -> std::collections::HashMap<i64, serde_json::Value> {
    let mut map = std::collections::HashMap::new();
    for mut v in live {
        if let Some(port) = v.get("port").and_then(serde_json::Value::as_i64) {
            if let Some(obj) = v.as_object_mut() {
                obj.entry("live").or_insert(json!(true));
                obj.insert("allocated".to_string(), json!(false));
            }
            map.insert(port, v);
        }
    }
    map
}

/// Voce di una porta REGISTRATA ma non in ascolto (nessun url linkabile).
fn stopped_port_entry(port: i64, label: &str, mode: &str) -> serde_json::Value {
    json!({
        "port": port,
        "label": label,
        "service": label,
        "allocated": true,
        "allocation_mode": mode,
        "live": false,
    })
}

/// MainPID dei servizi systemd `--user` `{slug}-*.service` vivi, con la mappa
/// pid->short_name associata. Estratto da `detect_project_ports` (blocco 2a) per
/// tenerla sotto soglia di lunghezza/complessita: interroga `systemctl --user
/// list-units` per l'elenco unit e `systemctl --user show ... MainPID` per ogni
/// unit, filtrando i pid vivi. Comportamento invariato.
async fn systemd_main_pids_by_service(
    slug: &str,
) -> (Vec<u32>, std::collections::HashMap<u32, String>) {
    let svc_prefix = format!("{}-", slug);
    let mut pid_to_service: std::collections::HashMap<u32, String> =
        std::collections::HashMap::new();
    let units = list_project_service_units(&svc_prefix).await;

    let mut pids = Vec::new();
    for unit in &units {
        let short = unit
            .strip_prefix(&svc_prefix)
            .unwrap_or(unit)
            .strip_suffix(".service")
            .unwrap_or(unit)
            .to_string();
        let show_out = tokio::process::Command::new("systemctl")
            .args(["--user", "show", unit, "--property=MainPID"])
            .output()
            .await
            .unwrap_or_else(|_| std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            });
        // Solo i MainPID di processi ancora vivi (punto unico process_alive).
        if let Some(pid) = parse_main_pid_stdout(&String::from_utf8_lossy(&show_out.stdout))
            .filter(|&pid| crate::process_util::process_alive(pid))
        {
            pids.push(pid);
            pid_to_service.insert(pid, short);
        }
    }
    (pids, pid_to_service)
}

/// PUNTO UNICO (regola L) della propagazione di un'attribuzione pid->T lungo
/// l'albero dei processi: BFS che parte dai pid gia' mappati (gli unici con
/// attribuzione nota a priori) e scende `children`. Un figlio gia' mappato non
/// viene ne' sovrascritto ne' riaccodato, quindi l'ordine delle passate non
/// perde i match anche se il figlio era gia' stato raccolto altrove.
///
/// Generico sul valore perche' i due chiamanti propagano attribuzioni diverse
/// con lo stesso identico algoritmo: pid->service (`propagate_service_to_descendants`,
/// da `detect_project_ports`) e pid->project_id (`detect_all_port_bindings`).
fn propagate_to_descendants<T: Clone>(
    map: &mut std::collections::HashMap<u32, T>,
    children: &std::collections::HashMap<u32, Vec<u32>>,
) {
    let mut queue: std::collections::VecDeque<u32> = map.keys().copied().collect();
    while let Some(pid) = queue.pop_front() {
        let Some(value) = map.get(&pid).cloned() else {
            continue;
        };
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                if let std::collections::hash_map::Entry::Vacant(e) = map.entry(child) {
                    e.insert(value.clone());
                    queue.push_back(child);
                }
            }
        }
    }
}

/// Propaga l'associazione pid->service ai discendenti dei MainPID systemd (gli
/// unici con service noto a priori). Delega al punto unico
/// [`propagate_to_descendants`]: comportamento invariato.
fn propagate_service_to_descendants(
    pid_to_service: &mut std::collections::HashMap<u32, String>,
    children: &std::collections::HashMap<u32, Vec<u32>>,
) {
    propagate_to_descendants(pid_to_service, children);
}

/// Assegna il campo `service` alle porte LIVE prive di risoluzione dall'albero
/// processi, usando la mappa autoritativa port->label di `nexus_port_allocations`
/// (regola L). Non sovrascrive un `service` gia' risolto. Pura e testabile.
pub(super) fn assign_service_from_allocations(
    ports: &mut [serde_json::Value],
    alloc_label_by_port: &std::collections::HashMap<i32, String>,
) {
    for p in ports.iter_mut() {
        let Some(obj) = p.as_object_mut() else {
            continue;
        };
        let has_service = obj
            .get("service")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_service {
            continue;
        }
        if let Some(port_num) = obj.get("port").and_then(|v| v.as_i64()) {
            if let Some(label) = alloc_label_by_port.get(&(port_num as i32)) {
                obj.insert("service".to_string(), json!(label));
            }
        }
    }
}

/// Converte le terne `(porta, pid, programma)` in ascolto nelle voci JSON del
/// pannello Porte, tenendo solo i pid appartenenti al progetto (`all_pids`) e
/// annotando il service dal `pid_to_service`. Estratto da `detect_project_ports`
/// (comportamento invariato).
fn listening_ports_to_json(
    listening: Vec<(u16, u32, String)>,
    all_pids: &std::collections::HashSet<u32>,
    pid_to_service: &std::collections::HashMap<u32, String>,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for (port_num, pid, program) in listening {
        if !all_pids.contains(&pid) {
            continue;
        }
        let label = if program.is_empty() {
            format!("Porta {}", port_num)
        } else {
            program.clone()
        };
        out.push(json!({
            "port": port_num,
            "label": label,
            "pid": pid,
            "state": "LISTEN",
            "url": format!("http://localhost:{}", port_num),
            "service": pid_to_service.get(&pid).cloned(),
        }));
    }
    out
}

/// Rileva le porte TCP in ascolto associate ai processi del progetto.
/// Strategia:
/// 1. Legge i PID dei processi agent_processes in esecuzione per il progetto
/// 2. Aggiunge i MainPID dei servizi systemd --user con prefisso {slug}-
/// 3. Scansiona /proc per qualsiasi processo con cwd nel project_root
/// 4. Espande con tutti i processi discendenti
#[cfg_attr(windows, allow(unreachable_code))]
pub(super) async fn detect_project_ports(
    project_root: &str,
    slug: &str,
    project_id: Uuid,
    db: &sqlx::PgPool,
) -> Vec<serde_json::Value> {
    // Windows nativo: niente systemctl/`/proc`/`ss`/docker-per-slug. I servizi
    // sono processi gestiti (agent_processes); la rilevazione live usa un
    // percorso dedicato (Get-NetTCPConnection + albero processi Win32_Process).
    #[cfg(windows)]
    {
        let _ = (project_root, slug);
        return detect_project_ports_windows(project_id, db).await;
    }

    let mut ports: Vec<serde_json::Value> = Vec::new();

    // 1. PID dai processi agent — include sia 'running' che altri status purché il processo sia ancora vivo.
    // Lo status nel DB può essere 'failed' dopo un riavvio di mcp-core anche se il processo gira ancora.
    // Separazione DB per-progetto: agent_processes e' migrata, instrada sul pool
    // del progetto. DB progetto non disponibile -> WARN e nessun PID da
    // agent_processes (restano le fonti systemd/cwd, che non passano dal DB).
    let agent_pids: Vec<i32> =
        match crate::project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(proj_pool) => sqlx::query(
                "SELECT pid FROM agent_processes WHERE project_id = $1 AND pid IS NOT NULL",
            )
            .bind(project_id)
            .fetch_all(&proj_pool)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|row| row.try_get::<i32, _>("pid").ok())
            // Verifica che il processo sia ancora vivo (punto unico cross-platform).
            .filter(|pid| crate::process_util::process_alive(*pid as u32))
            .collect(),
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "detect_project_ports: DB progetto non disponibile, salto agent_processes"
                );
                Vec::new()
            }
        };

    // 2a. MainPID dei servizi systemd --user `{slug}-*.service` + mappa pid→short_name
    let (systemd_pids, mut pid_to_service) = systemd_main_pids_by_service(slug).await;

    // 2a-bis. Servizi DETACHED (WSL/no systemd --user): il MainPID non esiste, ma
    // il wrapper avviato da spawn_detached_service e' rintracciabile via pgrep
    // sull'ExecStart del file unit su disco. Senza questo seed, pid_to_service
    // resta vuoto e TUTTE le porte risultano service=null -> nessun link in UI.
    for (pid, short) in detached_service_root_pids(slug).await {
        if crate::process_util::process_alive(pid) {
            pid_to_service.entry(pid).or_insert(short);
        }
    }

    // 2b. Raccogli tutti i PID rilevanti: agent + systemd + processi con cwd = project_root
    let mut all_pids: std::collections::HashSet<u32> = agent_pids
        .iter()
        .map(|p| *p as u32)
        .chain(systemd_pids)
        .collect();
    // Seed detached (2a-bis): i PID wrapper entrano nel set per la BFS discendenti.
    all_pids.extend(pid_to_service.keys().copied());

    // Scan /proc per costruire mappa figli e trovare processi con cwd nel project_root.
    // Tutto sincrono → spawn_blocking per non bloccare il runtime tokio.
    let project_root_owned = project_root.to_string();
    let (children, cwd_pids) =
        tokio::task::spawn_blocking(move || scan_proc_children_and_cwd(&project_root_owned))
            .await
            .unwrap_or_default();

    all_pids.extend(cwd_pids);

    // Espandi all_pids con tutti i discendenti dei PID già noti (BFS).
    let mut queue: std::collections::VecDeque<u32> = all_pids.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                if all_pids.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    // Propagazione `pid_to_service` ai discendenti (BFS dedicata dai MainPID systemd).
    propagate_service_to_descendants(&mut pid_to_service, &children);

    if all_pids.is_empty() {
        return ports;
    }

    // 3. Leggi le porte TCP in ascolto tramite ss (async) oppure /proc/net/tcp (sync via spawn_blocking)
    let listening = match read_listening_ports_ss().await {
        Ok(v) => v,
        Err(_) => tokio::task::spawn_blocking(read_listening_ports_proc)
            .await
            .unwrap_or_default(),
    };

    ports.extend(listening_ports_to_json(
        listening,
        &all_pids,
        &pid_to_service,
    ));

    // 4. Container Docker associati ai servizi del progetto: nome con prefisso slug
    ports.extend(docker_container_ports_for_slug(slug).await);

    // Dedup per porta
    ports.sort_by_key(|p| p["port"].as_u64().unwrap_or(0));
    ports.dedup_by_key(|p| p["port"].as_u64().unwrap_or(0));
    ports
}

/// Mappa una singola pubblicazione Docker `host->container` (es.
/// `0.0.0.0:5215->8080/tcp`) nella voce porta JSON del pannello. `None` se
/// l'entry non pubblica una porta host valida (nessun `->`, o porta host non
/// parsabile: casi gia' saltati dal codice inline che sostituisce). Lo
/// `svc_guess` deriva dal nome container togliendo il prefisso slug e i suffissi
/// di ambiente. Comportamento invariato.
fn docker_published_port_entry(
    entry: &str,
    cname: &str,
    prefix_dash: &str,
    prefix_underscore: &str,
) -> Option<serde_json::Value> {
    let entry = entry.trim();
    // Estrae la porta host: cerca pattern host_port->container_port
    let arrow_pos = entry.find("->")?;
    let host_part = &entry[..arrow_pos];
    let host_port: u16 = host_part
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    if host_port == 0 {
        return None;
    }
    // Tenta di derivare lo "short" del servizio dal nome container:
    // redemptor-backend-dev → "backend"; redemptor-sqlserver-dev → "sqlserver"
    let svc_guess = cname
        .strip_prefix(prefix_dash)
        .or_else(|| cname.strip_prefix(prefix_underscore))
        .map(|rest| {
            rest.trim_end_matches("-dev")
                .trim_end_matches("-prod")
                .trim_end_matches("_dev")
                .trim_end_matches("_prod")
                .to_string()
        });
    Some(json!({
        "port":    host_port,
        "label":   format!("docker:{}", cname),
        "pid":     0,
        "state":   "LISTEN",
        "url":     format!("http://localhost:{}", host_port),
        "service": svc_guess,
    }))
}

/// Porte pubblicate dai container Docker del progetto (nome con prefisso
/// `{slug}-`/`{slug}_` o contenente lo slug). Interroga `docker ps` e mappa ogni
/// pubblicazione `host->container` a una voce porta con `svc_guess` derivato dal
/// nome container. Estratto da `detect_project_ports` (blocco 4) per tenerla sotto
/// soglia; comportamento invariato. Vec vuoto se docker non e' raggiungibile.
async fn docker_container_ports_for_slug(slug: &str) -> Vec<serde_json::Value> {
    let Ok(docker_out) = tokio::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}|{{.Ports}}"])
        .output()
        .await
    else {
        return Vec::new();
    };
    let docker_str = String::from_utf8_lossy(&docker_out.stdout);
    let docker_prefix1 = format!("{}-", slug);
    let docker_prefix2 = format!("{}_", slug);
    let mut out = Vec::new();
    for line in docker_str.lines() {
        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() != 2 {
            continue;
        }
        let cname = parts[0].trim();
        // Filtra container appartenenti al progetto (per nome o per project label di docker-compose)
        if !cname.starts_with(&docker_prefix1)
            && !cname.starts_with(&docker_prefix2)
            && !cname.contains(slug)
        {
            continue;
        }
        // Esempio porte: "0.0.0.0:5215->8080/tcp, [::]:5215->8080/tcp"
        for entry in parts[1].split(',') {
            if let Some(voce) =
                docker_published_port_entry(entry, cname, &docker_prefix1, &docker_prefix2)
            {
                out.push(voce);
            }
        }
    }
    out
}

/// Rilevazione porte live su Windows. I servizi sono processi gestiti
/// (agent_processes, kind='service'): deriva le porte in ascolto mappando gli
/// OwningProcess delle socket LISTEN sui pid dei servizi del progetto (o loro
/// discendenti, es. node/vite figli di `npm run dev`). Punto unico liveness:
/// process_util::process_alive. Nessun systemctl/`/proc`.
#[cfg(windows)]
async fn detect_project_ports_windows(
    project_id: Uuid,
    db: &sqlx::PgPool,
) -> Vec<serde_json::Value> {
    use std::collections::{HashMap, HashSet};

    // 1. pid dei servizi vivi del progetto (agent_processes migrata -> pool progetto).
    //    DB progetto non disponibile -> nessuna porta rilevabile (WARN, best-effort).
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "detect_project_ports_windows: DB progetto non disponibile, nessuna porta rilevata"
            );
            return Vec::new();
        }
    };
    let svc_rows: Vec<(Option<i32>, String)> = sqlx::query_as(
        "SELECT pid, label FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service' \
           AND status IN ('running', 'starting') AND pid IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();

    let mut svc_pid_label: HashMap<u32, String> = HashMap::new();
    for (pid, label) in svc_rows {
        if let Some(p) = pid {
            if p > 0 && crate::process_util::process_alive(p as u32) {
                svc_pid_label.insert(p as u32, label);
            }
        }
    }
    // NB: `svc_pid_label` vuoto NON e' piu' un'uscita anticipata. Il pass 2 sulle
    // allocazioni (sotto) e' una fonte AUTONOMA basata sul segnale strutturato
    // "porta allocata + in LISTEN" (regola M): con tutti i servizi `failed` ma la
    // porta ancora tenuta da un processo orfano/figlio, il pannello deve mostrarla
    // lo stesso. L'early-return precedente la faceva sparire.

    // 2. Mappa figlio->genitore (Win32_Process) per risalire dal pid in ascolto
    //    (node/vite) fino al pid del servizio (npm/pnpm).
    let child_to_parent = windows_process_parents().await;

    // 3. Socket TCP in ascolto: (porta, owning_pid).
    let listening = windows_listening_ports().await;

    let mut ports: Vec<serde_json::Value> = Vec::new();
    let mut seen: HashSet<u16> = HashSet::new();
    for (port, pid, _program) in &listening {
        let (port, pid) = (*port, *pid);
        let Some(service) = resolve_service_ancestor(pid, &svc_pid_label, &child_to_parent) else {
            continue;
        };
        if !seen.insert(port) {
            continue;
        }
        ports.push(json!({
            "port": port,
            "label": service,
            "pid": pid,
            "state": "LISTEN",
            "url": format!("http://localhost:{port}"),
            "service": service,
        }));
    }

    // Pass 2 (robustezza link — regola L + M). L'albero Win32 puo' NON collegare il
    // listener al servizio (dev-server detached, catena parent spezzata): la porta
    // veniva scartata e il pannello Servizi perdeva il link a un servizio ATTIVO.
    // La mappa AUTORITATIVA porta->servizio e' nexus_port_allocations (META = `db`,
    // regola L): se un servizio VIVO ha una porta allocata realmente in LISTEN
    // (segnale strutturato, regola M), quella e' la sua porta live — a prescindere
    // dall'ancestry. Match servizio<->label via il punto unico similar_service_labels.
    let running_labels: HashSet<String> = svc_pid_label.values().cloned().collect();
    let listening_ports: HashSet<u16> = listening.iter().map(|(p, _, _)| *p).collect();
    let allocs: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port, COALESCE(label, '') FROM nexus_port_allocations \
         WHERE project_id = $1 AND COALESCE(label, '') <> ''",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    for (port, service) in
        allocations_to_live_ports(&running_labels, &listening_ports, &seen, &allocs)
    {
        ports.push(json!({
            "port": port,
            "label": service,
            "state": "LISTEN",
            "url": format!("http://localhost:{port}"),
            "service": service,
        }));
    }
    ports
}

/// Pura (regola L, testabile su ogni piattaforma): dalle allocazioni port->label
/// del progetto ricava le porte LIVE aggiuntive del pass 2 di
/// `detect_project_ports_windows`. Una porta e' live sse: (a) e' realmente in
/// LISTEN (`listening_ports`, segnale strutturato — regola M), (b) non gia' emessa
/// dal pass 1 (`already_seen`), (c) e' nel range Nexus.
///
/// Il LABEL preferisce un servizio VIVO con nome simile (`running_labels`, punto
/// unico `similar_service_labels`), ma NON e' piu' un requisito: se nessun
/// servizio vivo combacia (il servizio e' `failed`/`stopped` in agent_processes
/// mentre il suo processo tiene ancora la porta — orfano, o il pid registrato e'
/// il wrapper morto mentre il figlio vive) si usa il LABEL DELL'ALLOCAZIONE.
/// La porta in ascolto e' la prova strutturale che il progetto sta servendo li'
/// (regola M): scartarla perche' lo stato registrato dice "failed" faceva sparire
/// dal pannello Porte servizi realmente attivi (incidente vendita-immobile
/// 21/07: frontend in LISTEN su 39804, "failed" in agent_processes, invisibile).
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn allocations_to_live_ports(
    running_labels: &std::collections::HashSet<String>,
    listening_ports: &std::collections::HashSet<u16>,
    already_seen: &std::collections::HashSet<u16>,
    allocs: &[(i32, String)],
) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut emitted: std::collections::HashSet<u16> = std::collections::HashSet::new();
    for (port_i, label) in allocs {
        let Ok(port) = u16::try_from(*port_i) else {
            continue;
        };
        // Solo porte del range Nexus (20000-39999): un'allocazione spuria su una
        // porta infrastrutturale (es. 5434 = cluster Postgres, auto-rilevata
        // dall'output del backend) NON deve attribuire il listener ESTRANEO a un
        // servizio del progetto solo perche' la label e' simile.
        if !(PROJECT_PORT_RANGE_START..=PROJECT_PORT_RANGE_END).contains(&port) {
            continue;
        }
        if !listening_ports.contains(&port) || already_seen.contains(&port) || !emitted.insert(port)
        {
            continue;
        }
        // Selezione DETERMINISTICA del servizio proprietario: una label che combacia
        // con piu' servizi vivi non mutuamente simili (es. "api-web" ~ "api-server"
        // E ~ "web-ui") altrimenti oscillerebbe tra i polling (HashSet iter order),
        // facendo lampeggiare il link. Ordine: uguaglianza esatta case-insensitive,
        // poi piu' parole significative in comune, infine lessicografico. Punto unico
        // similar/shared in agent_processes (regola L).
        let mut candidates: Vec<&String> = running_labels
            .iter()
            .filter(|rl| crate::agent_processes::similar_service_labels(rl, label))
            .collect();
        candidates.sort_by(|a, b| {
            let (sa, sb): (&str, &str) = (a.as_str(), b.as_str());
            let lab = label.trim();
            let exact = |s: &str| s.trim().eq_ignore_ascii_case(lab);
            exact(sb)
                .cmp(&exact(sa))
                .then_with(|| {
                    crate::agent_processes::shared_significant_words(sb, label)
                        .cmp(&crate::agent_processes::shared_significant_words(sa, label))
                })
                .then_with(|| sa.cmp(sb))
        });
        // Label del servizio vivo se c'e', altrimenti il label dell'allocazione.
        let service = candidates
            .first()
            .map(|s| (*s).clone())
            .unwrap_or_else(|| label.clone());
        out.push((port, service));
    }
    out
}

/// Risale la catena dei genitori da `pid` finche' trova un pid servizio. Cap a
/// 12 hop per robustezza contro cicli/pid riusati.
#[cfg(windows)]
fn resolve_service_ancestor(
    pid: u32,
    svc_pid_label: &std::collections::HashMap<u32, String>,
    child_to_parent: &std::collections::HashMap<u32, u32>,
) -> Option<String> {
    let mut current = pid;
    for _ in 0..12 {
        if let Some(label) = svc_pid_label.get(&current) {
            return Some(label.clone());
        }
        match child_to_parent.get(&current) {
            Some(&parent) if parent != current && parent != 0 => current = parent,
            _ => break,
        }
    }
    None
}

/// Socket TCP in ascolto via `Get-NetTCPConnection` → (porta, pid, programma).
///
/// PUNTO UNICO Windows (regola L) per "chi ascolta su quale porta": usato sia
/// dal pannello Servizi sia da `port_recovery::listening_ports` (che prima del
/// dispatch #[cfg(windows)] ritornava SEMPRE vuoto su Windows, lasciando ciechi
/// try_free_port / scan_bucket_orphans / guardia RefuseActive — incidente
/// Beaty-Book 2026-07-02, node orfani + EADDRINUSE ricorrente). Il nome
/// programma (Get-Process, una sola enumerazione) serve alle euristiche
/// `looks_like_server_process` per adozione/cleanup orfani.
#[cfg(windows)]
pub(crate) async fn windows_listening_ports() -> Vec<(u16, u32, String)> {
    // Syscall (GetExtendedTcpTable + Toolhelp32) invece di due interpreti
    // PowerShell: misurati 6.7s per `Get-NetTCPConnection`+`Get-Process`, contro
    // millisecondi qui. Il costo non era un dettaglio di efficienza: il
    // `port_enforcer` scandisce ogni 5s con timeout 10s, e quei probe da soli
    // (9.9s in due) mandavano in timeout OGNI iterazione -- l'enforcement delle
    // porte non e' mai girato (33 "iterazione abortita" nei log del 26/07).
    tokio::task::spawn_blocking(|| {
        let processi = crate::process_util::windows_process_snapshot();
        crate::process_util::windows_listening_sockets()
            .into_iter()
            .map(|(porta, pid)| {
                let nome = processi
                    .get(&pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                (porta, pid, nome)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Mappa figlio->genitore di tutti i processi, proiettata dalla fotografia
/// Toolhelp32 (punto unico `process_util::windows_process_snapshot`).
///
/// Prima lanciava `Get-CimInstance Win32_Process` in PowerShell: 3.2s misurati
/// a invocazione, pagati a ogni scansione del port_enforcer (ogni 5s) e a ogni
/// rilevazione servizi. Vedi il commento in `windows_listening_ports`.
#[cfg(windows)]
async fn windows_process_parents() -> std::collections::HashMap<u32, u32> {
    tokio::task::spawn_blocking(|| {
        crate::process_util::windows_process_snapshot()
            .into_iter()
            .map(|(pid, entry)| (pid, entry.parent_pid))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Legge porte TCP in ascolto via `ss -tlnp` → Vec<(port, pid, program)>
pub async fn read_listening_ports_ss() -> anyhow::Result<Vec<(u16, u32, String)>> {
    let output = tokio::process::Command::new("ss")
        .args(["-tlnp"])
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result = Vec::new();
    for line in stdout.lines().skip(1) {
        // Esempio: LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:(("node",pid=1234,fd=5))
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let local_addr = parts.get(3).unwrap_or(&"");
        let port: u16 = local_addr
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        if port == 0 {
            continue;
        }
        // Estrai pid e program da users:(("program",pid=NNN,fd=N))
        let users_str = parts[4..].join(" ");
        let pid = users_str
            .split("pid=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let program = users_str.split('"').nth(1).unwrap_or("").to_string();
        if pid > 0 {
            result.push((port, pid, program));
        }
    }
    Ok(result)
}

/// Fallback: legge /proc/net/tcp e /proc/net/tcp6 → Vec<(port, pid, program)>
/// Mappa inode socket -> porta per le sole socket in LISTEN, leggendo una riga di
/// `/proc/net/tcp{,6}`. `None` se la riga e' malformata, non in LISTEN, o senza
/// porta/inode validi. Estratta da `read_listening_ports_proc`; il formato e'
/// quello del kernel: `parts[1]` = local_address `HEXADDR:HEXPORT`, `parts[3]` =
/// stato (`0A` = LISTEN), `parts[9]` = inode.
fn proc_net_tcp_listen_entry(line: &str) -> Option<(u64, u16)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 10 {
        return None;
    }
    // stato 0A = LISTEN
    if parts[3] != "0A" {
        return None;
    }
    // local_address es. 00000000:0BB8
    let port = u16::from_str_radix(parts[1].split(':').nth(1).unwrap_or("0"), 16).unwrap_or(0);
    let inode: u64 = parts[9].parse().unwrap_or(0);
    if port == 0 || inode == 0 {
        return None;
    }
    Some((inode, port))
}

/// Inode socket -> porta di tutte le socket TCP/TCP6 in LISTEN.
/// Estratta da `read_listening_ports_proc` (fase 1); un file illeggibile viene
/// saltato, come nel codice inline che sostituisce.
fn proc_net_tcp_listen_inodes() -> std::collections::HashMap<u64, u16> {
    let mut inode_to_port: std::collections::HashMap<u64, u16> = std::collections::HashMap::new();
    for path in &["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            if let Some((inode, port)) = proc_net_tcp_listen_entry(line) {
                inode_to_port.insert(inode, port);
            }
        }
    }
    inode_to_port
}

/// Porte in ascolto del processo `pid`, risolvendo i suoi fd socket
/// (`/proc/{pid}/fd/* -> "socket:[inode]"`) contro `inode_to_port`. Estratta da
/// `read_listening_ports_proc` (fase 2); il `program` resta vuoto come prima.
fn proc_listening_ports_of_pid(
    pid: u32,
    inode_to_port: &std::collections::HashMap<u64, u16>,
    out: &mut Vec<(u16, u32, String)>,
) {
    let fd_dir = format!("/proc/{}/fd", pid);
    let Ok(fds) = std::fs::read_dir(&fd_dir) else {
        return;
    };
    for fd in fds.flatten() {
        let Ok(target) = std::fs::read_link(fd.path()) else {
            continue;
        };
        let t = target.to_string_lossy();
        // "socket:[12345]"
        let Some(inode_str) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) else {
            continue;
        };
        if let Ok(inode) = inode_str.parse::<u64>() {
            if let Some(&port) = inode_to_port.get(&inode) {
                out.push((port, pid, String::new()));
            }
        }
    }
}

pub fn read_listening_ports_proc() -> Vec<(u16, u32, String)> {
    let inode_to_port = proc_net_tcp_listen_inodes();

    // Mappa inode → pid via /proc/{pid}/fd/*
    let mut result = Vec::new();
    if let Ok(proc_entries) = std::fs::read_dir("/proc") {
        for entry in proc_entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let Ok(pid) = name_str.parse::<u32>() else {
                continue;
            };
            proc_listening_ports_of_pid(pid, &inode_to_port, &mut result);
        }
    }
    result
}

// Punto unico bucket/porte riservate: vive in nexus-tool-kit::ports
// (split 7.4 fase B: sandbox.rs, ora nel crate, ne ha bisogno). Il
// re-export mantiene validi i path project_workspace::services::* storici.
pub use nexus_tool_kit::ports::{
    port_in_project_bucket, project_bucket_range, NEXUS_RESERVED_PORTS,
    PROJECT_PORT_BUCKET_SIZE, PROJECT_PORT_RANGE_END, PROJECT_PORT_RANGE_START,
};

fn stable_hash_u16(input: &str) -> u16 {
    // FNV-1a 32-bit, then fold to u16. Stable across runs/platforms.
    let mut h: u32 = 2166136261;
    for b in input.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h ^ (h >> 16)) as u16
}

/// Trova una porta libera *nel bucket deterministico* del progetto.
/// INVARIANTE (regola H): la porta ritornata resta SEMPRE nel bucket del
/// progetto. Mai allocare fuori bucket: il `port_enforcer` ammette solo porte nel
/// bucket del progetto (o allocate esplicitamente) e ucciderebbe il processo che
/// binda una porta fuori bucket, producendo esattamente il sintomo "porta non
/// ammissibile". In caso (estremo) di bucket saturo si ritorna la base del bucket
/// con WARN, mai una porta fuori range.
pub(super) async fn find_free_project_port(
    project_id: &Uuid,
    registry: &crate::port_registry::PortRegistryCache,
) -> u16 {
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let allocated: std::collections::HashSet<u16> = registry
        .current()
        .await
        .all_allocated_ports()
        .into_iter()
        .collect();

    let (start, end) = project_bucket_range(project_id);

    let mut port = start;
    while port <= end {
        if !reserved.contains(&port)
            && !allocated.contains(&port)
            && tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
                .await
                .is_ok()
        {
            return port;
        }
        port += 1;
    }

    // Bucket saturo: NON uscire dal bucket. Una porta fuori bucket verrebbe
    // rifiutata e uccisa dal port_enforcer (ammette solo porte nel bucket del
    // progetto o allocate esplicitamente) -> proprio il sintomo "porta non
    // ammissibile". Inoltre uscire dal bucket innescava un effetto domino:
    // il fallback partiva da PROJECT_PORT_RANGE_START (20000) e "rubava" porte
    // ai bucket di altri progetti, facendoli sembrare pieni a loro volta.
    // Manteniamo l'invariante 1 progetto = 1 bucket: ritorniamo la base del
    // bucket con WARN. Scenario estremo (50 servizi nello stesso bucket);
    // l'eventuale bind-fail sara' visibile ma la porta resta autorizzata.
    tracing::warn!(
        project_id = %project_id,
        bucket_start = start,
        bucket_end = end,
        "find_free_project_port: bucket progetto saturo, nessuna porta libera nel bucket; ritorno la base del bucket (resta nel range autorizzato)"
    );
    start
}

/// Porta deterministica per un dato servizio/config all'interno del bucket del progetto.
/// Usa `service_key` come input stabile (es. label o short name) e linear-probing nel bucket.
pub(super) async fn deterministic_project_port_for_key(
    project_id: &Uuid,
    service_key: &str,
    registry: &crate::port_registry::PortRegistryCache,
) -> u16 {
    let (start, end) = project_bucket_range(project_id);
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let allocated: std::collections::HashSet<u16> = registry
        .current()
        .await
        .all_allocated_ports()
        .into_iter()
        .collect();

    let mut offset = stable_hash_u16(service_key) % PROJECT_PORT_BUCKET_SIZE;
    let mut tries: u16 = 0;
    while tries < PROJECT_PORT_BUCKET_SIZE {
        let port = start.saturating_add(offset);
        if port >= start
            && port <= end
            && !reserved.contains(&port)
            && !allocated.contains(&port)
            && tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
                .await
                .is_ok()
        {
            return port;
        }
        offset = (offset + 1) % PROJECT_PORT_BUCKET_SIZE;
        tries += 1;
    }
    find_free_project_port(project_id, registry).await
}

/// Restituisce true se lo script npm/pnpm/yarn è probabilmente un web server
/// (quindi ha bisogno di una porta).
pub(crate) fn is_web_service_script(script_name: &str) -> bool {
    matches!(script_name, "dev" | "start" | "serve" | "preview")
}

/// Prima di avviare un servizio systemd, estrae le porte dal file .service
/// (Environment= e ExecStart) e libera quelle occupate da processi estranei
/// (inclusi container Docker). Ritorna le porte effettivamente liberate.
///
/// `pub(super)` per essere richiamata dal punto unico `service_manager`
/// (SystemdUserBackend) che ne preserva il pre-start su Linux (regola L).
pub(super) async fn free_ports_for_unit(unit_name: &str) -> Vec<serde_json::Value> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let unit_path = format!("{}/.config/systemd/user/{}", home, unit_name);
    let unit_content = match tokio::fs::read_to_string(&unit_path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let ports = extract_ports_from_unit(&unit_content);
    if ports.is_empty() {
        return Vec::new();
    }

    let own_pid = get_service_main_pid(unit_name).await;

    let listening = match read_listening_ports_ss().await {
        Ok(v) => v,
        Err(_) => tokio::task::spawn_blocking(read_listening_ports_proc)
            .await
            .unwrap_or_default(),
    };

    let mut freed = Vec::new();
    for target_port in &ports {
        for &(port, pid, ref program) in &listening {
            if port != *target_port {
                continue;
            }
            if pid == 0 {
                continue;
            }
            if Some(pid) == own_pid {
                continue;
            }
            // Terminazione via punto unico cross-platform (regola L): TERM+KILL
            // con ricontrollo liveness su Unix, taskkill /T /F su Windows.
            crate::process_util::kill_pid(pid).await;
            freed.push(json!({
                "port": port,
                "pid": pid,
                "program": program,
                "method": "kill",
            }));
            tracing::info!(
                "Porta {} liberata: terminato PID {} ({}) per avvio {}",
                port,
                pid,
                program,
                unit_name
            );
        }

        // Container Docker che occupano questa porta
        if let Ok(docker_out) = tokio::process::Command::new("docker")
            .args(["ps", "--format", "{{.Names}}|{{.Ports}}"])
            .output()
            .await
        {
            let docker_str = String::from_utf8_lossy(&docker_out.stdout);
            for line in docker_str.lines() {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if parts.len() != 2 {
                    continue;
                }
                let cname = parts[0].trim();
                let port_section = parts[1];
                let occupies_port = port_section.split(',').any(|entry| {
                    if let Some(arrow_pos) = entry.find("->") {
                        let host_part = &entry[..arrow_pos];
                        host_part
                            .rsplit(':')
                            .next()
                            .and_then(|p| p.trim().parse::<u16>().ok())
                            == Some(*target_port)
                    } else {
                        false
                    }
                });
                if occupies_port {
                    let _ = tokio::process::Command::new("docker")
                        .args(["stop", "-t", "5", cname])
                        .output()
                        .await;
                    freed.push(json!({
                        "port": target_port,
                        "container": cname,
                        "method": "docker stop",
                    }));
                    tracing::info!(
                        "Porta {} liberata: fermato container Docker '{}' per avvio {}",
                        target_port,
                        cname,
                        unit_name
                    );
                }
            }
        }
    }
    freed
}

/// Estrae il numero di porta dalla coda di una stringa host:porta (es.
/// `http://+:5215`, `0.0.0.0:5215`): prende le cifre ASCII subito dopo l'ultimo
/// `:`. Ritorna None se non c'e' porta valida (> 0).
fn port_after_last_colon(s: &str) -> Option<u16> {
    let colon_pos = s.rfind(':')?;
    let after = &s[colon_pos + 1..];
    let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse::<u16>().ok().filter(|&p| p > 0)
}

/// Estrae le porte da una riga `Environment=...` di un unit file. Riconosce sia
/// la forma diretta (`PORT=5215`) sia gli URL con porta (`ASPNETCORE_URLS=http://+:5215`),
/// eventualmente separati da `;`.
fn ports_from_environment_line(rest: &str, ports: &mut Vec<u16>) {
    // Es: PORT=5215, ASPNETCORE_URLS=http://+:5215, SERVER_PORT=8080
    for segment in rest.split_whitespace() {
        let Some(val) = segment.split('=').nth(1) else {
            continue;
        };
        // Porta diretta (es. PORT=5215)
        if let Ok(p) = val.parse::<u16>() {
            if p > 0 {
                ports.push(p);
                continue;
            }
        }
        // URL con porta (es. http://+:5215 o http://0.0.0.0:5215)
        for part in val.split(';') {
            if let Some(p) = port_after_last_colon(part) {
                ports.push(p);
            }
        }
    }
}

/// Estrae le porte da una riga `ExecStart=...` di un unit file. Riconosce
/// `--port 5215`, `-p 5215`, `--server.port 5215`, `--urls http://+:5215` e la
/// forma inline `--port=5215` / `-p=5215`.
fn ports_from_exec_start_line(rest: &str, ports: &mut Vec<u16>) {
    // Pattern: --port 5215, -p 5215, --urls http://+:5215
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1);
        if matches!(*tok, "--port" | "-p" | "--server.port") {
            if let Some(p) = next.and_then(|v| v.parse::<u16>().ok()).filter(|&p| p > 0) {
                ports.push(p);
            }
        }
        if *tok == "--urls" {
            if let Some(p) = next.and_then(|v| port_after_last_colon(v)) {
                ports.push(p);
            }
        }
        // --port=5215
        if tok.starts_with("--port=") || tok.starts_with("-p=") {
            if let Some(p) = tok
                .split('=')
                .nth(1)
                .and_then(|v| v.parse::<u16>().ok())
                .filter(|&p| p > 0)
            {
                ports.push(p);
            }
        }
    }
}

pub(super) fn extract_ports_from_unit(content: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Environment=") {
            ports_from_environment_line(rest, &mut ports);
        }
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            ports_from_exec_start_line(rest, &mut ports);
        }
    }
    ports.sort();
    ports.dedup();
    ports
}

// ── Diagnostica crash-loop ──────────────────────────────────────────────────

struct ServiceDiagnosis {
    error: String,
    suggestion: String,
    kind: &'static str,
}

/// Diagnosi per "modulo Node.js non trovato": distingue tra dipendenze del tutto
/// assenti e un singolo modulo mancante controllando la presenza di `node_modules`
/// (nella root o in una sotto-dir). Estratto da `diagnose_service_failure`.
fn diagnose_missing_node_module(root: &std::path::Path) -> ServiceDiagnosis {
    // Verifica se node_modules esiste
    let has_node_modules = root.join("node_modules").exists()
        || std::fs::read_dir(root)
            .ok()
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().is_dir() && e.path().join("node_modules").exists())
            })
            .unwrap_or(false);
    let suggestion = if has_node_modules {
        "Un modulo non e' installato. Esegui 'npm install' o 'pnpm install' nel terminale del progetto."
    } else {
        "Le dipendenze non sono installate. Esegui 'npm install' nel terminale del progetto prima di avviare il servizio."
    };
    ServiceDiagnosis {
        error: "Modulo Node.js non trovato — dipendenze mancanti".into(),
        suggestion: suggestion.into(),
        kind: "missing_dependencies",
    }
}

fn diagnose_service_failure(log: &str, _unit: &str, root: &std::path::Path) -> ServiceDiagnosis {
    let log_lc = log.to_lowercase();

    // 1. Script npm mancante
    if log_lc.contains("missing script:") {
        let script = log
            .lines()
            .find(|l| l.to_lowercase().contains("missing script:"))
            .and_then(|l| l.split('"').nth(1))
            .unwrap_or("sconosciuto");
        return ServiceDiagnosis {
            error: format!("Lo script npm '{}' non esiste nel package.json", script),
            suggestion: "Verifica che il package.json contenga lo script corretto nella sezione \"scripts\". Prova a eseguire 'Rianalizza progetto' dal pannello Source Control.".into(),
            kind: "missing_script",
        };
    }

    // 2. Directory di lavoro non trovata
    if log_lc.contains("changing to the requested working directory failed")
        || log_lc.contains("no such file or directory")
        || log_lc.contains("chdir")
    {
        return ServiceDiagnosis {
            error: "La directory di lavoro del servizio non esiste".into(),
            suggestion: "Il progetto potrebbe essere incompleto. Disinstalla il servizio e usa '+ Configura' per ricrearlo dopo aver verificato la struttura del progetto.".into(),
            kind: "missing_directory",
        };
    }

    // 3. Dipendenze node mancanti
    if log_lc.contains("cannot find module")
        || log_lc.contains("module not found")
        || log_lc.contains("err_module_not_found")
    {
        return diagnose_missing_node_module(root);
    }

    // 4-7. Euristiche a diagnosi statica (SDK .NET, build .NET, porta occupata,
    // permessi): raggruppate in un helper per tenere questa funzione sotto soglia.
    if let Some(d) = diagnose_static_heuristics(&log_lc) {
        return d;
    }

    // 8. Fallback: nessuna euristica ha matchato, riassumi le righe di errore.
    diagnose_service_failure_fallback(log)
}

/// Euristiche di diagnosi a esito statico (nessuna ispezione del filesystem):
/// SDK .NET mancante, build .NET fallita, porta occupata, permessi insufficienti.
/// Ritorna la prima che matcha su `log_lc` (gia' in minuscolo), None altrimenti.
/// Estratto da `diagnose_service_failure` (comportamento e ordine invariati).
fn diagnose_static_heuristics(log_lc: &str) -> Option<ServiceDiagnosis> {
    // 4. SDK .NET mancante
    if log_lc.contains("dotnet")
        && (log_lc.contains("not found") || log_lc.contains("command not found"))
    {
        return Some(ServiceDiagnosis {
            error: "Il .NET SDK non e' installato o non e' nel PATH".into(),
            suggestion: "Installa il .NET SDK con 'sudo apt install dotnet-sdk-9.0' oppure usa la versione Docker del servizio.".into(),
            kind: "missing_sdk",
        });
    }

    // 5. Build .NET fallita
    if log_lc.contains("build failed") || log_lc.contains("msbuild") && log_lc.contains("error") {
        return Some(ServiceDiagnosis {
            error: "La build .NET e' fallita".into(),
            suggestion: "Esegui 'dotnet build' manualmente nel terminale per vedere gli errori di compilazione.".into(),
            kind: "build_failed",
        });
    }

    // 6. Porta occupata
    if log_lc.contains("address already in use") || log_lc.contains("eaddrinuse") {
        return Some(ServiceDiagnosis {
            error: "La porta richiesta e' gia' occupata da un altro processo".into(),
            suggestion: "Usa il pulsante 'X Porte' per liberare le porte conflittuali, poi riavvia il servizio.".into(),
            kind: "port_in_use",
        });
    }

    // 7. Permessi insufficienti
    if log_lc.contains("permission denied") || log_lc.contains("eacces") {
        return Some(ServiceDiagnosis {
            error: "Permessi insufficienti per eseguire il servizio".into(),
            suggestion: "Verifica i permessi dei file del progetto. Potresti dover eseguire 'chmod +x' sul file eseguibile.".into(),
            kind: "permission_denied",
        });
    }

    None
}

/// Riassunto diagnostico di fallback quando nessuna euristica specifica matcha:
/// mostra fino a 3 righe di errore (o le ultime 3 righe se non ce ne sono).
/// Estratto da `diagnose_service_failure` per tenerla sotto soglia (comportamento
/// invariato).
fn diagnose_service_failure_fallback(log: &str) -> ServiceDiagnosis {
    let last_lines: Vec<&str> = log
        .lines()
        .filter(|l| {
            let ll = l.to_lowercase();
            ll.contains("error")
                || ll.contains("fail")
                || ll.contains("exception")
                || ll.contains("fatal")
                || ll.contains("panic")
        })
        .collect();
    let error_summary = if last_lines.is_empty() {
        log.lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ")
    } else {
        last_lines
            .into_iter()
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ")
    };

    ServiceDiagnosis {
        error: if error_summary.is_empty() {
            "Il servizio si arresta ripetutamente (causa sconosciuta)".into()
        } else {
            error_summary
        },
        suggestion: "Controlla i log completi nel tab Terminale con 'journalctl --user -u <servizio> -n 50'.".into(),
        kind: "unknown",
    }
}

async fn get_service_main_pid(unit_name: &str) -> Option<u32> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit_name, "--property=MainPID"])
        .output()
        .await
        .ok()?;
    // Punto unico del parsing `MainPID=` (regola L).
    parse_main_pid_stdout(&String::from_utf8_lossy(&out.stdout))
}

// ── API Gestione Porte Allocate ──────────────────────────────────────────────

/// GET /api/projects/:id/port-allocations
/// Lista tutte le porte allocate al progetto.
pub async fn get_port_allocations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let allocations = state.port_registry.ports_for_project(&project_id).await;
    let items: Vec<Value> = allocations
        .iter()
        .map(|a| {
            json!({
                "id": a.id.to_string(),
                "port": a.port,
                "label": a.label,
                "allocation_mode": a.allocation_mode,
                "run_config_id": a.run_config_id.map(|u| u.to_string()),
                "service_unit": a.service_unit,
            })
        })
        .collect();

    Ok(Json(json!({ "allocations": items })))
}

/// Campi del body di `create_port_allocation` dopo la validazione. Presta i
/// `&str` dal `Value` di origine: nessuna copia rispetto al codice inline che
/// sostituisce.
struct NewPortAllocation<'a> {
    port: u16,
    label: &'a str,
    mode: &'a str,
    run_config_id: Option<Uuid>,
    service_unit: Option<&'a str>,
}

/// Valida il body di `create_port_allocation`, separando i controlli sull'input
/// dall'effetto sul registry. Ordine dei controlli invariato (presenza `port` ->
/// porte privilegiate -> porte riservate Nexus -> `mode`), stessi status code e
/// stessi messaggi: il primo controllo che fallisce decide la risposta.
fn parse_new_port_allocation(body: &Value) -> Result<NewPortAllocation<'_>, ApiError> {
    let port = body["port"].as_u64().ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Campo 'port' obbligatorio (numero)",
        )
    })? as u16;

    // Validazione range
    if port < 1024 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Porta deve essere >= 1024 (porte privilegiate non ammesse)",
        ));
    }

    // Controllo porte riservate Nexus
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    if reserved.contains(&port) {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!("Porta {} riservata ai servizi interni Nexus", port),
        ));
    }

    let label = body["label"].as_str().unwrap_or("");
    let mode = body["mode"].as_str().unwrap_or("manual");
    if mode != "auto" && mode != "manual" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Campo 'mode' deve essere 'auto' o 'manual'",
        ));
    }

    let run_config_id = body["run_config_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    let service_unit = body["service_unit"].as_str();

    Ok(NewPortAllocation {
        port,
        label,
        mode,
        run_config_id,
        service_unit,
    })
}

/// POST /api/projects/:id/port-allocations
/// Alloca una porta al progetto. Body JSON: { port, label?, mode? "manual"|"auto", run_config_id?, service_unit? }
pub async fn create_port_allocation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let req = parse_new_port_allocation(&body)?;

    match state
        .port_registry
        .allocate(
            project_id,
            req.port,
            req.label,
            req.mode,
            req.run_config_id,
            req.service_unit,
        )
        .await
    {
        Ok(alloc) => Ok(Json(json!({
            "ok": true,
            "allocation": {
                "id": alloc.id.to_string(),
                "port": alloc.port,
                "label": alloc.label,
                "allocation_mode": alloc.allocation_mode,
            }
        }))),
        Err(e) => Err(api_error(StatusCode::CONFLICT, e)),
    }
}

/// Verifica che `port` risulti allocata proprio a `project_id` nel registry.
/// Estratta da `delete_port_allocation` con gli stessi esiti: 403 se la porta e'
/// di un altro progetto, 404 se non e' allocata. Il guard del registry viene
/// rilasciato all'uscita, come il `drop` esplicito che sostituisce.
async fn ensure_port_owned_by_project(
    state: &AppState,
    port: u16,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let registry = state.port_registry.current().await;
    match registry.by_port.get(&port) {
        Some(alloc) if alloc.project_id != project_id => Err(api_error(
            StatusCode::FORBIDDEN,
            "Porta allocata a un altro progetto",
        )),
        Some(_) => Ok(()),
        None => Err(api_error(StatusCode::NOT_FOUND, "Porta non allocata")),
    }
}

/// Termina (best-effort) il processo che binda `port` e marca la sua riga
/// `agent_processes` come stopped. Ritorna `(killed_pid, marked_stopped)`.
/// Estratta da `delete_port_allocation`; invariante di sicurezza invariata: si
/// killa SOLO se il binding e' attribuito a questo progetto, e ogni passo e'
/// best-effort (binding non rilevabile o porta non trovata -> nessun kill).
async fn kill_project_port_binding(
    state: &AppState,
    port: u16,
    project_id: Uuid,
) -> (Option<u32>, bool) {
    let Ok(bindings) = detect_all_port_bindings(&state.db).await else {
        return (None, false);
    };
    let Some(binding) = bindings.iter().find(|b| b.port == port) else {
        return (None, false);
    };
    // Killa solo se il binding e' associato a questo progetto (sicurezza)
    if binding.project_id != Some(project_id) {
        return (None, false);
    }
    let pid = binding.pid;
    // Terminazione via punto unico cross-platform (regola L): su Unix
    // TERM grazioso + KILL se ancora vivo dopo l'attesa incapsulata;
    // su Windows taskkill /T /F. Il precedente `kill` inline era no-op
    // su Windows -> la "x" del pannello non liberava la porta.
    crate::process_util::kill_pid(pid).await;
    // Marca agent_processes come stopped (riconciliazione best-effort: il kill
    // e' gia' avvenuto, un DB progetto non disponibile degrada con WARN senza
    // far fallire la richiesta).
    // Separazione DB per-progetto: agent_processes e' migrata, instrada
    // sul pool del progetto.
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "delete_port_allocation: DB progetto non disponibile, salto la riconciliazione agent_processes"
                );
                return (Some(pid), false);
            }
        };
    let upd = sqlx::query(
        "UPDATE agent_processes SET status='stopped', stopped_at=NOW() \
         WHERE pid = $1 AND project_id = $2 AND status IN ('running','starting')",
    )
    .bind(pid as i32)
    .bind(project_id)
    .execute(&proj_pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    (Some(pid), upd > 0)
}

/// DELETE /api/projects/:id/port-allocations/:port
/// Rilascia una porta allocata al progetto E termina il processo che la usa.
///
/// Comportamento:
/// 1. Verifica ownership (porta allocata a questo progetto)
/// 2. Trova il PID che binda la porta tramite `ss -tlnp`
/// 3. Se il PID appartiene a un `agent_processes` del progetto: SIGTERM (2s)
///    + SIGKILL + marca `status='stopped'` in DB
/// 4. Rilascia l'allocazione dal registry
///
/// Senza il kill, l'utente vede la porta "ricomparire" perche' il processo
/// e' ancora vivo e il detect la rileva di nuovo.
pub async fn delete_port_allocation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, port_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let _project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let port: u16 = port_str
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Porta non valida"))?;

    // Verifica che la porta appartenga effettivamente al progetto
    ensure_port_owned_by_project(&state, port, _project_id).await?;

    // ── Termina il processo che binda la porta (best-effort) ────────────────
    // Senza questo, il processo continuerebbe a girare e il prossimo detect
    // ricreerebbe l'allocazione: l'utente vede "la × non pulisce".
    let (killed_pid, marked_stopped) = kill_project_port_binding(&state, port, _project_id).await;

    state
        .port_registry
        .release(port)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(json!({
        "ok": true,
        "killed_pid": killed_pid,
        "marked_stopped": marked_stopped,
    })))
}

// ── Port binding globale per port_enforcer ──────────────────────────────────

/// Un binding di rete rilevato: porta TCP in LISTEN con PID e (opzionalmente)
/// il `project_id` associato via `agent_processes`.
#[derive(Debug, Clone)]
pub struct PortBinding {
    pub port: u16,
    pub pid: u32,
    pub program: String,
    pub project_id: Option<uuid::Uuid>,
}

/// Mappa pid -> project_id dei processi `running`/`starting` in `agent_processes`.
///
/// Separazione DB: agent_processes e' migrata per-progetto -> la vista
/// globale si ottiene aggregando i DB progetto (stesso pattern delle
/// viste admin globali, regola L). `db` resta il META: serve per
/// l'elenco progetti e la risoluzione dei pool. Sul meta la tabella e'
/// vuota a flag ON: la mappa usciva vuota e l'enforcement porte e il
/// kill dal pannello Porte non scattavano MAI. Un DB progetto
/// irraggiungibile degrada con WARN senza azzerare gli altri.
///
/// Estratta da `detect_all_port_bindings` (blocco 2); comportamento invariato.
async fn pid_to_project_from_agent_processes(
    db: &sqlx::PgPool,
) -> std::collections::HashMap<u32, uuid::Uuid> {
    let mut pid_rows: Vec<(Option<i32>, uuid::Uuid)> = Vec::new();
    for proj in crate::project_db_routes::list_all_project_ids(db).await {
        // Un DB progetto irraggiungibile degrada con WARN senza azzerare gli altri.
        let pool = match crate::project_db_routes::project_data_pool_from(db, proj).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %proj,
                    error = %e,
                    "detect_all_port_bindings: DB progetto non disponibile, salto il progetto"
                );
                continue;
            }
        };
        match sqlx::query_as::<_, (Option<i32>, uuid::Uuid)>(
            "SELECT pid, project_id FROM agent_processes \
             WHERE pid IS NOT NULL AND status IN ('running', 'starting')",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(mut rows) => pid_rows.append(&mut rows),
            Err(e) => tracing::warn!(
                project_id = %proj,
                error = %e,
                "detect_all_port_bindings: query agent_processes fallita per il progetto"
            ),
        }
    }

    let mut pid_to_project: std::collections::HashMap<u32, uuid::Uuid> =
        std::collections::HashMap::new();
    for (pid_opt, proj_id) in &pid_rows {
        if let Some(pid) = pid_opt {
            if *pid > 0 {
                pid_to_project.insert(*pid as u32, *proj_id);
            }
        }
    }
    pid_to_project
}

/// Fallback CWD: per i PID in ascolto senza project_id da `agent_processes`,
/// tenta l'associazione via `/proc/<pid>/cwd` confrontato con
/// `repository_root_path` dei progetti. Cattura processi avviati fuori dal tool
/// system Nexus. Non sovrascrive attribuzioni gia' risolte (`or_insert`).
///
/// Estratta da `detect_all_port_bindings` (blocco 4); comportamento invariato,
/// inclusi gli early-return quando non c'e' nulla da risolvere.
async fn match_unmatched_pids_by_cwd(
    db: &sqlx::PgPool,
    listening: &[(u16, u32, String)],
    pid_to_project: &mut std::collections::HashMap<u32, uuid::Uuid>,
) {
    let unmatched_pids: Vec<u32> = listening
        .iter()
        .filter(|(_, pid, _)| !pid_to_project.contains_key(pid))
        .map(|(_, pid, _)| *pid)
        .collect();
    if unmatched_pids.is_empty() {
        return;
    }

    // Carica mappa root_path -> project_id
    let project_roots: Vec<(uuid::Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, repository_root_path FROM projects \
         WHERE repository_root_path IS NOT NULL AND repository_root_path != ''",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    if project_roots.is_empty() {
        return;
    }

    let roots_clone: Vec<(uuid::Uuid, String)> = project_roots
        .into_iter()
        .filter_map(|(id, r)| r.map(|p| (id, p)))
        .collect();
    let cwd_matches =
        tokio::task::spawn_blocking(move || resolve_pids_by_cwd(&unmatched_pids, &roots_clone))
            .await
            .unwrap_or_default();

    for (pid, proj_id) in cwd_matches {
        pid_to_project.entry(pid).or_insert(proj_id);
    }
}

/// Scansiona tutte le porte TCP in ascolto (`ss -tlnp` o fallback `/proc/net/tcp`)
/// e associa ogni PID al `project_id` corrispondente consultando `agent_processes`.
///
/// Usata dal `port_enforcer` per individuare violazioni: processi di progetto
/// che bindano porte fuori dal bucket assegnato.
///
/// Windows nativo: niente `ss`/`/proc`. La rilevazione usa il percorso dedicato
/// `detect_all_port_bindings_windows` (Get-NetTCPConnection + albero processi
/// Win32). Senza questo dispatch la lista usciva SEMPRE vuota -> il port_enforcer
/// e il kill di `delete_port_allocation`/`cleanup_project_ports` erano ciechi.
#[cfg_attr(windows, allow(unreachable_code))]
pub async fn detect_all_port_bindings(db: &sqlx::PgPool) -> Result<Vec<PortBinding>, String> {
    #[cfg(windows)]
    {
        return detect_all_port_bindings_windows(db).await;
    }

    // 1. Ottieni tutte le porte in ascolto con PID.
    //    `ss -tlnp` via tokio::process (async). Fallback: /proc/net/tcp via spawn_blocking.
    let listening = match read_listening_ports_ss().await {
        Ok(v) => v,
        Err(_) => tokio::task::spawn_blocking(read_listening_ports_proc)
            .await
            .unwrap_or_default(),
    };

    if listening.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Costruisci mappa pid -> project_id da agent_processes (aggregando i DB
    //    progetto: vedi pid_to_project_from_agent_processes).
    let mut pid_to_project = pid_to_project_from_agent_processes(db).await;

    // 3. Espandi con discendenti: scan /proc sincrono, spostato su spawn_blocking
    //    per non bloccare il runtime tokio (fix: freeze mcp-core su molti processi).
    let known_pids: Vec<u32> = pid_to_project.keys().copied().collect();
    let children = tokio::task::spawn_blocking(move || build_children_map(&known_pids))
        .await
        .unwrap_or_default();

    // BFS: propaga project_id dai PID noti ai discendenti (punto unico, regola L).
    propagate_to_descendants(&mut pid_to_project, &children);

    // 4. Fallback CWD per i PID in ascolto ancora senza project_id.
    match_unmatched_pids_by_cwd(db, &listening, &mut pid_to_project).await;

    // 5. Costruisci i PortBinding
    let bindings: Vec<PortBinding> = listening
        .into_iter()
        .map(|(port, pid, program)| PortBinding {
            port,
            pid,
            program,
            project_id: pid_to_project.get(&pid).copied(),
        })
        .collect();

    Ok(bindings)
}

/// Windows: variante di `detect_all_port_bindings` senza `ss`/`/proc`. Porte in
/// ascolto via `windows_listening_ports` (Get-NetTCPConnection, PUNTO UNICO
/// Windows), mappa pid->project_id da `agent_processes` aggregando i DB progetto,
/// e risalita dell'albero processi Win32 (`windows_process_parents`) per associare
/// il pid in ascolto (node/vite) al pid del servizio noto (npm/pnpm) e quindi al
/// progetto. Stessa strategia di `detect_project_ports_windows`.
#[cfg(windows)]
async fn detect_all_port_bindings_windows(db: &sqlx::PgPool) -> Result<Vec<PortBinding>, String> {
    use std::collections::HashMap;

    let listening = windows_listening_ports().await;
    if listening.is_empty() {
        return Ok(Vec::new());
    }

    // pid -> project_id da agent_processes (la tabella e' migrata: aggreghiamo i
    // DB progetto, come fa il ramo Unix). Un DB progetto irraggiungibile degrada
    // senza azzerare gli altri.
    //
    // VALIDAZIONE IDENTITA' del PID (anti-riciclo, punto unico regola L
    // `process_util::pid_identity_confirmed`, lo stesso dell'observer): Windows
    // ricicla i PID in modo aggressivo e le righe 'running' possono essere
    // stantie (crash non ancora sanato, restart di Nexus). Senza il confronto
    // creation-time vs started_at, un PID riciclato su un processo ESTRANEO
    // veniva attribuito al progetto e — tramite la risalita dell'albero processi
    // qui sotto — le sue porte (49664-49671 dei servizi di sistema, 5434 del
    // cluster DB dell'infrastruttura) finivano flaggate e killate come
    // "violazioni porta" del progetto: falsi positivi eterni nel pannello
    // Problemi con detail "processo 'lsass'/'svchost'/'postgres' terminato".
    let mut pid_to_project: HashMap<u32, uuid::Uuid> = HashMap::new();
    for proj in crate::project_db_routes::list_all_project_ids(db).await {
        let Some(pool) = crate::project_db_routes::project_data_pool_or_warn(
            db,
            proj,
            "detect_all_port_bindings_windows",
        )
        .await
        else {
            continue;
        };
        let rows: Vec<(Option<i32>, uuid::Uuid, Option<chrono::DateTime<chrono::Utc>>)> =
            sqlx::query_as(
                "SELECT pid, project_id, started_at FROM agent_processes \
                 WHERE pid IS NOT NULL AND status IN ('running', 'starting')",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
        for (pid_opt, proj_id, started_at) in rows {
            let Some(pid) = pid_opt.filter(|p| *p > 0) else {
                continue;
            };
            let pid = pid as u32;
            if !crate::process_util::pid_identity_confirmed(pid, started_at.map(|t| t.timestamp()))
            {
                continue;
            }
            pid_to_project.insert(pid, proj_id);
        }
    }

    // Albero processi (figlio -> genitore) per risalire dal listener al servizio.
    let child_to_parent = windows_process_parents().await;

    let bindings = listening
        .into_iter()
        .map(|(port, pid, program)| PortBinding {
            port,
            pid,
            program,
            project_id: resolve_project_ancestor(pid, &pid_to_project, &child_to_parent),
        })
        .collect();
    Ok(bindings)
}

/// Risale la catena dei genitori da `pid` finche' trova un pid mappato a un
/// progetto. Cap a 12 hop (robustezza contro cicli/pid riusati). Gemello di
/// `resolve_service_ancestor` ma ritorna il `project_id` invece della label.
#[cfg(windows)]
fn resolve_project_ancestor(
    pid: u32,
    pid_to_project: &std::collections::HashMap<u32, uuid::Uuid>,
    child_to_parent: &std::collections::HashMap<u32, u32>,
) -> Option<uuid::Uuid> {
    let mut current = pid;
    for _ in 0..12 {
        if let Some(proj) = pid_to_project.get(&current) {
            return Some(*proj);
        }
        match child_to_parent.get(&current) {
            Some(&parent) if parent != current && parent != 0 => current = parent,
            _ => break,
        }
    }
    None
}

/// Risolve PID -> project_id leggendo /proc/<pid>/cwd e confrontando con
/// i root_path dei progetti. Operazione sincrona: chiamare da `spawn_blocking`.
fn resolve_pids_by_cwd(
    pids: &[u32],
    project_roots: &[(uuid::Uuid, String)],
) -> Vec<(u32, uuid::Uuid)> {
    let mut results = Vec::new();
    for &pid in pids {
        let cwd_link = format!("/proc/{}/cwd", pid);
        let cwd = match std::fs::read_link(&cwd_link) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        // Trova il progetto il cui root_path e' un prefisso del cwd.
        // Se piu' progetti matchano, prende quello con il path piu' lungo (piu' specifico).
        let mut best: Option<(uuid::Uuid, usize)> = None;
        for (proj_id, root) in project_roots {
            if cwd.starts_with(root.as_str()) || cwd == *root {
                let len = root.len();
                if best.is_none_or(|(_, prev_len)| len > prev_len) {
                    best = Some((*proj_id, len));
                }
            }
        }
        if let Some((proj_id, _)) = best {
            results.push((pid, proj_id));
        }
    }
    results
}

/// Costruisce la mappa padre->figli leggendo /proc/*/status.
/// Operazione puramente sincrona: va chiamata da `spawn_blocking`.
/// Scansiona /proc per costruire: (a) mappa parent→children, (b) insieme di PID
/// il cui cwd e' dentro il project_root dato.
/// Operazione sincrona: chiamare da `spawn_blocking`.
fn scan_proc_children_and_cwd(
    project_root: &str,
) -> (
    std::collections::HashMap<u32, Vec<u32>>,
    std::collections::HashSet<u32>,
) {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let mut cwd_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return (children, cwd_pids),
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let pid = match name_str.parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Leggi PPid da /proc/{pid}/status
        let status_path = format!("/proc/{}/status", pid);
        if let Ok(content) = std::fs::read_to_string(&status_path) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("PPid:") {
                    if let Ok(ppid) = rest.trim().parse::<u32>() {
                        children.entry(ppid).or_default().push(pid);
                    }
                    break;
                }
            }
        }
        // Controlla cwd per associazione al progetto
        let cwd_path = format!("/proc/{}/cwd", pid);
        if let Ok(cwd) = std::fs::read_link(&cwd_path) {
            let cwd_str = cwd.to_string_lossy();
            if cwd_str.starts_with(project_root) {
                cwd_pids.insert(pid);
            }
        }
    }
    (children, cwd_pids)
}

/// Filtra solo i PID che sono discendenti dei `known_pids` per ridurre
/// le letture inutili.
fn build_children_map(_known_pids: &[u32]) -> std::collections::HashMap<u32, Vec<u32>> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return children,
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let pid = match name_str.parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Leggi solo la riga PPid: da /proc/<pid>/status (pochi byte)
        let status_path = format!("/proc/{}/status", pid);
        let content = match std::fs::read_to_string(&status_path) {
            Ok(c) => c,
            Err(_) => continue, // processo scomparso, zombie, ecc.
        };
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("PPid:") {
                if let Ok(ppid) = rest.trim().parse::<u32>() {
                    children.entry(ppid).or_default().push(pid);
                }
                break;
            }
        }
    }
    children
}

/// Verifica se una porta e' allocata ad un progetto specifico in `nexus_port_allocations`.
pub async fn port_allocated_to_project(
    db: &sqlx::PgPool,
    port: u16,
    project_id: uuid::Uuid,
) -> bool {
    // Solo allocazioni MANUAL giustificano una porta fuori dal bucket: le
    // allocazioni auto/dynamic create da agenti AI per processi che hanno
    // bindato porte arbitrarie (es. Vite default 5173) NON devono salvare
    // il processo dal port_enforcer. Altrimenti l'agente puo' aggirare
    // l'isolamento creando un'allocazione dynamic per qualsiasi porta.
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM nexus_port_allocations \
         WHERE port = $1 AND project_id = $2 AND allocation_mode = 'manual')",
    )
    .bind(port as i32)
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocations_to_live_ports_emette_ogni_porta_allocata_in_ascolto() {
        use std::collections::HashSet;
        let running: HashSet<String> = ["frontend".to_string(), "backend".to_string()]
            .into_iter()
            .collect();
        let listening: HashSet<u16> = [31840u16, 31792, 31900].into_iter().collect();
        // 31792 gia' emessa dal pass 1 (albero processi risolto)
        let already_seen: HashSet<u16> = [31792u16].into_iter().collect();
        let allocs = vec![
            (31840i32, "frontend-dev".to_string()), // servizio vivo (frontend ~ frontend-dev) -> label del servizio
            (31792, "backend".to_string()),         // gia' vista dal pass 1 -> saltata
            (31900, "worker".to_string()),          // in ascolto, servizio NON vivo -> label dell'allocazione
            (31999, "frontend".to_string()),        // NON in ascolto -> saltata (nessun listener)
            (70000, "frontend".to_string()),        // fuori range u16 -> saltata
        ];
        let out = allocations_to_live_ports(&running, &listening, &already_seen, &allocs);
        assert_eq!(
            out,
            vec![
                (31840u16, "frontend".to_string()),
                (31900u16, "worker".to_string()),
            ]
        );
    }

    #[test]
    fn merge_ports_view_fonde_registro_e_live() {
        // Live: 39826 (backend, in ascolto con pid/url). Registro: 39826 backend
        // (auto) + 39804 frontend (auto, FERMO, non nel probe).
        let live = vec![json!({
            "port": 39826, "label": "backend", "service": "backend",
            "pid": 26648, "state": "LISTEN", "url": "http://localhost:39826", "live": true
        })];
        let allocs = vec![
            (39826i64, "backend".to_string(), "auto".to_string()),
            (39804i64, "frontend".to_string(), "auto".to_string()),
        ];
        let out = merge_ports_view(live, &allocs);
        assert_eq!(out.len(), 2);
        // Ordinato per porta: 39804 (ferma) prima, 39826 (live) dopo.
        let ferma = &out[0];
        assert_eq!(ferma["port"], 39804);
        assert_eq!(ferma["allocated"], true);
        assert_eq!(ferma["live"], false);
        assert!(ferma.get("url").is_none(), "porta ferma: nessun url linkabile");
        let viva = &out[1];
        assert_eq!(viva["port"], 39826);
        assert_eq!(viva["allocated"], true);
        assert_eq!(viva["live"], true);
        assert_eq!(viva["allocation_mode"], "auto");
        assert_eq!(viva["url"], "http://localhost:39826");
    }

    #[test]
    fn merge_ports_view_porta_live_non_allocata_resta() {
        // Un listener su una porta NON nel registro (raro): resta, allocated=false.
        let live = vec![json!({
            "port": 35500, "label": "extra", "service": "extra",
            "url": "http://localhost:35500", "live": true
        })];
        let out = merge_ports_view(live, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["port"], 35500);
        assert_eq!(out[0]["allocated"], false);
        assert_eq!(out[0]["live"], true);
    }

    #[test]
    fn merge_ports_view_dedup_porta_allocata_due_volte() {
        // Una porta duplicata nel registro non produce due voci.
        let allocs = vec![
            (39804i64, "frontend".to_string(), "auto".to_string()),
            (39804i64, "frontend-old".to_string(), "manual".to_string()),
        ];
        let out = merge_ports_view(vec![], &allocs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["port"], 39804);
        assert_eq!(out[0]["label"], "frontend");
    }

    #[test]
    fn allocations_to_live_ports_mostra_porta_di_servizio_failed_in_ascolto() {
        use std::collections::HashSet;
        // Scenario incidente vendita-immobile 21/07: il frontend e' `failed` in
        // agent_processes (running_labels ha solo "backend"), ma il suo processo
        // node tiene ancora la porta 39804 in LISTEN. Deve comparire nel pannello
        // Porte, col label dell'ALLOCAZIONE, invece di sparire.
        let running: HashSet<String> = ["backend".to_string()].into_iter().collect();
        let listening: HashSet<u16> = [39804u16].into_iter().collect();
        let already_seen: HashSet<u16> = HashSet::new();
        let allocs = vec![(39804i32, "frontend".to_string())];
        let out = allocations_to_live_ports(&running, &listening, &already_seen, &allocs);
        assert_eq!(out, vec![(39804u16, "frontend".to_string())]);
    }

    #[test]
    fn allocations_to_live_ports_dedup_per_porta() {
        use std::collections::HashSet;
        let running: HashSet<String> = ["api".to_string()].into_iter().collect();
        let listening: HashSet<u16> = [31810u16].into_iter().collect();
        let already_seen: HashSet<u16> = HashSet::new();
        // due allocazioni sulla stessa porta: emessa una sola volta
        let allocs = vec![
            (31810i32, "api".to_string()),
            (31810, "api-old".to_string()),
        ];
        let out = allocations_to_live_ports(&running, &listening, &already_seen, &allocs);
        assert_eq!(out, vec![(31810u16, "api".to_string())]);
    }

    #[test]
    fn allocations_to_live_ports_ignora_porte_fuori_range_nexus() {
        use std::collections::HashSet;
        // Regressione (dati live Beaty-Book): un'allocazione spuria su 5434 (cluster
        // Postgres, auto-rilevata dall'output del backend) NON deve attribuire il
        // listener estraneo al servizio del progetto solo perche' la label e' simile.
        let running: HashSet<String> = ["backend-dev".to_string()].into_iter().collect();
        let listening: HashSet<u16> = [5434u16].into_iter().collect(); // Postgres ascolta qui
        let already_seen: HashSet<u16> = HashSet::new();
        let allocs = vec![(5434i32, "backend".to_string())];
        assert!(allocations_to_live_ports(&running, &listening, &already_seen, &allocs).is_empty());
    }

    #[test]
    fn allocations_to_live_ports_selezione_deterministica_su_match_ambiguo() {
        use std::collections::HashSet;
        // Label allocazione multi-parola simile a DUE servizi vivi mutuamente non
        // simili: la scelta deve essere STABILE (niente flicker del link).
        let running: HashSet<String> = ["api-server".to_string(), "web-ui".to_string()]
            .into_iter()
            .collect();
        let listening: HashSet<u16> = [31500u16].into_iter().collect();
        let already_seen: HashSet<u16> = HashSet::new();
        let allocs = vec![(31500i32, "api-web".to_string())];
        // Nessun match esatto, parole condivise pari (1 e 1) -> tie-break
        // lessicografico: "api-server" < "web-ui". Deterministico e idempotente.
        let out = allocations_to_live_ports(&running, &listening, &already_seen, &allocs);
        assert_eq!(out, vec![(31500u16, "api-server".to_string())]);
        let out2 = allocations_to_live_ports(&running, &listening, &already_seen, &allocs);
        assert_eq!(out, out2);
    }

    #[test]
    fn is_project_unit_file_riconosce_solo_le_unit_del_progetto() {
        // Criterio UNICO (regola L) usato sia dall'enumerazione gestiti
        // (list_services_fallback) sia dal marking wizard (mark_existing_services).
        assert!(is_project_unit_file(
            "beauty-book-backend.service",
            "beauty-book"
        ));
        assert!(is_project_unit_file(
            "beauty-book-frontend.service",
            "beauty-book"
        ));
        // Prefisso di un altro progetto: NO.
        assert!(!is_project_unit_file(
            "other-backend.service",
            "beauty-book"
        ));
        // Estensione non .service (timer/socket): NO.
        assert!(!is_project_unit_file(
            "beauty-book-backend.timer",
            "beauty-book"
        ));
        // Manca il separatore '-' dopo lo slug: NO (evita falsi match tra slug uno
        // prefisso dell'altro, es. "beauty-book" vs "beauty-bookshop").
        assert!(!is_project_unit_file(
            "beauty-bookshop-api.service",
            "beauty-book"
        ));
    }

    #[test]
    fn riconosce_servizio_docker_compose() {
        // ExecStart reali generati dal wizard: vanno riconosciuti come compose
        // (one-shot) per leggere lo stato dai container e non dal processo.
        assert!(is_docker_compose_service(
            "/usr/bin/docker compose -f docker-compose.nexus.yml up --build"
        ));
        assert!(is_docker_compose_service(
            "/usr/bin/docker compose -f docker-compose.yml -f docker-compose.nexus.yml up -d"
        ));
        assert!(is_docker_compose_service("docker-compose up"));
    }

    #[test]
    fn non_confonde_altri_servizi() {
        // Servizi applicativi normali: lo stato resta basato sul processo.
        assert!(!is_docker_compose_service("/usr/bin/npm run dev"));
        assert!(!is_docker_compose_service(
            "/home/app/.bin/vite --host 0.0.0.0 --port 39566"
        ));
        // `docker compose` senza `up` (es. ps/logs) non e' un servizio di avvio.
        assert!(!is_docker_compose_service("/usr/bin/docker compose ps"));
    }

    fn riga(
        label: &str,
        status: &str,
        minuti_fa: i64,
    ) -> (String, String, chrono::DateTime<chrono::Utc>) {
        // Base fissa: i test non dipendono dall'orologio di sistema.
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-02T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        (
            label.to_string(),
            status.to_string(),
            base - chrono::Duration::minutes(minuti_fa),
        )
    }

    #[test]
    fn voce_generica_morta_nascosta_running_visibile() {
        // Regressione "Service" fantasma: tentativo storico fallito, non un
        // servizio. Ma se GIRA deve restare visibile (va potuta fermare).
        let rows = vec![
            riga("Service", "failed", 60),
            riga("backend", "running", 10),
        ];
        let visible = super::visible_windows_services(&rows);
        assert_eq!(visible, vec![("backend".to_string(), true)]);

        let rows = vec![riga("Service", "running", 60)];
        let visible = super::visible_windows_services(&rows);
        assert_eq!(visible, vec![("Service".to_string(), true)]);
    }

    #[test]
    fn variante_morta_superseded_da_label_simile_piu_recente() {
        // Regressione doppio vite: "frontend-dev" morta dopo che "frontend"
        // l'ha sostituita non e' un servizio a se', sparisce dal pannello.
        let rows = vec![
            riga("frontend", "running", 5),
            riga("frontend-dev", "stopped", 30),
        ];
        let visible = super::visible_windows_services(&rows);
        assert_eq!(visible, vec![("frontend".to_string(), true)]);
    }

    #[test]
    fn duplicati_entrambi_running_restano_entrambi_visibili() {
        // Finche' girano entrambi l'utente deve poterli vedere e fermare.
        let rows = vec![
            riga("frontend", "running", 5),
            riga("frontend-dev", "running", 30),
        ];
        let visible = super::visible_windows_services(&rows);
        assert_eq!(
            visible,
            vec![
                ("frontend".to_string(), true),
                ("frontend-dev".to_string(), true)
            ]
        );
    }

    #[test]
    fn servizio_fermo_senza_sostituto_resta_visibile() {
        // Un servizio spento ma non superseded va mostrato (va potuto riavviare);
        // conta la riga PIU' RECENTE della label, non le storiche.
        let rows = vec![
            riga("backend", "stopped", 10),
            riga("backend", "running", 60),
            riga("frontend", "running", 5),
        ];
        let visible = super::visible_windows_services(&rows);
        assert_eq!(
            visible,
            vec![
                ("backend".to_string(), false),
                ("frontend".to_string(), true)
            ]
        );
    }

    // Regressione Windows (Wave 1): la risoluzione porta->progetto per il
    // port_enforcer risale l'albero processi dal listener (node/vite) fino al pid
    // del servizio noto. Senza, la lista bindings usciva vuota e l'enforcement era
    // cieco su Windows.
    #[cfg(windows)]
    #[test]
    fn resolve_project_ancestor_risale_al_servizio_noto() {
        use std::collections::HashMap;
        let proj = uuid::Uuid::from_u128(7);
        let mut pid_to_project: HashMap<u32, uuid::Uuid> = HashMap::new();
        pid_to_project.insert(100, proj); // 100 = servizio noto (es. npm/pnpm)
        let mut child_to_parent: HashMap<u32, u32> = HashMap::new();
        child_to_parent.insert(200, 150); // listener node -> figlio intermedio
        child_to_parent.insert(150, 100); // intermedio -> servizio noto

        // Il listener 200 risale la catena fino al servizio 100 -> mappa al progetto.
        assert_eq!(
            super::resolve_project_ancestor(200, &pid_to_project, &child_to_parent),
            Some(proj)
        );
        // Un pid senza antenati noti non mappa a nessun progetto.
        assert_eq!(
            super::resolve_project_ancestor(999, &pid_to_project, &child_to_parent),
            None
        );
        // Cap anti-ciclo: una catena che cicla non manda in loop infinito.
        let mut cyclic: HashMap<u32, u32> = HashMap::new();
        cyclic.insert(1, 2);
        cyclic.insert(2, 1);
        assert_eq!(
            super::resolve_project_ancestor(1, &HashMap::new(), &cyclic),
            None
        );
    }

    #[test]
    fn assign_service_da_allocazione_solo_se_mancante() {
        use std::collections::HashMap;
        // Mappa autoritativa port->servizio (nexus_port_allocations).
        let mut alloc: HashMap<i32, String> = HashMap::new();
        alloc.insert(31776, "backend".to_string());
        alloc.insert(31798, "frontend".to_string());
        let mut ports = vec![
            // Porta live senza service risolto dall'albero processi (dev server
            // orfano): deve ricevere "backend" dall'allocazione -> il pannello
            // mostra il link.
            json!({ "port": 31776, "service": serde_json::Value::Null, "live": true }),
            // Porta live con service GIA' risolto: non va sovrascritta dalla label
            // (la risoluzione dall'albero processi e' piu' specifica).
            json!({ "port": 31798, "service": "frontend-dev", "live": true }),
            // Porta senza allocazione: resta senza service.
            json!({ "port": 31800, "live": true }),
        ];
        super::assign_service_from_allocations(&mut ports, &alloc);
        assert_eq!(ports[0]["service"].as_str(), Some("backend"));
        assert_eq!(ports[1]["service"].as_str(), Some("frontend-dev"));
        assert!(ports[2].get("service").and_then(|v| v.as_str()).is_none());
    }

    #[test]
    fn reconcile_dead_service_rows_marca_stopped_solo_i_running_morti() {
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows = vec![
            // running con pid MORTO -> stopped, pid raccolto
            (
                "backend".to_string(),
                "running".to_string(),
                Some(200),
                base,
                Some(base),
            ),
            // running con pid VIVO e proprio -> invariato
            (
                "frontend".to_string(),
                "running".to_string(),
                Some(100),
                base,
                Some(base),
            ),
            // gia' stopped (pid morto) -> invariato, NON raccolto (non era running)
            (
                "worker".to_string(),
                "stopped".to_string(),
                Some(200),
                base,
                Some(base),
            ),
            // starting senza pid -> non vivo -> stopped, ma nessun pid da persistere
            ("api".to_string(), "starting".to_string(), None, base, None),
        ];
        // Predicato mock: vivo E identita' confermata solo se pid in {100,300} e
        // started_at combacia con `base` (identita' del run). Un pid vivo ma con
        // started_at diverso simula il RICICLO (processo estraneo).
        let alive_confirmed =
            |p: i32, started: Option<chrono::DateTime<chrono::Utc>>| {
                (p == 100 || p == 300) && started == Some(base)
            };
        let (reconciled, dead) = super::reconcile_dead_service_rows(rows, alive_confirmed);
        assert_eq!(
            reconciled[0],
            ("backend".to_string(), "stopped".to_string(), base)
        );
        assert_eq!(
            reconciled[1],
            ("frontend".to_string(), "running".to_string(), base)
        );
        assert_eq!(
            reconciled[2],
            ("worker".to_string(), "stopped".to_string(), base)
        );
        assert_eq!(
            reconciled[3],
            ("api".to_string(), "stopped".to_string(), base)
        );
        // Solo il pid 200 della riga RUNNING va persistito come stopped.
        assert_eq!(dead, vec![200]);
    }

    #[test]
    fn reconcile_dead_service_rows_marca_stopped_il_pid_riciclato() {
        // Regressione coerenza pannello Servizi vs Problemi: un PID ancora VIVO ma
        // RICICLATO dal SO su un processo estraneo (started_at reale != atteso) non
        // deve restare 'running'. Prima il predicato era la sola liveness -> il
        // pannello Servizi mostrava 'running' cio' che l'observer marcava 'failed'.
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows = vec![(
            "backend".to_string(),
            "running".to_string(),
            Some(500),
            base,
            Some(base),
        )];
        // pid 500 e' VIVO (liveness true) ma la sua identita' NON e' confermata
        // (started_at reale diverso): il predicato del caller ritorna false.
        let alive_confirmed = |_p: i32, _started: Option<chrono::DateTime<chrono::Utc>>| false;
        let (reconciled, dead) = super::reconcile_dead_service_rows(rows, alive_confirmed);
        assert_eq!(
            reconciled[0],
            ("backend".to_string(), "stopped".to_string(), base)
        );
        assert_eq!(dead, vec![500]);
    }
}
