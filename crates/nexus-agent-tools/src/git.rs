//! Tool Git: status, stage, commit, push, pull.

use nexus_types::git_exec::run_git_command;
use serde_json::Value;

use super::ToolContextCore;

pub async fn tool_git_status(ctx: &ToolContextCore) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    match run_git_command(&ctx.root_path, &["status", "--porcelain=v1", "-b"]).await {
        Ok((stdout, _)) => {
            if stdout.trim().is_empty() {
                "Repository pulito, nessuna modifica pendente.".to_string()
            } else {
                stdout
            }
        }
        Err(e) => format!("[git status error: {}]", e),
    }
}

pub async fn tool_git_stage(ctx: &ToolContextCore, input: &Value) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let paths: Vec<String> = match input.get("paths").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect(),
        None => return "[Errore: parametro 'paths' mancante o non valido]".to_string(),
    };
    if paths.is_empty() {
        return "[Errore: 'paths' vuoto]".to_string();
    }
    let mut args = vec!["add"];
    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    args.extend(path_refs.iter().copied());
    match run_git_command(&ctx.root_path, &args).await {
        Ok(_) => format!("Staged: {}", paths.join(", ")),
        Err(e) => format!("[git add error: {}]", e),
    }
}

pub async fn tool_git_commit(ctx: &ToolContextCore, input: &Value) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let message = match input.get("message").and_then(Value::as_str) {
        Some(s) => s,
        None => return "[Errore: parametro 'message' mancante]".to_string(),
    };
    match run_git_command(&ctx.root_path, &["commit", "-m", message]).await {
        Ok((stdout, _)) => {
            // Dispatcher: notifica GitStatusChanged → pannello Git aggiorna branch + counts
            if let Ok((status_out, _)) =
                run_git_command(&ctx.root_path, &["status", "--porcelain=v1", "-b"]).await
            {
                let branch = status_out
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("## "))
                    .map(|s| s.split("...").next().unwrap_or(s).to_string())
                    .unwrap_or_default();
                let modified_count = status_out
                    .lines()
                    .skip(1)
                    .filter(|l| !l.trim().is_empty())
                    .count();
                nexus_events::dispatcher::emit(
                    &ctx.project_channels,
                    ctx.project_id,
                    nexus_events::event::ProjectEvent::GitStatusChanged {
                        branch,
                        ahead: 0,
                        behind: 0,
                        modified_count: modified_count as i32,
                    },
                );
            }
            // Re-indicizza i file modificati nel commit in background
            // (contratto FileReindexer: l'impl mcp-core delega a reindex_single_file).
            let reindexer = ctx.reindexer.clone();
            let project_id_bg = ctx.project_id;
            let root_bg = ctx.root_path.clone();
            tokio::spawn(async move {
                // Recupera i file dell'ultimo commit
                if let Ok((diff_out, _)) = run_git_command(
                    &root_bg,
                    &["diff-tree", "--no-commit-id", "-r", "--name-only", "HEAD"],
                )
                .await
                {
                    for line in diff_out.lines() {
                        let file_path = root_bg.join(line.trim());
                        if file_path.exists() {
                            reindexer
                                .reindex_file(project_id_bg, root_bg.clone(), file_path)
                                .await;
                        }
                    }
                }
            });
            stdout.trim().to_string()
        }
        Err(e) => format!("[git commit error: {}]", e),
    }
}

pub async fn tool_git_push(ctx: &ToolContextCore) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    match run_git_command(&ctx.root_path, &["push"]).await {
        Ok((stdout, stderr)) => {
            let out = if stdout.trim().is_empty() {
                stderr
            } else {
                stdout
            };
            out.trim().to_string()
        }
        Err(e) => format!("[git push error: {}]", e),
    }
}

pub async fn tool_git_pull(ctx: &ToolContextCore) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    match run_git_command(&ctx.root_path, &["pull", "--rebase"]).await {
        Ok((stdout, _)) => stdout.trim().to_string(),
        Err(e) => format!("[git pull error: {}]", e),
    }
}

/// Fix M16: configura un remote git (es. `origin`) puntando a un URL.
/// Tool agente che evita all'agente di usare `run_command git remote add ...` shell.
/// Input: `{name: string, url: string}` (default name = "origin")
pub async fn tool_git_remote_add(ctx: &ToolContextCore, input: &Value) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("origin")
        .trim();
    let url = match input.get("url").and_then(Value::as_str) {
        Some(u) if !u.trim().is_empty() => u.trim(),
        _ => return "[Errore: parametro 'url' obbligatorio]".to_string(),
    };

    // Validazione: name puro alfanumerico/underscore/dash, no path traversal
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return format!(
            "[Errore: nome remote non valido '{}' (solo alfanumerico/-/_)]",
            name
        );
    }

    // Validazione: url deve essere https:// o git@ (no file:// path locali per evitare leak)
    if !url.starts_with("https://") && !url.starts_with("git@") && !url.starts_with("ssh://") {
        return format!(
            "[Errore: url remote deve iniziare con https://, git@ o ssh:// (rifiutato: '{}')]",
            url
        );
    }

    // Se il remote esiste gia: rimuovilo e ricrealo (idempotente)
    let _ = run_git_command(&ctx.root_path, &["remote", "remove", name]).await;

    match run_git_command(&ctx.root_path, &["remote", "add", name, url]).await {
        Ok(_) => {
            // Verifica con git remote -v
            match run_git_command(&ctx.root_path, &["remote", "-v"]).await {
                Ok((stdout, _)) => format!(
                    "Remote '{}' configurato verso {}.\n\nStato remote:\n{}",
                    name,
                    url,
                    stdout.trim()
                ),
                Err(_) => format!("Remote '{}' configurato verso {}.", name, url),
            }
        }
        Err(e) => format!("[git remote add error: {}]", e),
    }
}
