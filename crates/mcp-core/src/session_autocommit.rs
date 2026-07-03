//! Auto-commit per sessione su branch dedicato `nexus/session/<short_id>`.
//!
//! Rete di sicurezza secondaria sopra `file_mutations` (mig 0349): ogni
//! mutazione file riuscita dell'agente produce anche un commit git su un
//! branch isolato. Permette `git diff` cumulativi della sessione e sopravvive
//! anche se il DB perde dati.
//!
//! Design:
//!   - Branch dedicato per sessione: niente touch al branch dell'utente.
//!     HEAD non si muove, l'index utente non viene toccato.
//!   - Plumbing con GIT_INDEX_FILE temporaneo: usa un index file separato +
//!     write-tree + commit-tree + update-ref. Nessun side-effect.
//!   - Nessun push remoto: restiamo locali (regola H).
//!   - No-op silenzioso se non e' un repo git, setting disabilitato o sessione
//!     assente. Fail-loud nei log, ma non blocca la mutazione gia' avvenuta.
//!
//! Config DB (regola G):
//!   - `agent.autocommit.enabled` (default true): kill switch.
//!   - `agent.autocommit.branch_prefix` (default `nexus/session/`): namespace.

use sqlx::PgPool;
use std::path::Path;
use std::process::Stdio;
use uuid::Uuid;

const DEFAULT_BRANCH_PREFIX: &str = "nexus/session/";
const AUTHOR_NAME: &str = "Nexus Agent";
const AUTHOR_EMAIL: &str = "agent@nexus.local";

#[derive(Debug, Clone)]
pub struct AutocommitConfig {
    pub enabled: bool,
    pub branch_prefix: String,
}

impl Default for AutocommitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            branch_prefix: DEFAULT_BRANCH_PREFIX.to_string(),
        }
    }
}

pub async fn load_config(db: &PgPool) -> AutocommitConfig {
    let enabled = nexus_auth::get_bool_setting(db, "agent.autocommit.enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(true);
    let branch_prefix = nexus_auth::get_setting(db, "agent.autocommit.branch_prefix")
        .await
        .unwrap_or_else(|| DEFAULT_BRANCH_PREFIX.to_string());
    AutocommitConfig {
        enabled,
        branch_prefix,
    }
}

fn short_session(session_id: Uuid) -> String {
    let s = session_id.simple().to_string();
    s.chars().take(8).collect()
}

async fn git(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, (i32, String)> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = match cmd.output().await {
        Ok(o) => o,
        Err(e) => return Err((-1, format!("spawn git: {e}"))),
    };
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err((code, stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn git_with_stdin(
    root: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    input: &str,
) -> Result<String, (i32, String)> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err((-1, format!("spawn git: {e}"))),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes()).await;
        drop(stdin);
    }
    let out = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return Err((-1, format!("wait git: {e}"))),
    };
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err((code, stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Snapshotta la working tree su un commit del branch `nexus/session/<short>`
/// senza toccare l'index dell'utente ne' HEAD. `relative_path_hint` finisce
/// nel messaggio di commit; il contenuto e' l'INTERA working tree.
///
/// SOPPRESSIONE FASE 2 (buco B2): quando `isolated_subrun` e' `true` (il ctx e'
/// un sub-run in git worktree effimero) l'autocommit e' un NO-OP immediato. L'index
/// temp (`nexus-autocommit-{short}.idx`) e il branch ref (`nexus/session/{short}`)
/// sono keyed sulla SESSIONE, condivisi da tutti i sub-run del batch (stessa
/// session_id) e residenti nell'object store `.git` condiviso dai worktree: N
/// sub-run paralleli si corromperebbero index e ref a vicenda. Per i sub-run
/// isolati l'UNICA fonte di verita' del commit e' l'apply atomico serializzato
/// post-run (PR4), quindi l'autocommit intra-worktree e' ridondante e dannoso.
pub async fn snapshot_after_mutation(
    db: &PgPool,
    project_root: &Path,
    is_git_repo: bool,
    session_id: Option<Uuid>,
    isolated_subrun: bool,
    op: &str,
    relative_path_hint: &str,
) {
    if !is_git_repo {
        return;
    }
    // Sub-run isolato: autocommit soppresso (buco B2). L'apply atomico post-run
    // (PR4) e' l'unica fonte del commit; niente contesa su index/ref di sessione.
    if isolated_subrun {
        return;
    }
    let Some(sid) = session_id else {
        return;
    };

    let cfg = load_config(db).await;
    if !cfg.enabled {
        return;
    }

    let short = short_session(sid);
    let prefix = cfg.branch_prefix.trim_end_matches('/').to_string();
    let branch_with_slash = format!("{prefix}/{short}");
    let branch_ref = format!("refs/heads/{branch_with_slash}");

    // Index file temporaneo per sessione, riusato fra mutazioni successive.
    let tmp_index = std::env::temp_dir().join(format!("nexus-autocommit-{short}.idx"));
    let tmp_index_str = tmp_index.to_string_lossy().to_string();
    let env: &[(&str, &str)] = &[("GIT_INDEX_FILE", tmp_index_str.as_str())];

    // 1) bootstrap index temp da HEAD (idempotente: copre anche rebase utente)
    if let Err((code, err)) = git(project_root, &["read-tree", "HEAD"], env).await {
        tracing::warn!(
            session = %short, code, "session_autocommit: read-tree HEAD fallito: {err}"
        );
        return;
    }

    // 2) stage del singolo file (-A copre anche delete/rename)
    if let Err((code, err)) = git(project_root, &["add", "-A", "--", relative_path_hint], env).await
    {
        tracing::warn!(
            session = %short, code, file = %relative_path_hint,
            "session_autocommit: add fallito: {err}"
        );
        return;
    }

    // 3) write-tree dall'index temp
    let tree_out = match git(project_root, &["write-tree"], env).await {
        Ok(s) => s,
        Err((code, err)) => {
            tracing::warn!(
                session = %short, code, "session_autocommit: write-tree fallito: {err}"
            );
            return;
        }
    };
    let tree = tree_out.trim();

    // 4) parent: ultimo commit del branch nexus se esiste, altrimenti HEAD
    let parent_out = git(project_root, &["rev-parse", &branch_ref], env).await;
    let parent = match parent_out {
        Ok(s) => s.trim().to_string(),
        Err(_) => match git(project_root, &["rev-parse", "HEAD"], env).await {
            Ok(s) => s.trim().to_string(),
            Err((code, err)) => {
                tracing::warn!(
                    session = %short, code,
                    "session_autocommit: rev-parse HEAD fallito: {err}"
                );
                return;
            }
        },
    };

    // Idempotenza: se il tree e' identico al parent non creiamo un commit vuoto
    if let Ok(parent_tree_out) = git(
        project_root,
        &["rev-parse", &format!("{parent}^{{tree}}")],
        env,
    )
    .await
    {
        if parent_tree_out.trim() == tree {
            return;
        }
    }

    // 5) commit-tree con messaggio da stdin, autore = Nexus Agent
    let msg = format!("agent: {op} {relative_path_hint} (session {short})");
    let commit_env: &[(&str, &str)] = &[
        ("GIT_INDEX_FILE", tmp_index_str.as_str()),
        ("GIT_AUTHOR_NAME", AUTHOR_NAME),
        ("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL),
        ("GIT_COMMITTER_NAME", AUTHOR_NAME),
        ("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL),
    ];
    let commit_out = match git_with_stdin(
        project_root,
        &["commit-tree", tree, "-p", &parent, "-F", "-"],
        commit_env,
        &msg,
    )
    .await
    {
        Ok(s) => s,
        Err((code, err)) => {
            tracing::warn!(
                session = %short, code,
                "session_autocommit: commit-tree fallito: {err}"
            );
            return;
        }
    };
    let new_commit = commit_out.trim();

    // 6) update-ref: fa avanzare il branch nexus al nuovo commit
    if let Err((code, err)) = git(project_root, &["update-ref", &branch_ref, new_commit], env).await
    {
        tracing::warn!(
            session = %short, code, branch = %branch_with_slash,
            "session_autocommit: update-ref fallito: {err}"
        );
        return;
    }

    tracing::debug!(
        session = %short, branch = %branch_with_slash, commit = %new_commit,
        file = %relative_path_hint, op = %op,
        "session_autocommit: snapshot creato"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Soppressione FASE 2 (buco B2): con `isolated_subrun=true` la funzione e' un
    /// NO-OP immediato — esce PRIMA di `load_config(db)` e di qualunque comando git.
    /// Lo verifichiamo con un pool lazy verso una porta chiusa e `is_git_repo=true`
    /// (cosi' NON si esce dal ramo `!is_git_repo`): se la guardia isolated non
    /// mordesse, `load_config` tenterebbe la connessione al DB irraggiungibile e la
    /// chiamata NON ritornerebbe entro il timeout stretto. La root inesistente
    /// prova in aggiunta che nessun comando git viene lanciato.
    #[tokio::test]
    async fn isolated_subrun_sopprime_autocommit_noop() {
        let db = PgPool::connect_lazy("postgres://x:x@127.0.0.1:1/x").expect("pool lazy");
        let fake_root = Path::new("/percorso/che/non/esiste/nexus-test");
        let sid = Uuid::new_v4();

        let fut = snapshot_after_mutation(
            &db,
            fake_root,
            /* is_git_repo */ true,
            Some(sid),
            /* isolated_subrun */ true,
            "create",
            "src/lib.rs",
        );
        // No-op: ritorna subito (nessun connect DB, nessun spawn git).
        let res = tokio::time::timeout(Duration::from_secs(3), fut).await;
        assert!(
            res.is_ok(),
            "isolated_subrun=true deve essere no-op immediato (nessun I/O DB/git)"
        );
    }
}
