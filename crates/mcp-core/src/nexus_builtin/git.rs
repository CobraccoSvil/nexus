//! Handler per il gruppo `git_advanced` del server Nexus Builtin.
//!
//! Gestisce log, diff, branch, checkout e creazione branch tramite
//! il comando `git` eseguito nella root del progetto.

use super::*;

pub(super) async fn handle_git_log(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).min(100);
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let n_str = limit.to_string();
    match run_git(&root, &["log", &format!("-{}", n_str), "--pretty=format:%H|%an|%ae|%ai|%s", "--no-merges"]).await {
        Ok(out) => {
            let commits: Vec<Value> = out.lines().filter(|l| !l.is_empty()).map(|l| {
                let parts: Vec<&str> = l.splitn(5, '|').collect();
                json!({
                    "hash": parts.first().copied().unwrap_or(""),
                    "author": parts.get(1).copied().unwrap_or(""),
                    "email": parts.get(2).copied().unwrap_or(""),
                    "date": parts.get(3).copied().unwrap_or(""),
                    "message": parts.get(4).copied().unwrap_or(""),
                })
            }).collect();
            format_json(&json!({ "commits": commits, "count": commits.len() }))
        }
        Err(e) => e,
    }
}

pub(super) async fn handle_git_diff(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let path = args.get("path").and_then(Value::as_str);
    let git_args: Vec<&str> = if let Some(p) = path {
        vec!["diff", "--", p]
    } else {
        vec!["diff"]
    };
    match run_git(&root, &git_args).await {
        Ok(diff) if diff.is_empty() => "Nessuna modifica non committata.".to_string(),
        Ok(diff) => diff,
        Err(e) => e,
    }
}

pub(super) async fn handle_git_branches(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    match run_git(&root, &["branch", "-a", "--format=%(refname:short)|%(HEAD)"]).await {
        Ok(out) => {
            let branches: Vec<Value> = out.lines().filter(|l| !l.is_empty()).map(|l| {
                let parts: Vec<&str> = l.splitn(2, '|').collect();
                let name = parts.first().copied().unwrap_or("").trim_start_matches("* ").to_string();
                let is_current = parts.get(1).copied().unwrap_or("") == "*";
                json!({ "name": name, "current": is_current })
            }).collect();
            format_json(&json!({ "branches": branches }))
        }
        Err(e) => e,
    }
}

pub(super) async fn handle_git_checkout(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let branch = match args.get("branch").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => return "[Errore] Parametro 'branch' obbligatorio".to_string(),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    match run_git(&root, &["checkout", &branch]).await {
        Ok(_) => format_json(&json!({ "ok": true, "branch": branch })),
        Err(e) => e,
    }
}

pub(super) async fn handle_git_create_branch(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return e,
    };
    let branch_name = match args.get("branch_name").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => return "[Errore] Parametro 'branch_name' obbligatorio".to_string(),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    match run_git(&root, &["checkout", "-b", &branch_name]).await {
        Ok(_) => format_json(&json!({ "ok": true, "branch": branch_name })),
        Err(e) => e,
    }
}
