// ═══════════════════════════════════════════════════════════════════════════
// claude_agents.rs — Generatore DB -> file .claude/agents/*.md (Componente A).
//
// Proietta le definizioni autoritative del DB (nexus_subagent_definitions +
// nexus_prompt_templates) nei file .claude/agents/<name>.md consumati da
// Claude Code CLI. UNA fonte di verita' (DB, multi-provider, locale), DUE
// proiezioni (runtime Nexus + file CLI).
//
// Vincoli rispettati:
//  - I file CLI NON serializzano `model` (il modello e' provider-agnostico,
//    risolto a runtime via model_purpose -> routing matrix). Niente lock su
//    un provider, niente nome modello nei file.
//  - I tool semantici (search_codebase_semantic, ...) degradano a Grep nella
//    proiezione CLI: la memoria vettoriale resta un asset Nexus-side, non
//    replicato su Anthropic.
//  - Idempotente: scrive solo se il contenuto cambia (sha256). Rispetta i file
//    curati a mano (senza marker AUTO-GENERATO) salvo force esplicito.
//  - Read-only sul DB. Path validation: solo dentro <repo_root>/.claude/agents/.
// ═══════════════════════════════════════════════════════════════════════════

use crate::AppState;
use anyhow::{Context, Result};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use sha2::{Digest, Sha256};

const MARKER: &str = "# AUTO-GENERATO dal DB Nexus";

/// Mappa i tool MCP (tool_whitelist DB) ai tool Claude Code, dedup nell'ordine
/// canonico. I tool semantici/memoria degradano a Grep (non replicati su CLI).
fn map_mcp_to_claude(whitelist: &[String]) -> Vec<&'static str> {
    let mut set: Vec<&'static str> = Vec::new();
    let mut push = |t: &'static str, set: &mut Vec<&'static str>| {
        if !set.contains(&t) {
            set.push(t);
        }
    };
    for tool in whitelist {
        let claude = match tool.as_str() {
            "read_file" | "read_file_lines" => Some("Read"),
            "write_file" => Some("Write"),
            "edit_file" => Some("Edit"),
            "list_files" => Some("Glob"),
            "search_in_files"
            | "search_codebase_semantic"
            | "search_file_semantic"
            | "recall_context"
            | "nexus_search_semantic" => Some("Grep"),
            "run_command"
            | "run_tests"
            | "run_specific_test"
            | "run_lint_fix"
            | "format_file"
            | "run_playwright_tests" => Some("Bash"),
            "nexus_todo_write" => Some("TodoWrite"),
            // knowledge_*, dispatch_subagent, request_port, ... : nessun analogo CLI
            _ => None,
        };
        if let Some(c) = claude {
            push(c, &mut set);
        }
    }
    // Riordina secondo l'ordine canonico.
    let order = ["Read", "Edit", "Write", "Grep", "Glob", "Bash", "TodoWrite"];
    let mut out: Vec<&'static str> = Vec::new();
    for o in order {
        if set.contains(&o) {
            out.push(o);
        }
    }
    out
}

/// Renderizza il contenuto completo del file .claude/agents/<name>.md.
fn render_agent_file(
    name: &str,
    kind: &str,
    description: &str,
    tools: &[&'static str],
    prompt_body: &str,
    def_hash: &str,
) -> String {
    let tools_csv = tools.join(", ");
    let has_semantic_note = prompt_body.contains("search_codebase_semantic")
        || prompt_body.contains("nexus_search_semantic")
        || prompt_body.contains("recall_context");
    let footer = if has_semantic_note {
        "\n\n> Nota: ricerca semantica e memoria vettoriale del progetto sono \
disponibili solo eseguendo questo agente DENTRO Nexus (multi-provider). \
Nel CLI Claude Code degradano a ricerca testuale (Grep).\n"
    } else {
        "\n"
    };
    format!(
        "---\n\
{MARKER} (nexus_subagent_definitions + nexus_prompt_templates).\n\
# NON EDITARE A MANO: ogni modifica viene sovrascritta alla rigenerazione.\n\
# Fonte di verita': nexus_subagent_definitions, kind=\"{kind}\". hash={def_hash}\n\
# Per modificare: UPDATE del DB oppure .nexus/agents/{kind}.md (override progetto).\n\
name: {name}\n\
description: {description}\n\
tools: {tools_csv}\n\
---\n\n\
{prompt_body}{footer}"
    )
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

#[derive(serde::Serialize)]
pub struct AgentGenResult {
    pub kind: String,
    pub name: String,
    pub file: String,
    pub action: String, // "written" | "unchanged" | "skipped_unmanaged" | "error"
    pub detail: Option<String>,
}

/// Genera (o preview) i file .claude/agents/*.md dalle definizioni DB.
/// `dry_run=true`: non scrive, ritorna solo cosa farebbe.
/// `force_overwrite_unmanaged=true`: sovrascrive anche i file senza marker
/// (promozione one-shot dei file curati a definizione DB).
pub async fn regenerate_all(
    state: &AppState,
    dry_run: bool,
    force_overwrite_unmanaged: bool,
) -> Result<Vec<AgentGenResult>> {
    // Gate da settings.
    let enabled: bool = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'claude_agents.export_enabled'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim() != "false")
    .unwrap_or(true);
    if !enabled {
        anyhow::bail!("claude_agents.export_enabled = false");
    }

    let output_dir: String = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'claude_agents.output_dir'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| ".claude/agents".to_string());
    let name_prefix: String = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'claude_agents.name_prefix'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "nexus-".to_string());

    let repo_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());
    let base = format!(
        "{}/{}",
        repo_root.trim_end_matches('/'),
        output_dir.trim_matches('/')
    );

    // Leggi le definizioni abilitate (read-only).
    let rows = sqlx::query_as::<_, (String, String, Vec<String>, String)>(
        "SELECT kind, description, tool_whitelist, prompt_key \
         FROM nexus_subagent_definitions WHERE is_enabled = true ORDER BY kind",
    )
    .fetch_all(&state.db)
    .await
    .context("query nexus_subagent_definitions")?;

    let mut results = Vec::new();
    for (kind, description, whitelist, prompt_key) in rows {
        let name = format!("{}{}", name_prefix, kind.replace('_', "-"));
        let file_rel = format!("{name}.md");
        let full_path = format!("{base}/{file_rel}");

        // Risolvi il prompt body dal DB (nexus_prompt_templates).
        let prompt_body: String = sqlx::query_scalar::<_, String>(
            "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
        )
        .bind(&prompt_key)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        if prompt_body.trim().is_empty() {
            results.push(AgentGenResult {
                kind: kind.clone(),
                name,
                file: file_rel,
                action: "error".to_string(),
                detail: Some(format!("prompt '{prompt_key}' non trovato/vuoto")),
            });
            continue;
        }

        let tools = map_mcp_to_claude(&whitelist);
        let def_hash = &sha256_hex(&format!(
            "{kind}|{description}|{:?}|{prompt_key}",
            whitelist
        ))[..8];
        let content = render_agent_file(
            &name,
            &kind,
            &description,
            &tools,
            prompt_body.trim(),
            def_hash,
        );
        let new_hash = sha256_hex(&content);

        // Controlla file esistente: marker + hash.
        let existing = tokio::fs::read_to_string(&full_path).await.ok();
        if let Some(old) = &existing {
            let is_managed = old.contains(MARKER);
            if !is_managed && !force_overwrite_unmanaged {
                results.push(AgentGenResult {
                    kind: kind.clone(),
                    name,
                    file: file_rel,
                    action: "skipped_unmanaged".to_string(),
                    detail: Some("file curato a mano (no marker), non sovrascritto".to_string()),
                });
                continue;
            }
            if sha256_hex(old) == new_hash {
                results.push(AgentGenResult {
                    kind: kind.clone(),
                    name,
                    file: file_rel,
                    action: "unchanged".to_string(),
                    detail: None,
                });
                continue;
            }
        }

        if dry_run {
            results.push(AgentGenResult {
                kind: kind.clone(),
                name,
                file: file_rel,
                action: "written".to_string(),
                detail: Some("(dry-run: non scritto)".to_string()),
            });
            continue;
        }

        // Path validation: deve restare sotto base.
        if !full_path.starts_with(&base) {
            results.push(AgentGenResult {
                kind: kind.clone(),
                name,
                file: file_rel,
                action: "error".to_string(),
                detail: Some("path fuori da output_dir".to_string()),
            });
            continue;
        }
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&full_path, &content).await {
            Ok(_) => results.push(AgentGenResult {
                kind: kind.clone(),
                name,
                file: file_rel,
                action: "written".to_string(),
                detail: None,
            }),
            Err(e) => results.push(AgentGenResult {
                kind: kind.clone(),
                name,
                file: file_rel,
                action: "error".to_string(),
                detail: Some(e.to_string()),
            }),
        }
    }
    Ok(results)
}

// ── Handler REST ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct RegenerateBody {
    #[serde(default)]
    pub force_overwrite_unmanaged: bool,
}

/// GET /api/claude-agents/preview — dry-run, nessuna scrittura.
pub async fn preview_handler(State(state): State<AppState>) -> impl IntoResponse {
    match regenerate_all(&state, true, false).await {
        Ok(r) => (StatusCode::OK, Json(json!({"dry_run": true, "agents": r}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/claude-agents/regenerate — scrive i file (rispetta marker).
pub async fn regenerate_handler(
    State(state): State<AppState>,
    body: Option<Json<RegenerateBody>>,
) -> impl IntoResponse {
    let force = body
        .map(|Json(b)| b.force_overwrite_unmanaged)
        .unwrap_or(false);
    match regenerate_all(&state, false, force).await {
        Ok(r) => (
            StatusCode::OK,
            Json(json!({"dry_run": false, "force": force, "agents": r})),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_map_degrada_semantici_a_grep_e_dedup() {
        let wl = vec![
            "read_file".to_string(),
            "write_file".to_string(),
            "edit_file".to_string(),
            "run_command".to_string(),
            "search_codebase_semantic".to_string(),
            "search_in_files".to_string(),
            "list_files".to_string(),
            "nexus_todo_write".to_string(),
            "knowledge_search".to_string(),
        ];
        let out = map_mcp_to_claude(&wl);
        // ordine canonico, dedup di Grep (search_codebase_semantic + search_in_files)
        assert_eq!(
            out,
            vec!["Read", "Edit", "Write", "Grep", "Glob", "Bash", "TodoWrite"]
        );
        // knowledge_search non mappato
    }

    #[test]
    fn render_non_contiene_campo_model() {
        let c = render_agent_file(
            "nexus-rust-implementer",
            "rust_implementer",
            "desc",
            &["Read", "Edit", "Bash"],
            "<role>x</role>",
            "abcd1234",
        );
        assert!(c.contains(MARKER));
        assert!(c.contains("name: nexus-rust-implementer"));
        assert!(c.contains("tools: Read, Edit, Bash"));
        assert!(
            !c.contains("\nmodel:"),
            "il file CLI non deve serializzare model"
        );
    }
}
