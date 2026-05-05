//! Tool servizio: avvio processi long-running, lettura output, stop, build immagine progetto.
//! Include anche helper terminali legacy (attualmente non in uso, dead_code).

use super::*;

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
                Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
            }
        } else {
            ctx.root_path.clone()
        }
    } else {
        ctx.root_path.clone()
    };

    // Kill processi precedenti con la stessa label per evitare duplicati
    if let Ok(existing) = crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        for proc in existing.iter().filter(|p| p.label == label && (p.status == "running" || p.status == "starting")) {
            let _ = crate::agent_processes::stop_process(&ctx.db, proc.id).await;
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
        None,
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
                    let mut msg = format!(
                        "Servizio '{}' avviato (process_id: {}, pid: {}, status: {})\n",
                        label,
                        process_id,
                        info.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
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

/// Legge l'output di un servizio avviato con run_service
pub(super) async fn tool_read_service_output(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = input.get("process_id").and_then(Value::as_str).unwrap_or("");

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
        Ok(msg) => msg,
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

// ── Helper terminale legacy (non usati attualmente, mantenuti per compatibilità) ─────

/// Fase 1: aspetta che il frontend confermi la ricezione del comando (delivered/failed).
/// Molto veloce: il frontend risponde quasi subito.
#[allow(dead_code)]
async fn wait_for_terminal_delivery(db: &PgPool, command_id: Uuid) -> Option<(String, Option<String>)> {
    for _ in 0..33 {
        // max ~10s
        if let Ok(Some(row)) = sqlx::query(
            "SELECT status, output_preview, fail_reason FROM terminal_commands WHERE id = $1",
        )
        .bind(command_id)
        .fetch_optional(db)
        .await
        {
            let status: String = row.try_get("status").unwrap_or_else(|_| "pending".to_string());
            if status == "delivered" || status == "finished" {
                let output_preview: Option<String> = row.try_get("output_preview").unwrap_or(None);
                return Some((status, output_preview));
            }
            if status == "failed" {
                let fail_reason: Option<String> = row.try_get("fail_reason").unwrap_or(None);
                return Some(("failed".to_string(), fail_reason));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    None
}

/// Risultato del completamento di un comando terminale.
#[allow(dead_code)]
struct TerminalFinishResult {
    exit_code: Option<i32>,
    output: Option<String>,
    still_running: bool,
}

/// Fase 2: aspetta che il frontend segnali "finished" (output stabile o processo terminato).
/// Aspetta fino a max_secs — il frontend debounce è 3s, quindi il finish arriva in ~5-8s.
#[allow(dead_code)]
async fn wait_for_terminal_finish(db: &PgPool, command_id: Uuid, max_secs: u32) -> TerminalFinishResult {
    for _ in 0..max_secs {
        if let Ok(Some(row)) = sqlx::query(
            "SELECT finished_at, exit_code, full_output FROM terminal_commands WHERE id = $1",
        )
        .bind(command_id)
        .fetch_optional(db)
        .await
        {
            let finished_at: Option<chrono::DateTime<chrono::Utc>> =
                row.try_get("finished_at").unwrap_or(None);
            if finished_at.is_some() {
                let exit_code: Option<i32> = row.try_get("exit_code").unwrap_or(None);
                let full_output: Option<String> = row.try_get("full_output").unwrap_or(None);
                return TerminalFinishResult {
                    exit_code,
                    output: full_output,
                    still_running: exit_code.is_none(),
                };
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    // Timeout 120s: il processo e' probabilmente un server long-running
    TerminalFinishResult {
        exit_code: None,
        output: None,
        still_running: true,
    }
}

#[allow(dead_code)]
async fn tool_read_terminal_output(ctx: &AgentToolContext, input: &Value) -> String {
    let command_id_str = input.get("command_id").and_then(Value::as_str).unwrap_or("");

    let row = if command_id_str.is_empty() {
        // Leggi l'ultimo comando finito del progetto
        sqlx::query(
            "SELECT id, command, exit_code, full_output, finished_at, status
             FROM terminal_commands
             WHERE project_id = $1 AND (full_output IS NOT NULL OR status IN ('delivered','failed'))
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(ctx.project_id)
        .fetch_optional(&*ctx.db)
        .await
    } else {
        let cmd_id = match Uuid::parse_str(command_id_str) {
            Ok(id) => id,
            Err(_) => return "[Errore: command_id non valido]".to_string(),
        };
        sqlx::query(
            "SELECT id, command, exit_code, full_output, finished_at, status
             FROM terminal_commands WHERE id = $1",
        )
        .bind(cmd_id)
        .fetch_optional(&*ctx.db)
        .await
    };

    match row {
        Ok(Some(r)) => {
            let command: String = r.try_get("command").unwrap_or_default();
            let status: String = r.try_get("status").unwrap_or_default();
            let exit_code: Option<i32> = r.try_get("exit_code").unwrap_or(None);
            let full_output: Option<String> = r.try_get("full_output").unwrap_or(None);

            let mut result = format!("Comando: `{}`\nStato: {}", command, status);
            if let Some(code) = exit_code {
                result.push_str(&format!("\nExit code: {}", code));
            }
            if let Some(ref output) = full_output {
                if !output.trim().is_empty() {
                    result.push_str(&format!("\nOutput:\n{}", output));
                } else {
                    result.push_str("\n(nessun output catturato)");
                }
            } else {
                result.push_str("\n(output non ancora disponibile — il comando potrebbe essere ancora in esecuzione)");
            }
            result
        }
        Ok(None) => "[Nessun comando terminale trovato per questo progetto]".to_string(),
        Err(e) => format!("[Errore lettura output terminale: {}]", e),
    }
}
