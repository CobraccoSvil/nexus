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

    // Tip del branch di sessione, se la sessione ne ha gia' prodotto uno: e' la
    // base del cumulo (passo 1) E il parent del nuovo commit (passo 4). Una sola
    // interrogazione per entrambi: due `rev-parse` distinti potrebbero osservare
    // due valori diversi e comporre un commit la cui base non e' il suo parent.
    let session_tip = git(project_root, &["rev-parse", &branch_ref], env)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // 1) bootstrap dell'index temp.
    //
    // La base e' il TIP DEL BRANCH DI SESSIONE, non HEAD: il presidio promette
    // un cumulo (`git diff` cumulativi della sessione, doc di modulo) e con
    // `read-tree HEAD` incondizionato non lo era. L'index ripartiva da HEAD a
    // ogni mutazione e vi si stagiava il solo file appena scritto, quindi il
    // tree del commit N riportava ogni ALTRO file alla versione di HEAD: la
    // catena dei parent era corretta (N -> N-1) e faceva sembrare cumulativa
    // una storia in cui ogni commit annullava il precedente.
    //
    // Il ri-ancoraggio a HEAD non sparisce, diventa una CONDIZIONE: se l'utente
    // committa sotto la sessione, HEAD smette di essere antenato del branch e
    // ripartire dal tip riporterebbe indietro il lavoro dell'utente. La domanda
    // la pone git (`merge-base --is-ancestor`), non un confronto fra hash: fra
    // HEAD e il tip puo' esserci un numero qualunque di commit.
    let base = match &session_tip {
        Some(tip)
            if git(
                project_root,
                &["merge-base", "--is-ancestor", "HEAD", tip],
                env,
            )
            .await
            .is_ok() =>
        {
            tip.as_str()
        }
        _ => "HEAD",
    };
    if let Err((code, err)) = git(project_root, &["read-tree", base], env).await {
        tracing::warn!(
            session = %short, code, base = %base,
            "session_autocommit: read-tree fallito: {err}"
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

    // 4) parent: ultimo commit del branch nexus se esiste, altrimenti HEAD.
    // Riusa il tip letto al passo 1: la storia resta lineare anche quando la
    // base e' tornata a HEAD (ri-ancoraggio), perche' il branch non deve
    // perdere i propri commit precedenti — solo smettere di riproporne il tree.
    let parent = match &session_tip {
        Some(tip) => tip.clone(),
        None => match git(project_root, &["rev-parse", "HEAD"], env).await {
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
    use std::path::PathBuf;
    use std::time::Duration;

    /// git SINCRONO per il setup del repo di prova. Fuori dal path di produzione:
    /// qui `expect` e' ammesso (regola F).
    // `git_sync` e `temp_repo` erano duplicate identiche nel `mod tests` di
    // `nexus-tool-kit::worktree`. La definizione vive dal 2026-08-05 in
    // `nexus-test-preconditions`, accanto a `seed_project_meta`: quel crate e'
    // sotto entrambi nel grafo, ed e' l'unico posto da cui li raggiungono
    // tutti e due senza invertire una dipendenza.
    use nexus_test_preconditions::{git_sync, temp_repo};

    /// Come [`git_sync`] ma ritorna stdout: serve a INTERROGARE il repo dopo che
    /// la produzione l'ha scritto, invece di dedurne lo stato (regola O).
    fn git_out(cwd: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {args:?} fallito in {cwd:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }


    /// L'index temporaneo vive in `std::env::temp_dir()`, FUORI dal tempdir del
    /// test: senza questa pulizia ogni esecuzione ne lascerebbe uno.
    struct IndexTemp(PathBuf);
    impl Drop for IndexTemp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Semina i due settings che [`load_config`] legge davvero. Il prefisso NON e'
    /// quello di default apposta: se la lettura dal DB non avvenisse, il branch
    /// nascerebbe altrove e `ls-tree` fallirebbe invece di passare in silenzio.
    async fn crea_settings(pool: &PgPool, prefix: &str) {
        crate::test_support::create_settings_table_with(pool, "agent.autocommit.enabled", "true")
            .await;
        crate::test_support::seed_setting(pool, "agent.autocommit.branch_prefix", prefix).await;
    }

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

    /// La rete di sicurezza e' CUMULATIVA: il tree dell'ultimo commit di sessione
    /// contiene TUTTE le mutazioni della sessione, non solo l'ultima.
    ///
    /// E' la promessa del doc di modulo ("`git diff` cumulativi della sessione")
    /// ed e' l'unica che rende utile il presidio nel caso per cui esiste: il DB
    /// perde le righe di `file_mutations` e il branch resta l'unica copia.
    ///
    /// Il difetto misurato: l'index temporaneo ripartiva da `HEAD` a ogni
    /// mutazione e vi si stagiava il SOLO file appena scritto, quindi il commit N
    /// riportava ogni altro file alla versione di HEAD. La catena dei parent era
    /// corretta (N -> N-1) e faceva sembrare cumulativa una storia in cui ogni
    /// commit ANNULLAVA il precedente: `git log` mostrava tre commit,
    /// `git diff HEAD..<branch>` un file solo.
    ///
    /// Il test attraversa la produzione per intero (regola O): repo git vero,
    /// settings letti dal DB, e la verifica interroga git (`ls-tree`, `show`)
    /// invece di dedurre lo stato dalle chiamate fatte.
    #[sqlx::test]
    async fn il_branch_di_sessione_accumula_tutte_le_mutazioni(pool: PgPool) {
        let prefix = "nexus-test/sessione/";
        crea_settings(&pool, prefix).await;

        let (_td, root) = temp_repo();
        let sid = Uuid::new_v4();
        let short = short_session(sid);
        let _idx = IndexTemp(std::env::temp_dir().join(format!("nexus-autocommit-{short}.idx")));
        let branch = format!("refs/heads/{}{short}", prefix);

        // Tre mutazioni della stessa sessione: due file distinti, poi il ritocco
        // del primo (il caso "modify" dopo che un ALTRO file e' stato toccato).
        std::fs::write(root.join("primo.txt"), "alfa\n").expect("write primo");
        snapshot_after_mutation(&pool, &root, true, Some(sid), false, "create", "primo.txt").await;

        std::fs::write(root.join("secondo.txt"), "beta\n").expect("write secondo");
        snapshot_after_mutation(&pool, &root, true, Some(sid), false, "create", "secondo.txt").await;

        std::fs::write(root.join("primo.txt"), "alfa corretto\n").expect("rewrite primo");
        snapshot_after_mutation(&pool, &root, true, Some(sid), false, "modify", "primo.txt").await;

        let elencati = git_out(&root, &["ls-tree", "-r", "--name-only", &branch]);
        let presenti: Vec<&str> = elencati
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();

        assert!(
            presenti.contains(&"secondo.txt"),
            "il tree dell'ultimo commit di sessione deve conservare le mutazioni \
             PRECEDENTI, non solo l'ultima; contenuto: {presenti:?}"
        );
        assert!(
            presenti.contains(&"primo.txt"),
            "il file mutato per ultimo deve esserci; contenuto: {presenti:?}"
        );

        // Non basta il nome: il cumulo deve portare il contenuto AGGIORNATO del
        // file ritoccato, non la sua prima versione.
        let primo = git_out(&root, &["show", &format!("{branch}:primo.txt")]);
        assert_eq!(primo, "alfa corretto\n", "l'ultima versione del file mutato");
        let secondo = git_out(&root, &["show", &format!("{branch}:secondo.txt")]);
        assert_eq!(secondo, "beta\n", "la mutazione intermedia, intatta");

        // La storia resta lineare: un commit per mutazione, sopra il commit
        // iniziale dell'utente.
        let n = git_out(&root, &["rev-list", "--count", &branch]);
        assert_eq!(n.trim(), "4", "3 mutazioni + il commit iniziale");
    }

    /// Il ri-ancoraggio a HEAD non e' scomparso col cumulo: quando l'utente
    /// COMMITTA sotto la sessione (HEAD non e' piu' antenato del branch), la base
    /// torna a HEAD, cosi' lo snapshot successivo riflette il lavoro dell'utente
    /// invece di riportarlo indietro.
    ///
    /// E' la ragione per cui il `read-tree HEAD` era incondizionato; qui e'
    /// conservata come CONDIZIONE, non come reset a ogni mutazione.
    #[sqlx::test]
    async fn head_che_avanza_riancora_lo_snapshot(pool: PgPool) {
        let prefix = "nexus-test/sessione/";
        crea_settings(&pool, prefix).await;

        let (_td, root) = temp_repo();
        let sid = Uuid::new_v4();
        let short = short_session(sid);
        let _idx = IndexTemp(std::env::temp_dir().join(format!("nexus-autocommit-{short}.idx")));
        let branch = format!("refs/heads/{}{short}", prefix);

        std::fs::write(root.join("agente.txt"), "scritto dall'agente\n").expect("write agente");
        snapshot_after_mutation(&pool, &root, true, Some(sid), false, "create", "agente.txt").await;

        // L'utente committa sul PROPRIO branch: HEAD avanza e smette di essere
        // antenato del branch di sessione.
        std::fs::write(root.join("utente.txt"), "scritto dall'utente\n").expect("write utente");
        git_sync(&root, &["add", "-A"]);
        git_sync(&root, &["commit", "-q", "-m", "lavoro dell'utente"]);

        std::fs::write(root.join("agente2.txt"), "ancora l'agente\n").expect("write agente2");
        snapshot_after_mutation(&pool, &root, true, Some(sid), false, "create", "agente2.txt").await;

        let elencati = git_out(&root, &["ls-tree", "-r", "--name-only", &branch]);
        let presenti: Vec<&str> = elencati
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(
            presenti.contains(&"utente.txt"),
            "dopo un commit dell'utente lo snapshot riparte da HEAD e ne include \
             il lavoro; contenuto: {presenti:?}"
        );
        assert!(
            presenti.contains(&"agente2.txt"),
            "la mutazione in corso resta nello snapshot; contenuto: {presenti:?}"
        );
    }
}
