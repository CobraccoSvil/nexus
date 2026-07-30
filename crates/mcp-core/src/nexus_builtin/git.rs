//! Handler per il gruppo `git_advanced` del server Nexus Builtin.
//!
//! Gestisce log, diff, branch, checkout e creazione branch tramite
//! il comando `git` eseguito nella root del progetto.

use super::*;

pub(super) async fn handle_git_log(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(100);
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return tool_failure(e),
    };
    let n_str = limit.to_string();
    match run_git(
        &root,
        &[
            "log",
            &format!("-{}", n_str),
            "--pretty=format:%H|%an|%ae|%ai|%s",
            "--no-merges",
        ],
    )
    .await
    {
        Ok(out) => {
            let commits: Vec<Value> = out
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| {
                    let parts: Vec<&str> = l.splitn(5, '|').collect();
                    json!({
                        "hash": parts.first().copied().unwrap_or(""),
                        "author": parts.get(1).copied().unwrap_or(""),
                        "email": parts.get(2).copied().unwrap_or(""),
                        "date": parts.get(3).copied().unwrap_or(""),
                        "message": parts.get(4).copied().unwrap_or(""),
                    })
                })
                .collect();
            format_json(&json!({ "commits": commits, "count": commits.len() }))
        }
        Err(e) => tool_failure(e),
    }
}

pub(super) async fn handle_git_diff(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return tool_failure(e),
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
        Err(e) => tool_failure(e),
    }
}

pub(super) async fn handle_git_branches(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return tool_failure(e),
    };
    match run_git(
        &root,
        &["branch", "-a", "--format=%(refname:short)|%(HEAD)"],
    )
    .await
    {
        Ok(out) => {
            let branches: Vec<Value> = out
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| {
                    let parts: Vec<&str> = l.splitn(2, '|').collect();
                    let name = parts
                        .first()
                        .copied()
                        .unwrap_or("")
                        .trim_start_matches("* ")
                        .to_string();
                    let is_current = parts.get(1).copied().unwrap_or("") == "*";
                    json!({ "name": name, "current": is_current })
                })
                .collect();
            format_json(&json!({ "branches": branches }))
        }
        Err(e) => tool_failure(e),
    }
}

pub(super) async fn handle_git_checkout(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let branch = match args.get("branch").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'branch' obbligatorio"),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return tool_failure(e),
    };
    // Mutatore: un checkout fallito NON deve leggersi come successo, o
    // l'anti-loop insiste pensando che il branch sia gia' stato cambiato.
    match run_git(&root, &["checkout", &branch]).await {
        Ok(_) => format_json(&json!({ "ok": true, "branch": branch })),
        Err(e) => tool_failure(e),
    }
}

pub(super) async fn handle_git_create_branch(db: &PgPool, args: &Value) -> String {
    let project_id = match parse_uuid(args, "project_id") {
        Ok(id) => id,
        Err(e) => return tool_failure(e),
    };
    let branch_name = match args.get("branch_name").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => return tool_failure("[Errore] Parametro 'branch_name' obbligatorio"),
    };
    let root = match get_project_root(db, project_id).await {
        Ok(r) => r,
        Err(e) => return tool_failure(e),
    };
    // Mutatore: una creazione branch fallita (nome duplicato, ref invalido)
    // deve dichiararsi fallita, non "ok":true con un branch mai creato.
    match run_git(&root, &["checkout", "-b", &branch_name]).await {
        Ok(_) => format_json(&json!({ "ok": true, "branch": branch_name })),
        Err(e) => tool_failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pool lazy MAI connesso: sufficiente per i rami che falliscono prima di
    /// qualunque `.await` su `db` (parse_uuid, validazione parametri). Un
    /// pool lazy realmente INTERROGATO impiega il timeout di connessione
    /// (vedi `agent_tools::dispatch::ctx_for_dispatch_tests`, ~30s a
    /// chiamata): questi rami escono prima di interrogarlo.
    fn pool_mai_connesso() -> PgPool {
        PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy")
    }

    /// `parse_uuid` e' l'helper condiviso (non toccato da questo fix) che
    /// alimenta il primo ramo `Err` di tutti e 5 gli handler: chiamare
    /// l'handler reale con un `project_id` non-UUID esercita il PRODUTTORE
    /// vero, non una stringa fabbricata a mano (regola O).
    #[tokio::test]
    async fn project_id_invalido_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let out = handle_git_log(&db, &json!({ "project_id": "non-un-uuid" })).await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "project_id invalido deve dichiararsi fallito: {out}"
        );
    }

    /// Gemello del precedente sul ramo mutatore: un checkout con
    /// `project_id` invalido non deve mai leggersi come branch cambiato.
    #[tokio::test]
    async fn checkout_con_project_id_invalido_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let out = handle_git_checkout(
            &db,
            &json!({ "project_id": "non-un-uuid", "branch": "main" }),
        )
        .await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "checkout con project_id invalido deve dichiararsi fallito: {out}"
        );
    }

    /// Validazione del parametro `branch`: fallisce PRIMA di toccare `db`
    /// (nessun `.await` su `get_project_root` raggiunto), quindi il pool
    /// lazy mai connesso basta. Un checkout mutatore senza branch valido non
    /// deve MAI apparire riuscito all'anti-loop.
    #[tokio::test]
    async fn checkout_senza_branch_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let project_id = Uuid::new_v4().to_string();
        let out = handle_git_checkout(&db, &json!({ "project_id": project_id })).await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "branch mancante deve dichiararsi fallito: {out}"
        );
    }

    /// Stesso ramo di validazione per `handle_git_create_branch`: una
    /// creazione branch mai partita non deve apparire come `ok:true`.
    #[tokio::test]
    async fn create_branch_senza_branch_name_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let project_id = Uuid::new_v4().to_string();
        let out = handle_git_create_branch(&db, &json!({ "project_id": project_id })).await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "branch_name mancante deve dichiararsi fallito: {out}"
        );
    }

    /// `run_git` e' il PRODUTTORE reale del ramo `Err` finale di tutti e 5
    /// gli handler (regola O: si chiama il subprocesso vero, non si
    /// fabbrica la stringa d'errore). Una directory che non e' un repo git
    /// fa fallire `git log` in modo genuino e veloce (nessuna rete, nessun
    /// DB): il fallimento del comando deve restare dichiarato dopo il wrap.
    #[tokio::test]
    async fn run_git_fallito_resta_un_fallimento_dopo_il_wrap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().to_string();
        let esito = run_git(&root, &["log", "-1"]).await;
        let e = esito.expect_err("una directory non-repo deve far fallire `git log`");
        let out = tool_failure(e);
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "l'esito del subprocesso fallito deve restare dichiarato dopo tool_failure: {out}"
        );
    }
}
