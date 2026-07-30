//! Tool `nexus_verify_change` (ADR 0019 L3 + ADR 0036): catena di verifica
//! post-modifica con fail-fast al primo step rosso.
//!
//! Gli step NON sono un vocabolario fisso: vengono dal PROFILO PER-AMBIENTE
//! inferito da un LLM che osserva il progetto (`verify_profile`, mig 0508).
//! Nessuna matrice statica linguaggio->comando (decisione utente): se il
//! profilo non e' disponibile il tool lo dichiara con esito strutturato,
//! senza comandi generici di ripiego.
//!
//! Esito STRUTTURATO (regola M): ogni step riporta `exit_code` (punto unico
//! `tool_runner_server::extract_exit_code`) + `build_errors` (punto unico
//! `nexus_agent_graph::count_build_errors`, stessa coppia dei criteri del
//! final_gate); il consumatore legge `passed`/`first_failure`, mai la prosa.
//!
//! Precedenza comando per step (regola G/L): override locale in
//! `run_configurations` (role = nome step) > comando dello step nel profilo.

use super::*;

use crate::verify_profile::VerifyProfileStep;
use nexus_types::tool_outcome::tool_failure;

/// Costruisce l'esito FALLITO del tool: marker + payload JSON (contratto
/// `nexus_types::tool_outcome`), condiviso dai rami di errore qui sotto
/// (kill-switch disattivo, profilo assente, scope invalido). Senza il marker
/// in testa questi fallimenti erano indistinguibili da un report riuscito per
/// anti-loop/supervisore/final_gate (leggono solo `is_tool_failure`).
fn verify_failure(payload: Value) -> String {
    tool_failure(payload.to_string())
}

/// Comando risolto per uno step, con la provenienza (per il report).
struct ResolvedCmd {
    command: String,
    source: &'static str, // "run_configuration" | "verify_profile"
}

/// Override locale per-progetto: run_configuration col role omonimo dello
/// step (vince SEMPRE sul comando del profilo: e' la scelta esplicita
/// dell'utente). `None` = nessun override, si usa il profilo.
async fn resolve_step_override(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    step: &str,
) -> Option<String> {
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
    let (command, args) = row?;
    let full = if args.is_empty() {
        command
    } else {
        format!("{} {}", command, args.join(" "))
    };
    (!full.trim().is_empty()).then_some(full)
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
    (format!("{head}\n[... output troncato ...]\n{tail}"), true)
}

pub(super) async fn tool_nexus_verify_change(ctx: &AgentToolContext, input: &Value) -> String {
    // Kill-switch DB-driven.
    let enabled = nexus_auth::get_setting(&ctx.db, "agent.verify.enabled")
        .await
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return verify_failure(serde_json::json!({
            "error": "verify_disabled",
            "detail": "agent.verify.enabled non attivo: catena di verifica disabilitata dall'admin."
        }));
    }

    let scope = input
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("full")
        .to_ascii_lowercase();

    // Profilo per-ambiente (ADR 0036): inferito da LLM alla prima richiesta,
    // poi cache su tabella con invalidazione deterministica. Il tool puo'
    // triggerare l'inferenza (ha meta-db, neural e root nel contesto).
    let profile: Vec<VerifyProfileStep> =
        crate::verify_profile::ensure_profile(&ctx.db, &ctx.neural, ctx.project_id, &ctx.root_path)
            .await;
    if profile.is_empty() {
        // NESSUN comando generico di ripiego (decisione utente): esito
        // strutturato onesto, il chiamante sa che la verifica non e' partita.
        return verify_failure(serde_json::json!({
            "error": "profile_unavailable",
            "detail": "Profilo di verifica dell'ambiente non disponibile: inferenza LLM non riuscita e nessun profilo salvato. Riprova quando il modello e' raggiungibile, oppure definisci run_configurations con role di verifica.",
        }));
    }
    let available: Vec<&str> = profile.iter().map(|s| s.step.as_str()).collect();
    let steps: Vec<&VerifyProfileStep> = match scope.as_str() {
        // Catena completa: tutti gli step del profilo, nell'ordine del profilo.
        "full" => profile.iter().collect(),
        // Rapido: solo gli step che l'LLM ha marcato per il gate di chiusura.
        "quick" => profile.iter().filter(|s| s.gate).collect(),
        name => {
            let hit: Vec<&VerifyProfileStep> = profile.iter().filter(|s| s.step == name).collect();
            if hit.is_empty() {
                return verify_failure(serde_json::json!({
                    "error": "invalid_scope",
                    "detail": format!(
                        "scope '{name}' non presente nel profilo: usa quick|full oppure uno step del profilo"
                    ),
                    "available_steps": available,
                }));
            }
            hit
        }
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

    for profile_step in &steps {
        let step = profile_step.step.as_str();
        // Fail-fast: gli step dopo il primo rosso sono marcati skipped.
        if first_failure.is_some() {
            report_steps.push(serde_json::json!({
                "step": step,
                "skipped_reason": "fail_fast",
            }));
            continue;
        }
        // Precedenza: override esplicito dell'utente (run_configuration col
        // role omonimo) > comando dello step nel profilo inferito.
        let resolved = match resolve_step_override(&ctx.db, ctx.project_id, step).await {
            Some(command) => ResolvedCmd {
                command,
                source: "run_configuration",
            },
            None => ResolvedCmd {
                command: profile_step.command.clone(),
                source: "verify_profile",
            },
        };

        let mut tool_input = serde_json::json!({ "command": resolved.command });
        // working_dir: input esplicito del chiamante > working_dir dello step.
        if let Some(wd) = working_dir.or(profile_step.working_dir.as_deref()) {
            tool_input["working_dir"] = serde_json::json!(wd);
        }
        let started = std::time::Instant::now();
        // Timeout per-step: quello proposto dal profilo per QUESTO step, col
        // bound globale DB-driven come default (il run_command ha i suoi probe
        // interni): allo scadere lo step fallisce con motivo strutturato.
        let effective_timeout_s = profile_step
            .timeout_s
            .map(|t| t.max(1.0) as u64)
            .unwrap_or(step_timeout_s);
        let raw = match tokio::time::timeout(
            std::time::Duration::from_secs(effective_timeout_s),
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
                    "detail": format!("step oltre il timeout ({effective_timeout_s}s)"),
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
        "scope": scope,
        "passed": passed,
        "first_failure": first_failure,
        "steps": report_steps,
        "profile_steps_available": available,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_failure_dichiara_il_fallimento_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato dai 3 rami di errore del tool: se
        // domani uno di quei rami smettesse di passare da qui, resterebbe
        // invisibile ad anti-loop/supervisore/final_gate (regola M).
        let out = verify_failure(serde_json::json!({
            "error": "profile_unavailable",
            "detail": "profilo assente",
        }));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        // Il payload resta leggibile (per l'umano/il modello) dopo il marker.
        let after_marker = out
            .trim_start_matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
            .trim_start();
        let parsed: Value =
            serde_json::from_str(after_marker).expect("payload dopo il marker e' JSON valido");
        assert_eq!(parsed["error"], "profile_unavailable");
    }

    #[test]
    fn excerpt_preserva_testa_e_coda() {
        let raw = format!("{}FINE-CODA", "x".repeat(10_000));
        let (excerpt, truncated) = output_excerpt(&raw, 400);
        assert!(truncated);
        assert!(excerpt.starts_with("xxxx"));
        assert!(
            excerpt.ends_with("FINE-CODA"),
            "la coda (totali build) va preservata"
        );
        assert!(excerpt.contains("[... output troncato ...]"));
    }

    #[test]
    fn excerpt_sotto_soglia_invariato() {
        let (excerpt, truncated) = output_excerpt("breve", 400);
        assert!(!truncated);
        assert_eq!(excerpt, "breve");
    }

    #[sqlx::test]
    async fn resolve_step_override_vince_sul_profilo(pool: sqlx::PgPool) {
        // L'override utente (run_configurations, role = nome step) vince sul
        // comando del profilo inferito; senza override -> None (si usa il
        // profilo). NESSUN terzo livello statico (ADR 0036).
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
            "INSERT INTO run_configurations (project_id, command, args, role) VALUES \
             ($1, 'cargo', ARRAY['nextest','run'], 'test')",
        )
        .bind(pid)
        .execute(&pool)
        .await
        .expect("seed run_config");

        // Override locale presente.
        let r = resolve_step_override(&pool, pid, "test")
            .await
            .expect("override");
        assert_eq!(r, "cargo nextest run");

        // Altro progetto senza run_config -> None (si usa il profilo).
        assert!(resolve_step_override(&pool, uuid::Uuid::new_v4(), "test")
            .await
            .is_none());
        // Step senza role corrispondente -> None.
        assert!(resolve_step_override(&pool, pid, "lint").await.is_none());
    }
}
