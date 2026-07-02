//! Tool `nexus_verify_change` (ADR 0019 L3): catena di verifica post-modifica
//! typecheck -> build -> lint -> test con fail-fast al primo step rosso.
//!
//! Esito STRUTTURATO (regola M): ogni step riporta `exit_code` (punto unico
//! `tool_runner_server::extract_exit_code`) + `build_errors` (punto unico
//! `nexus_agent_graph::count_build_errors`, stessa coppia del criterio build
//! del final_gate); il consumatore legge `passed`/`first_failure`, mai la prosa.
//!
//! Risoluzione comando per step (punto unico `resolve_verify_command`, regola
//! G/L): run_configurations del progetto (role = nome step) > settings
//! `agent.verify.<lang>.<step>` > nessuno = step SKIPPATO con motivo
//! strutturato. MAI un comando hardcoded di fallback.
//!
//! Linguaggio dal build graph (ADR 0020): `BuildGraphCache::get_or_compute`.

use super::*;

/// Ordine canonico della catena completa (fail-fast al primo rosso).
const STEPS_FULL: &[&str] = &["typecheck", "build", "lint", "test"];
/// Scope rapido: solo i check statici (niente build/test).
const STEPS_QUICK: &[&str] = &["typecheck", "lint"];

/// Comando risolto per uno step, con la provenienza (per il report).
struct ResolvedCmd {
    command: String,
    source: &'static str, // "run_configuration" | "settings"
}

/// Punto unico (regola L) della risoluzione comando per (progetto, linguaggio,
/// step). Precedenza: override locale in `run_configurations` (role = step) >
/// default globale `agent.verify.<lang>.<step>` > `None` (step saltato).
async fn resolve_verify_command(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    lang: &str,
    step: &str,
) -> Option<ResolvedCmd> {
    // 1. Override per-progetto: run_configuration col role omonimo dello step.
    let row: Option<(String, Vec<String>)> = sqlx::query_as(
        "SELECT command, args FROM run_configurations \
         WHERE project_id = $1 AND role = $2 \
         ORDER BY essential DESC, updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(step)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if let Some((command, args)) = row {
        let full = if args.is_empty() {
            command
        } else {
            format!("{} {}", command, args.join(" "))
        };
        if !full.trim().is_empty() {
            return Some(ResolvedCmd {
                command: full,
                source: "run_configuration",
            });
        }
    }
    // 2. Default globale dal DB (regola G: nessun fallback hardcoded).
    let key = format!("agent.verify.{lang}.{step}");
    nexus_auth::get_setting(db, &key)
        .await
        .filter(|v| !v.trim().is_empty())
        .map(|command| ResolvedCmd {
            command,
            source: "settings",
        })
}

/// Estratto head+tail non distruttivo dell'output per il report (i totali dei
/// build sono IN FONDO: mai tagliare solo la coda).
fn output_excerpt(raw: &str, max_chars: usize) -> (String, bool) {
    let total = raw.chars().count();
    if total <= max_chars {
        return (raw.to_string(), false);
    }
    let head: String = raw.chars().take(max_chars / 2).collect();
    let tail: String = {
        let skip = total - max_chars / 2;
        raw.chars().skip(skip).collect()
    };
    (
        format!("{head}\n[... output troncato ...]\n{tail}"),
        true,
    )
}

pub(super) async fn tool_nexus_verify_change(ctx: &AgentToolContext, input: &Value) -> String {
    // Kill-switch DB-driven.
    let enabled = nexus_auth::get_setting(&ctx.db, "agent.verify.enabled")
        .await
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false);
    if !enabled {
        return serde_json::json!({
            "error": "verify_disabled",
            "detail": "agent.verify.enabled non attivo: catena di verifica disabilitata dall'admin."
        })
        .to_string();
    }

    let scope = input
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("full")
        .to_ascii_lowercase();
    let steps: Vec<&str> = match scope.as_str() {
        "full" => STEPS_FULL.to_vec(),
        "quick" => STEPS_QUICK.to_vec(),
        s if STEPS_FULL.contains(&s) => vec![STEPS_FULL[STEPS_FULL.iter().position(|x| *x == s).unwrap()]],
        other => {
            return serde_json::json!({
                "error": "invalid_scope",
                "detail": format!("scope '{other}' non valido: usa quick|full|typecheck|build|lint|test"),
            })
            .to_string();
        }
    };

    // Linguaggio dal build graph (ADR 0020). Cache non inizializzata o resolver
    // in errore -> "unknown": nessuno step eseguibile, motivo strutturato.
    let language = match nexus_build_graph::BuildGraphCache::global() {
        Some(cache) => cache
            .get_or_compute(ctx.project_id)
            .await
            .map(|info| info.language)
            .unwrap_or_else(|_| "unknown".to_string()),
        None => "unknown".to_string(),
    };

    let step_timeout_s = nexus_auth::get_setting(&ctx.db, "agent.verify.step_timeout_s")
        .await
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(180)
        .max(10);
    let output_max_chars = nexus_auth::get_setting(&ctx.db, "agent.verify.output_max_chars")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(4000)
        .max(200);

    let working_dir = input.get("working_dir").and_then(Value::as_str);

    let mut report_steps: Vec<Value> = Vec::with_capacity(steps.len());
    let mut passed = true;
    let mut first_failure: Option<&str> = None;

    for step in &steps {
        // Fail-fast: gli step dopo il primo rosso sono marcati skipped.
        if first_failure.is_some() {
            report_steps.push(serde_json::json!({
                "step": step,
                "skipped_reason": "fail_fast",
            }));
            continue;
        }
        let Some(resolved) = resolve_verify_command(&ctx.db, ctx.project_id, &language, step).await
        else {
            report_steps.push(serde_json::json!({
                "step": step,
                "skipped_reason": "no_command_configured",
                "detail": format!(
                    "nessuna run_configuration role='{step}' e nessun setting agent.verify.{language}.{step}"
                ),
            }));
            continue;
        };

        let mut tool_input = serde_json::json!({ "command": resolved.command });
        if let Some(wd) = working_dir {
            tool_input["working_dir"] = serde_json::json!(wd);
        }
        let started = std::time::Instant::now();
        // Timeout per-step DB-driven come bound esterno (il run_command ha i suoi
        // probe interni 10s/300s): allo scadere lo step fallisce con motivo
        // strutturato; il processo figlio residuo termina da solo.
        let raw = match tokio::time::timeout(
            std::time::Duration::from_secs(step_timeout_s),
            super::command::tool_run_command(ctx, &tool_input),
        )
        .await
        {
            Ok(s) => s,
            Err(_) => {
                passed = false;
                first_failure = Some(step);
                report_steps.push(serde_json::json!({
                    "step": step,
                    "command": resolved.command,
                    "command_source": resolved.source,
                    "passed": false,
                    "skipped_reason": null,
                    "timeout": true,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "detail": format!("step oltre agent.verify.step_timeout_s ({step_timeout_s}s)"),
                }));
                continue;
            }
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        // Esito STRUTTURATO: exit_code dal punto unico + rete build_errors
        // (exit 0 bugiardo di certi bundler, stessa coppia del final_gate).
        let exit_code = crate::tool_runner_server::extract_exit_code(&raw);
        let build_errors = nexus_agent_graph::count_build_errors(&raw);
        let step_ok = exit_code == Some(0) && build_errors == 0;
        if !step_ok {
            passed = false;
            first_failure = Some(step);
        }
        let (excerpt, truncated) = output_excerpt(&raw, output_max_chars);
        report_steps.push(serde_json::json!({
            "step": step,
            "command": resolved.command,
            "command_source": resolved.source,
            "passed": step_ok,
            "exit_code": exit_code,
            "build_errors": build_errors,
            "duration_ms": duration_ms,
            "output_excerpt": excerpt,
            "output_truncated": truncated,
        }));
    }

    serde_json::json!({
        "language": language,
        "scope": scope,
        "passed": passed,
        "first_failure": first_failure,
        "steps": report_steps,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_preserva_testa_e_coda() {
        let raw = format!("{}FINE-CODA", "x".repeat(10_000));
        let (excerpt, truncated) = output_excerpt(&raw, 400);
        assert!(truncated);
        assert!(excerpt.starts_with("xxxx"));
        assert!(excerpt.ends_with("FINE-CODA"), "la coda (totali build) va preservata");
        assert!(excerpt.contains("[... output troncato ...]"));
    }

    #[test]
    fn excerpt_sotto_soglia_invariato() {
        let (excerpt, truncated) = output_excerpt("breve", 400);
        assert!(!truncated);
        assert_eq!(excerpt, "breve");
    }

    #[sqlx::test]
    async fn resolve_verify_command_run_config_vince_su_setting(pool: sqlx::PgPool) {
        // Precedenza: run_configurations (role=step) > settings > None.
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .expect("create settings");
        sqlx::query(
            "CREATE TABLE run_configurations ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 project_id UUID NOT NULL, \
                 label TEXT NOT NULL DEFAULT '', \
                 command TEXT NOT NULL, \
                 args TEXT[] NOT NULL DEFAULT '{}', \
                 role TEXT, \
                 essential BOOLEAN NOT NULL DEFAULT FALSE, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(&pool)
        .await
        .expect("create run_configurations");
        let pid = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES \
             ('agent.verify.rust.test', 'cargo test --workspace')",
        )
        .execute(&pool)
        .await
        .expect("seed setting");
        sqlx::query(
            "INSERT INTO run_configurations (project_id, command, args, role) VALUES \
             ($1, 'cargo', ARRAY['nextest','run'], 'test')",
        )
        .bind(pid)
        .execute(&pool)
        .await
        .expect("seed run_config");

        // Override locale vince.
        let r = resolve_verify_command(&pool, pid, "rust", "test").await.expect("risolto");
        assert_eq!(r.command, "cargo nextest run");
        assert_eq!(r.source, "run_configuration");

        // Altro progetto senza run_config -> setting globale.
        let r2 = resolve_verify_command(&pool, uuid::Uuid::new_v4(), "rust", "test")
            .await
            .expect("risolto da settings");
        assert_eq!(r2.command, "cargo test --workspace");
        assert_eq!(r2.source, "settings");

        // Step senza chiave -> None (skip, mai hardcode).
        assert!(resolve_verify_command(&pool, pid, "rust", "lint").await.is_none());
    }
}
