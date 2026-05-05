//! Tool Git: status, stage, commit, push, pull.

use super::*;

pub(super) async fn tool_git_status(ctx: &AgentToolContext) -> String {
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

pub(super) async fn tool_git_stage(ctx: &AgentToolContext, input: &Value) -> String {
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

pub(super) async fn tool_git_commit(ctx: &AgentToolContext, input: &Value) -> String {
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
            // Re-indicizza i file modificati nel commit in background
            let db_bg = ctx.db.clone();
            let neural_bg = ctx.neural.clone();
            let project_id_bg = ctx.project_id;
            let root_bg = ctx.root_path.clone();
            tokio::spawn(async move {
                // Recupera i file dell'ultimo commit
                if let Ok((diff_out, _)) = run_git_command(
                    &root_bg,
                    &["diff-tree", "--no-commit-id", "-r", "--name-only", "HEAD"],
                ).await {
                    for line in diff_out.lines() {
                        let file_path = root_bg.join(line.trim());
                        if file_path.exists() {
                            let _ = crate::projects::reindex_single_file(
                                &db_bg, &neural_bg, project_id_bg, &root_bg, &file_path,
                            ).await;
                        }
                    }
                }
            });
            stdout.trim().to_string()
        }
        Err(e) => format!("[git commit error: {}]", e),
    }
}

pub(super) async fn tool_git_push(ctx: &AgentToolContext) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    match run_git_command(&ctx.root_path, &["push"]).await {
        Ok((stdout, stderr)) => {
            let out = if stdout.trim().is_empty() { stderr } else { stdout };
            out.trim().to_string()
        }
        Err(e) => format!("[git push error: {}]", e),
    }
}

pub(super) async fn tool_git_pull(ctx: &AgentToolContext) -> String {
    if !ctx.is_git_repo {
        return "Il progetto non e' un repository git.".to_string();
    }
    match run_git_command(&ctx.root_path, &["pull", "--rebase"]).await {
        Ok((stdout, _)) => stdout.trim().to_string(),
        Err(e) => format!("[git pull error: {}]", e),
    }
}
