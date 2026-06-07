//! Endpoint per esecuzione controllata di comandi dalla chat dell'IDE.
//!
//! POST /api/projects/:id/execute-command
//! Body: { "command": "npm test", "timeout_secs": 60 }
//! Response: { exit_code, stdout, stderr, blocked, blocked_reason?, duration_ms }
//!
//! Il comando viene eseguito nella root del progetto, con safety check
//! via `check_command` (stesse regole degli agent tool). Output troncato
//! a 100 KB per evitare risposte enormi.

use super::*;

#[derive(serde::Deserialize)]
pub struct ExecuteCommandRequest {
    pub command: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub async fn execute_command(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ExecuteCommandRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let command = body.command.trim().to_string();
    if command.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Comando vuoto"));
    }

    // Safety check: stesse regole degli agent tool
    if let Some(reason) = crate::agent_tools::safety::check_command(&command) {
        // Audit: registra il blocco
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(project_id, "chat_command_blocked", "command")
                .with_actor_user(user_id)
                .with_resource(command.clone())
                .with_details(serde_json::json!({
                    "category": reason.category,
                    "message": reason.message,
                    "source": "chat_execute",
                })),
        );
        return Ok(Json(json!({
            "exit_code": -1,
            "stdout": "",
            "stderr": format!(
                "Comando bloccato da safety check Nexus.\nMotivo: {}\nRimedio: {}",
                reason.message, reason.remediation
            ),
            "blocked": true,
            "blocked_reason": reason.message,
            "duration_ms": 0,
        })));
    }

    let timeout_secs = body.timeout_secs.unwrap_or(60).min(120).max(5);
    let timeout = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();

    // Esegui il comando nella root del progetto via bash
    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new("bash")
            .args(["-c", &command])
            .current_dir(&context.root_path)
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("TERM", "dumb")
            .output(),
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;
    let max_output = 100_000;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);

            // Capacita' 2: parsing strutturato degli errori di build sull'output
            // COMPLETO (prima del troncamento), solo se il comando e' fallito.
            // Punto unico: project_workspace::build_diagnostics.
            let diagnostics = if exit_code != 0 {
                crate::project_workspace::build_diagnostics::parse_diagnostics(
                    &command, &stdout, &stderr,
                )
            } else {
                Vec::new()
            };

            let stdout_out = if stdout.len() > max_output {
                format!(
                    "{}...\n[troncato a {} byte]",
                    &stdout[..max_output],
                    max_output
                )
            } else {
                stdout.to_string()
            };
            let stderr_out = if stderr.len() > max_output {
                format!(
                    "{}...\n[troncato a {} byte]",
                    &stderr[..max_output],
                    max_output
                )
            } else {
                stderr.to_string()
            };

            // Audit: registra esecuzione
            crate::security::record_audit(
                crate::security::AuditEntry::allowed(project_id, "chat_command_exec", "command")
                    .with_actor_user(user_id)
                    .with_resource(command)
                    .with_details(serde_json::json!({
                        "exit_code": exit_code,
                        "duration_ms": duration_ms,
                        "source": "chat_execute",
                    })),
            );

            Ok(Json(json!({
                "exit_code": exit_code,
                "stdout": stdout_out,
                "stderr": stderr_out,
                "blocked": false,
                "duration_ms": duration_ms,
                "diagnostics": diagnostics,
            })))
        }
        Ok(Err(e)) => Ok(Json(json!({
            "exit_code": -1,
            "stdout": "",
            "stderr": format!("Errore esecuzione: {}", e),
            "blocked": false,
            "duration_ms": duration_ms,
        }))),
        Err(_) => Ok(Json(json!({
            "exit_code": -1,
            "stdout": "",
            "stderr": format!("Timeout: il comando ha superato il limite di {} secondi", timeout_secs),
            "blocked": false,
            "duration_ms": duration_ms,
        }))),
    }
}
