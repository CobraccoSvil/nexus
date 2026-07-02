use sqlx::PgPool;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::sandbox::{self, SandboxConfig};

/// PUNTO UNICO (regola L): mappa il `role` di una run configuration
/// (frontend/backend/service/test/tool — mig 0068) sul `kind` della riga
/// `agent_processes`. Solo i processi long-running sono servizi del progetto
/// e compaiono nel pannello Servizi (`list_services_windows`); test e tool
/// sono task one-shot (es. `install-dependencies`, `playwright test`) e non
/// devono restare per sempre nella lista servizi. `None` (config antiche
/// senza role) resta 'service' per non nascondere server custom.
pub fn kind_for_run_config_role(role: Option<&str>) -> &'static str {
    match role {
        Some("test") | Some("tool") => "task",
        _ => "service",
    }
}

/// Parole che da sole non identificano un servizio: usate sia per riconoscere
/// le label generiche ("Service", "server") sia per escluderle dal confronto
/// di similarita' tra label.
const GENERIC_SERVICE_WORDS: &[&str] = &["service", "server", "run", "dev", "start", "app"];

/// Parole significative di una label servizio: lowercase, split su spazi,
/// trattini, underscore e punti ("frontend-dev" -> {"frontend"}), escluse le
/// parole generiche e quelle troppo corte.
fn significant_service_words(label: &str) -> std::collections::HashSet<String> {
    label
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '.')
        .filter(|w| w.len() > 2 && !GENERIC_SERVICE_WORDS.contains(w))
        .map(String::from)
        .collect()
}

/// Una label e' generica quando non ha nessuna parola significativa
/// ("Service", "server", "dev-server"): non identifica uno scopo e non deve
/// diventare una voce autonoma del pannello Servizi.
pub fn is_generic_service_label(label: &str) -> bool {
    significant_service_words(label).is_empty()
}

/// PUNTO UNICO (regola L): due label indicano lo stesso servizio di progetto?
/// Match esatto case-insensitive oppure almeno una parola significativa in
/// comune ("frontend-dev" ~ "frontend", "Backend API" ~ "backend").
pub fn similar_service_labels(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
        || !significant_service_words(a).is_disjoint(&significant_service_words(b))
}

/// PUNTO UNICO (regola L): ferma i processi `kind='service'` running/starting
/// del progetto la cui label indica lo stesso servizio di `label`. Da chiamare
/// PRIMA di ogni spawn di un servizio, da QUALUNQUE call site (tool agente
/// run_service, wizard install, start/restart del pannello, run config):
/// senza questo, ogni percorso di avvio lascia in vita il server precedente e
/// il progetto accumula duplicati sulla stessa codebase (due vite, due backend).
/// Ritorna le label fermate.
pub async fn stop_similar_running_services(
    db: &PgPool,
    project_id: Uuid,
    label: &str,
) -> Vec<String> {
    // agent_processes e' tabella migrata: instrada sul pool del progetto.
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, label FROM agent_processes \
         WHERE project_id = $1 AND kind = 'service' AND status IN ('running','starting')",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .unwrap_or_default();

    let mut stopped = Vec::new();
    for (id, other) in rows {
        if similar_service_labels(label, &other) {
            tracing::info!(
                old_label = %other,
                new_label = %label,
                process_id = %id,
                "servizio duplicato dello stesso scopo fermato prima del nuovo avvio"
            );
            let _ = stop_process(db, project_id, id).await;
            stopped.push(other);
        }
    }
    stopped
}

/// Spawns a background process on the server, captures output into the DB.
/// Returns the process UUID immediately (fire-and-forget for the caller).
///
/// Se `project_root` è `Some` e la sandbox Docker è disponibile, il processo
/// gira in un container isolato (`nexus-sandbox:latest`) con:
/// - solo la directory del progetto montata in lettura/scrittura
/// - nessuna variabile di sistema Nexus (DATABASE_URL, REDIS_URL, …)
/// - rete Docker isolata (non raggiunge localhost del server host)
/// - limiti di memoria e CPU tramite Docker cgroups
///
/// Se Docker non è disponibile, il processo gira direttamente ma con
/// le variabili di sistema Nexus filtrate (fallback sicuro).
pub async fn spawn_agent_process(
    db: &PgPool,
    project_id: Uuid,
    session_id: Option<Uuid>,
    label: &str,
    command: &str,
    working_dir: &str,
    project_root: Option<PathBuf>,
    env_overrides: Option<HashMap<String, String>>,
    sandbox_available: bool,
    kind: &str,
    service_image: Option<String>,
) -> Result<Uuid, String> {
    let process_id: Uuid = Uuid::new_v4();

    let will_use_docker = sandbox_available && project_root.is_some();

    // ── Strato 2 hardening: valida env_overrides PRIMA di insert DB ─────────────
    // Rifiuta PORT fuori bucket, DATABASE_URL su DB nexus, REDIS_URL su :6379, ecc.
    // Audit della rejection scritto qui sotto se l'errore arriva.
    let env_vars = env_overrides.unwrap_or_default();
    if let Err(e) = sandbox::validate_env_overrides(db, project_id, &env_vars).await {
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(project_id, "env_rejected", "env")
                .with_resource(label.to_string())
                .with_details(serde_json::json!({"reason": e, "command": command})),
        );
        return Err(format!("env override rifiutato: {e}"));
    }

    // Separazione DB: agent_processes e' una tabella migrata; con project_id in
    // scope instradiamo tutte le scritture/letture di questa funzione (e del task
    // di background) sul pool del progetto. A flag OFF ritorna il meta-pool.
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;

    // Insert initial DB row
    sqlx::query(
        r#"INSERT INTO agent_processes (id, project_id, session_id, label, command, working_dir, status, sandboxed, kind)
           VALUES ($1, $2, $3, $4, $5, $6, 'starting', $7, $8)"#,
    )
    .bind(process_id)
    .bind(project_id)
    .bind(session_id)
    .bind(label)
    .bind(command)
    .bind(working_dir)
    .bind(will_use_docker)
    .bind(kind)
    .execute(&proj_pool)
    .await
    .map_err(|e| format!("DB insert error: {e}"))?;

    // Notifica frontend: nuovo canale output disponibile per questo processo
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::ProjectEvent::OutputChannelCreated {
            channel_id: format!("agent:{}", process_id),
            label: label.to_string(),
        },
    );

    // ── Scelta della strategia di spawn ──────────────────────────────────────
    //
    // A) Docker immagine progetto: servizi con Dockerfile proprio (isolamento completo,
    //    l'immagine ha già le dipendenze giuste del progetto).
    // B) Docker nexus-sandbox: tool agente (kind != "service") con sandbox disponibile.
    //    NON usato per servizi senza Dockerfile: causerebbe EACCES sui file del progetto
    //    perché nexus-sandbox gira con UID diverso dall'host.
    // C) Spawn diretto con env filtrato: servizi senza Dockerfile propria, o se Docker
    //    non disponibile.

    // I servizi senza immagine dedicata girano direttamente sull'host.
    let use_docker = will_use_docker && (kind != "service" || service_image.is_some());

    let mut child = if use_docker {
        // use_docker e' true SOLO se will_use_docker E (kind != "service"
        // OR service_image.is_some()). Ma project_root resta Option; se
        // assente il caller ha sbagliato a invocare: errore esplicito.
        let root = project_root
            .as_ref()
            .ok_or_else(|| "project_root mancante con use_docker=true".to_string())?;
        let cwd = PathBuf::from(working_dir);
        let project_cfg = sandbox::load_project_sandbox_config(db, project_id).await;
        let mut config =
            SandboxConfig::new(root.clone(), process_id).with_project_config(&project_cfg);
        if let Some(img) = service_image {
            config = config.with_image(img);
        }
        // I servizi (kind=service) devono ricevere connessioni esterne sulla
        // porta dichiarata: abilita bridge network. I tool agente (kind!=service)
        // restano isolati (network_mode = "none" di default in SandboxConfig::new).
        if kind == "service" && project_cfg.network_mode.is_none() {
            config = config.with_service_network();
        }
        let mut docker_cmd = sandbox::build_sandboxed_command(command, &cwd, &env_vars, &config);

        #[cfg(unix)]
        {
            docker_cmd.process_group(0);
        }

        docker_cmd
            .spawn()
            .map_err(|e| format!("Docker sandbox spawn error: {e}"))?
    } else {
        // Fallback: processo diretto con env Nexus filtrato
        #[cfg(unix)]
        {
            let mut sh = Command::new("/bin/sh");
            sh.args(["-c", command])
                .current_dir(working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .process_group(0)
                // env_clear() + solo whitelist: rimuove DATABASE_URL, REDIS_URL, ecc.
                .env_clear();
            // Ripropaga le variabili sicure dell'host
            for (k, v) in sandbox::safe_env_for_direct_spawn() {
                sh.env(&k, &v);
            }
            // Aggiunge le override esplicite del progetto (non bloccate)
            for (k, v) in &env_vars {
                if !sandbox::is_blocked_env(k) {
                    sh.env(k, v);
                }
            }
            sh.spawn().map_err(|e| format!("Spawn error: {e}"))?
        }

        #[cfg(not(unix))]
        {
            // Windows: esegui via agent_shell (Git Bash) cosi' i comandi in sintassi
            // Unix (`a && b`, `... | tail`, `grep`) funzionano. NIENTE env_clear su
            // Windows: il processo eredita PATH/SystemRoot/TEMP necessari; sopra
            // aggiungiamo solo le override del progetto (es. PORT) non bloccate.
            let shell = sandbox::agent_shell();
            let mut sh = Command::new(&shell);
            sh.args(["-c", command])
                .current_dir(working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for (k, v) in &env_vars {
                if !sandbox::is_blocked_env(k) {
                    sh.env(k, v);
                }
            }
            sh.spawn().map_err(|e| format!("Spawn error: {e}"))?
        }
    };

    let pid = child.id().unwrap_or(0) as i32;

    // Update DB with PID and status=running
    let _ = sqlx::query(
        "UPDATE agent_processes SET pid=$1, status='running', started_at=NOW() WHERE id=$2",
    )
    .bind(pid)
    .bind(process_id)
    .execute(&proj_pool)
    .await;

    // Take stdout/stderr handles
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Il task di background eredita il pool instradato del progetto (separazione DB).
    let db_clone = proj_pool.clone();

    // Spawn background task to read output and flush to DB
    tokio::spawn(async move {
        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        let stdout_reader = stdout.map(tokio::io::BufReader::new);
        let stderr_reader = stderr.map(tokio::io::BufReader::new);

        let mut stdout_lines = stdout_reader.map(|r| r.lines());
        let mut stderr_lines = stderr_reader.map(|r| r.lines());

        // Flush interval: every 2 seconds
        let mut flush_interval = tokio::time::interval(std::time::Duration::from_secs(2));
        let mut process_exited = false;

        loop {
            tokio::select! {
                // Read stdout
                line = async {
                    if let Some(ref mut lines) = stdout_lines {
                        lines.next_line().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match line {
                        Ok(Some(l)) => {
                            stdout_buf.push_str(&l);
                            stdout_buf.push('\n');
                        }
                        Ok(None) => {
                            stdout_lines = None;
                            if stderr_lines.is_none() { process_exited = true; }
                        }
                        Err(_) => {
                            stdout_lines = None;
                            if stderr_lines.is_none() { process_exited = true; }
                        }
                    }
                }
                // Read stderr
                line = async {
                    if let Some(ref mut lines) = stderr_lines {
                        lines.next_line().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match line {
                        Ok(Some(l)) => {
                            stderr_buf.push_str(&l);
                            stderr_buf.push('\n');
                        }
                        Ok(None) => {
                            stderr_lines = None;
                            if stdout_lines.is_none() { process_exited = true; }
                        }
                        Err(_) => {
                            stderr_lines = None;
                            if stdout_lines.is_none() { process_exited = true; }
                        }
                    }
                }
                // Periodic flush to DB
                _ = flush_interval.tick() => {
                    flush_output(&db_clone, process_id, &mut stdout_buf, &mut stderr_buf).await;
                }
            }

            if process_exited {
                break;
            }
        }

        // Final flush
        flush_output(&db_clone, process_id, &mut stdout_buf, &mut stderr_buf).await;

        // Wait for exit code
        let exit_code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };

        let final_status = if exit_code == 0 { "stopped" } else { "failed" };
        // Non sovrascrivere se già marcato 'stopped' da una richiesta esplicita di stop
        let _ = sqlx::query(
            "UPDATE agent_processes SET status=$1, exit_code=$2, stopped_at=NOW() WHERE id=$3 AND status != 'stopped'",
        )
        .bind(final_status)
        .bind(exit_code)
        .bind(process_id)
        .execute(&db_clone)
        .await;
    });

    Ok(process_id)
}

/// Redazione dei segreti tecnici prima della persistenza/esposizione degli
/// output di processo (difesa in profondita', incidente Beaty-Book 2026-07-02:
/// connection string Postgres in chiaro in `agent_processes` -> tool_result).
/// Delega al punto unico `nexus_tool_kit::secret_text_scanner` (regola L):
/// nelle connection URL maschera la sola password (host/porta/db name restano
/// leggibili per il debugging), per API key/token maschera il valore.
/// Idempotente: applicarla sia al flush sia alla lettura non altera testo gia'
/// redatto.
pub(crate) fn redact_secrets_for_persistence(text: &str) -> String {
    let (redacted, kinds) = nexus_tool_kit::secret_text_scanner::SecretScanner
        .redact_secrets_preserving_context(text);
    if kinds > 0 {
        // Regola F: solo il conteggio dei tipi, mai il contenuto.
        tracing::debug!(
            kinds_redacted = kinds,
            "output di processo: segreti redatti prima della persistenza"
        );
    }
    redacted
}

/// Flush buffered output to DB (append-only, cap at 50KB per field).
/// I buffer contengono solo righe complete (lettura per-riga a monte), quindi
/// un segreto non viene mai spezzato tra due flush e la redazione per-chunk
/// e' affidabile.
async fn flush_output(
    db: &PgPool,
    process_id: Uuid,
    stdout_buf: &mut String,
    stderr_buf: &mut String,
) {
    if stdout_buf.is_empty() && stderr_buf.is_empty() {
        return;
    }

    let stdout_chunk = redact_secrets_for_persistence(&std::mem::take(stdout_buf));
    let stderr_chunk = redact_secrets_for_persistence(&std::mem::take(stderr_buf));

    let _ = sqlx::query(
        r#"UPDATE agent_processes
           SET output = LEFT(output || $1, 50000),
               error_output = LEFT(error_output || $2, 50000)
           WHERE id = $3"#,
    )
    .bind(&stdout_chunk)
    .bind(&stderr_chunk)
    .bind(process_id)
    .execute(db)
    .await;
}

/// Read the last N chars of output for a process
pub async fn read_process_output(
    db: &PgPool,
    project_id: Uuid,
    process_id: Uuid,
    max_chars: usize,
) -> Result<ProcessOutput, String> {
    // Separazione DB: agent_processes vive nel pool del progetto (flag ON),
    // risolto dal project_id passato dal chiamante. A flag OFF -> meta-DB.
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let row = sqlx::query(
        "SELECT status, exit_code, output, error_output, command, pid FROM agent_processes WHERE id=$1",
    )
    .bind(process_id)
    .fetch_optional(&proj_pool)
    .await
    .map_err(|e| format!("DB error: {e}"))?
    .ok_or_else(|| "Process not found".to_string())?;

    let status: String = row.try_get("status").unwrap_or_else(|_| "unknown".into());
    let exit_code: Option<i32> = row.try_get("exit_code").unwrap_or(None);
    // Difesa in profondita' anche in lettura: copre le righe persistite in
    // chiaro PRIMA dell'introduzione della redazione al flush (dati storici nel
    // DB). Redazione sull'output completo, PRIMA del tail: il taglio potrebbe
    // spezzare il pattern e lasciar passare la credenziale.
    let output =
        redact_secrets_for_persistence(&row.try_get::<String, _>("output").unwrap_or_default());
    let error_output = redact_secrets_for_persistence(
        &row.try_get::<String, _>("error_output").unwrap_or_default(),
    );
    let command: String = row.try_get("command").unwrap_or_default();
    let pid: Option<i32> = row.try_get("pid").unwrap_or(None);

    // Take last max_chars
    let out_tail = if output.len() > max_chars {
        &output[output.len() - max_chars..]
    } else {
        &output
    };
    let err_tail = if error_output.len() > max_chars {
        &error_output[error_output.len() - max_chars..]
    } else {
        &error_output
    };

    Ok(ProcessOutput {
        command,
        pid,
        status,
        exit_code,
        stdout: out_tail.to_string(),
        stderr: err_tail.to_string(),
    })
}

use sqlx::Row;


pub struct ProcessOutput {
    pub command: String,
    pub pid: Option<i32>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Stop a running process by PID (e Docker container, se applicabile).
pub async fn stop_process(
    db: &PgPool,
    project_id: Uuid,
    process_id: Uuid,
) -> Result<String, String> {
    // Separazione DB: agent_processes vive nel pool del progetto (flag ON),
    // risolto dal project_id passato dal chiamante. A flag OFF -> meta-DB.
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let row = sqlx::query("SELECT pid, status FROM agent_processes WHERE id=$1")
        .bind(process_id)
        .fetch_optional(&proj_pool)
        .await
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| "Process not found".to_string())?;

    let status: String = row.try_get("status").unwrap_or_default();
    if status != "running" && status != "starting" {
        return Ok(format!("Process already {status}"));
    }

    // Marca subito come 'stopped' PRIMA di inviare il segnale (evita race condition).
    let _ = sqlx::query(
        "UPDATE agent_processes SET status='stopped', stopped_at=NOW() WHERE id=$1 AND status IN ('running','starting')",
    )
    .bind(process_id)
    .execute(&proj_pool)
    .await;

    // 1. Ferma il container Docker (se esiste) — va fatto PRIMA di killare il docker CLI
    sandbox::stop_sandbox_container(process_id).await;

    // 2. Kill del process group (docker CLI o processo diretto)
    let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            // kill dell'intero process group (-{pid}): graceful (TERM) poi forzato (9).
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &format!("-{pid}")])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = tokio::process::Command::new("kill")
                .args(["-9", &format!("-{pid}")])
                .output()
                .await;
        }
        #[cfg(windows)]
        {
            // taskkill /T termina l'intero albero processi (equivalente del process
            // group Unix), /F forzato. Niente `kill` su Windows.
            let _ = tokio::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output()
                .await;
        }
    }

    Ok("Process stopped".to_string())
}

/// List recent processes for a project
pub async fn list_processes(db: &PgPool, project_id: Uuid) -> Result<Vec<ProcessSummary>, String> {
    // Separazione DB: tabella migrata, project_id in scope -> pool del progetto.
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let rows = sqlx::query(
        r#"SELECT id, label, command, pid, status, exit_code, created_at
           FROM agent_processes
           WHERE project_id = $1
           ORDER BY created_at DESC
           LIMIT 20"#,
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    .map_err(|e| format!("DB error: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|row| ProcessSummary {
            id: row.get::<Uuid, _>("id"),
            label: row.get::<String, _>("label"),
            command: row.get::<String, _>("command"),
            pid: row.try_get("pid").unwrap_or(None),
            status: row.get::<String, _>("status"),
            exit_code: row.try_get("exit_code").unwrap_or(None),
            created_at: row
                .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                .to_rfc3339(),
        })
        .collect())
}


pub struct ProcessSummary {
    pub id: Uuid,
    pub label: String,
    pub command: String,
    pub pid: Option<i32>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::{is_generic_service_label, kind_for_run_config_role, similar_service_labels};

    #[test]
    fn run_config_tool_e_test_sono_task_non_servizi() {
        // Regressione pannello Servizi: 'install-dependencies' (role=tool)
        // veniva registrato kind='service' e compariva per sempre nella lista.
        assert_eq!(kind_for_run_config_role(Some("tool")), "task");
        assert_eq!(kind_for_run_config_role(Some("test")), "task");
        assert_eq!(kind_for_run_config_role(Some("backend")), "service");
        assert_eq!(kind_for_run_config_role(Some("frontend")), "service");
        assert_eq!(kind_for_run_config_role(Some("service")), "service");
        // Config antiche senza role: conservativo, resta servizio.
        assert_eq!(kind_for_run_config_role(None), "service");
    }

    #[test]
    fn label_generiche_riconosciute() {
        // Regressione voce fantasma "Service": label senza parole significative.
        assert!(is_generic_service_label("Service"));
        assert!(is_generic_service_label("server"));
        assert!(is_generic_service_label("dev-server"));
        assert!(!is_generic_service_label("backend"));
        assert!(!is_generic_service_label("frontend-dev"));
        assert!(!is_generic_service_label("Backend API"));
    }

    #[test]
    fn similarity_label_stesso_servizio() {
        // Regressione doppio vite: "frontend-dev" e "frontend" sono lo stesso
        // servizio (il trattino va splittato come lo spazio).
        assert!(similar_service_labels("frontend-dev", "frontend"));
        assert!(similar_service_labels("Backend API", "backend"));
        assert!(similar_service_labels("Backend", "backend")); // case-insensitive
        assert!(similar_service_labels("Service", "service")); // eq, pur generica
        // Scopi diversi NON si fondono.
        assert!(!similar_service_labels("frontend", "backend"));
        assert!(!similar_service_labels("Service", "backend"));
        assert!(!similar_service_labels("worker-emails", "frontend"));
    }
}
