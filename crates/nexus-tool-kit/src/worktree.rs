//! Isolamento fisico dei sub-run agentici via **git worktree effimero**.
//!
//! Modulo self-contained (composition, non un tool esposto all'LLM) che fornisce
//! le primitive per creare, applicare e distruggere worktree git dedicati a un
//! singolo sub-run. Ogni sub-run isolato scrive nel proprio worktree; i cambiamenti
//! vengono poi promossi alla root del progetto in modo ATOMICO e SERIALIZZABILE.
//!
//! In PR2 il modulo NON e' cablato a runtime: nessun call site di produzione lo
//! invoca. Serve solo come fondamenta testabile (repo git temp) per PR4, dove
//! `tool_dispatch_subagents` diventera' il punto unico che orchestra worktree +
//! apply serializzato.
//!
//! ## Punto unico esecuzione comandi (regola L)
//! Tutte le invocazioni `git` passano da [`crate::exec::run_cmd`] (che sotto usa
//! `crate::sandbox::isolated_command`: env isolato + process group dedicato). Il
//! modulo non reimplementa lo spawn del subprocess.
//!
//! ## Collocazione worktree (regola E)
//! I worktree vivono SEMPRE sotto un'area controllata e per-run derivata dalla
//! root del progetto: `<project_root>/../.nexus-worktrees/<run_id>`. Mai in path
//! arbitrari fuori dal dominio del progetto. Vedi [`worktree_base_dir`].
//!
//! ## Apply atomico (regola H, mai toppe)
//! [`apply_worktree_atomic`] non usa mai `git apply` diretto sulla working tree
//! condivisa. Nel worktree fa `add -A` + `commit` (i worktree condividono l'object
//! store `.git`, quindi il commit e' visibile dalla root); poi nella root fa
//! `git merge --no-ff --no-commit <commit>`. Su conflitto fa `git merge --abort`,
//! riportando la root ESATTAMENTE allo stato pre-apply (rollback verificabile,
//! nessuna scrittura parziale). L'apply va SERIALIZZATO dal chiamante: un solo
//! worktree alla volta puo' essere applicato alla stessa root.

use crate::exec::{run_cmd, CmdOutput};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Timeout (secondi) per le singole invocazioni git di questo modulo. Le
/// operazioni sono locali (rev-parse, worktree add/remove, merge): un timeout
/// generoso copre repo grandi su Windows senza mai appendere all'infinito.
const GIT_TIMEOUT_SECS: u64 = 120;

/// Nome della directory base (sibling della project_root) sotto cui vivono tutti
/// i worktree effimeri Nexus. Sta un livello sopra la project_root perche' git
/// non consente di annidare un worktree dentro il working tree del repo stesso.
const WORKTREE_BASE_NAME: &str = ".nexus-worktrees";

/// Errore strutturato del modulo worktree. `thiserror` per il tipo, il chiamante
/// propaga con `?`. Nessun `unwrap`/`expect` fuori dai test.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// Un comando git e' terminato con exit code non-zero.
    #[error("git {op} fallito (exit={exit_code}): {stderr}")]
    Git {
        op: String,
        exit_code: i32,
        stderr: String,
    },

    /// Errore nell'esecuzione del subprocess git (binario mancante, timeout, IO).
    #[error("esecuzione git ({op}): {source}")]
    Exec {
        op: String,
        #[source]
        source: crate::NexusToolError,
    },

    /// La root indicata non ha un parent valido: impossibile derivare l'area
    /// worktree controllata (regola E).
    #[error("impossibile derivare la base worktree: la root {0} non ha directory parent")]
    NoParentDir(PathBuf),

    /// Errore di I/O nel setup dell'area worktree (creazione dir base).
    #[error("io area worktree: {0}")]
    Io(#[from] std::io::Error),
}

/// Alias di comodita' per i risultati del modulo.
pub type Result<T> = std::result::Result<T, WorktreeError>;

/// Handle di un worktree effimero associato a un singolo sub-run.
///
/// Trasporta tutto il necessario per applicare i cambiamenti alla root e per il
/// cleanup: `path` (dir del worktree), `base_commit` (SHA da cui il worktree e'
/// stato staccato, persistito per determinismo/replay), `project_root` (la root
/// reale a cui promuovere), `run_id` (identita' del sub-run), `branch` (branch
/// effimero del worktree).
#[derive(Debug, Clone)]
pub struct WorktreeHandle {
    /// Directory del worktree effimero (sotto l'area controllata, regola E).
    pub path: PathBuf,
    /// SHA del commit da cui il worktree e' stato creato. Persistito e riusato
    /// in apply/replay, mai ri-derivato da HEAD.
    pub base_commit: String,
    /// Root reale del progetto a cui i cambiamenti verranno promossi.
    pub project_root: PathBuf,
    /// Identita' del sub-run proprietario del worktree.
    pub run_id: Uuid,
    /// Branch effimero del worktree (`nexus-sub-<run_id>`).
    pub branch: String,
}

/// Esito dell'apply atomico dei cambiamenti di un worktree alla root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// I cambiamenti sono stati promossi e committati sulla root.
    Applied,
    /// Il merge ha prodotto conflitti: e' stato fatto `merge --abort`, la root e'
    /// intatta. `files` elenca i path in conflitto (best-effort dal diff).
    Conflict { files: Vec<String> },
    /// Il worktree non conteneva cambiamenti rispetto al `base_commit`: nulla da
    /// applicare.
    NoChanges,
}

/// Nome del branch effimero per un dato sub-run.
fn ephemeral_branch(run_id: Uuid) -> String {
    format!("nexus-sub-{run_id}")
}

/// Directory base (controllata, regola E) sotto cui allocare il worktree di un
/// sub-run: `<project_root>/../.nexus-worktrees`. Sta come sibling della root
/// perche' git non consente worktree annidati dentro il working tree del repo.
///
/// Ritorna [`WorktreeError::NoParentDir`] se la root non ha un parent (es. root
/// di un drive): fail-closed, il chiamante degradera' a Sequential.
pub fn worktree_base_dir(project_root: &Path) -> Result<PathBuf> {
    let parent = project_root
        .parent()
        .ok_or_else(|| WorktreeError::NoParentDir(project_root.to_path_buf()))?;
    Ok(parent.join(WORKTREE_BASE_NAME))
}

/// Directory del worktree per uno specifico sub-run:
/// `<project_root>/../.nexus-worktrees/<run_id>`.
fn worktree_path_for(project_root: &Path, run_id: Uuid) -> Result<PathBuf> {
    Ok(worktree_base_dir(project_root)?.join(run_id.to_string()))
}

/// Esegue un comando git in `cwd` e mappa un exit non-zero in
/// [`WorktreeError::Git`] (segnale strutturato dall'exit_code, regola M), mai dal
/// parsing del testo. Punto unico interno per il pattern run_cmd + check exit.
async fn git(op: &str, args: &[&str], cwd: &Path) -> Result<CmdOutput> {
    let out = run_cmd("git", args, cwd, GIT_TIMEOUT_SECS)
        .await
        .map_err(|source| WorktreeError::Exec {
            op: op.to_string(),
            source,
        })?;
    if !out.success() {
        return Err(WorktreeError::Git {
            op: op.to_string(),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    Ok(out)
}

/// Risolve `HEAD` a SHA nella root indicata: `git -C <root> rev-parse HEAD`.
pub async fn head_commit(root: &Path) -> Result<String> {
    let out = git("rev-parse HEAD", &["rev-parse", "HEAD"], root).await?;
    Ok(out.stdout.trim().to_string())
}

/// Verifica FAIL-CLOSED che `root` sia un repo git in cui `git worktree` e'
/// utilizzabile. Ritorna `true` solo se ENTRAMBI i probe hanno successo:
/// - `git -C root rev-parse --is-inside-work-tree` -> "true"
/// - `git -C root worktree list`
///
/// Su qualunque dubbio (comando che fallisce, binario mancante, timeout, output
/// inatteso, root non-git) ritorna `false`: il chiamante degradera' a Sequential.
/// Nessun errore propagato — il probe e' una capability query, non un'operazione.
pub async fn probe_isolatable(root: &Path) -> bool {
    // Probe 1: siamo dentro un working tree git?
    match run_cmd(
        "git",
        &["rev-parse", "--is-inside-work-tree"],
        root,
        GIT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() && out.stdout.trim() == "true" => {}
        _ => return false,
    }
    // Probe 2: il subcomando worktree e' utilizzabile su questo repo?
    matches!(
        run_cmd("git", &["worktree", "list"], root, GIT_TIMEOUT_SECS).await,
        Ok(out) if out.success()
    )
}

/// Crea un worktree effimero DEDICATO al sub-run `run_id`, staccato da
/// `base_commit`, sotto l'area controllata `<project_root>/../.nexus-worktrees/`
/// (regola E). Usa un branch effimero `nexus-sub-<run_id>`:
/// `git -C <root> worktree add -b nexus-sub-<run_id> <tempdir> <base_commit>`.
///
/// La directory base viene creata se assente. Ritorna l'handle per apply/cleanup.
pub async fn create_ephemeral_worktree(
    root: &Path,
    run_id: Uuid,
    base_commit: &str,
) -> Result<WorktreeHandle> {
    let base_dir = worktree_base_dir(root)?;
    // best-effort: la dir base sta sotto un'area controllata; create_dir_all e'
    // idempotente e non tocca risorse fuori dal dominio del progetto.
    std::fs::create_dir_all(&base_dir)?;

    let wt_path = worktree_path_for(root, run_id)?;
    let branch = ephemeral_branch(run_id);

    let wt_path_str = wt_path.to_string_lossy();
    git(
        "worktree add",
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            wt_path_str.as_ref(),
            base_commit,
        ],
        root,
    )
    .await?;

    Ok(WorktreeHandle {
        path: wt_path,
        base_commit: base_commit.to_string(),
        project_root: root.to_path_buf(),
        run_id,
        branch,
    })
}

/// Applica i cambiamenti del worktree alla `root` in modo ATOMICO e
/// SERIALIZZABILE (regola H). Approccio:
///
/// 1. Nel worktree: `git add -A`. Se non ci sono cambiamenti staged rispetto al
///    `base_commit` -> [`ApplyOutcome::NoChanges`] (nessuna scrittura sulla root).
/// 2. Nel worktree: `git commit` del delta del sub-run (commit effimero).
/// 3. Nella root: `git merge --no-ff --no-commit <branch>`.
///    - pulito -> `git commit` -> [`ApplyOutcome::Applied`].
///    - conflitto -> `git merge --abort` (root riportata allo stato pre-apply,
///      rollback verificabile) -> [`ApplyOutcome::Conflict`] con i file in
///      conflitto (best-effort da `git diff --name-only --diff-filter=U`).
///
/// Mai `git apply` diretto sulla working tree condivisa: l'unico canale di
/// promozione e' un merge git nativo (preserva rename/binari) con abort atomico.
///
/// # Serializzazione (contratto del chiamante)
/// L'apply NON e' sicuro se eseguito in concorrenza su piu' worktree verso la
/// STESSA root: index e refs `.git` sono condivisi. Il chiamante DEVE serializzare
/// gli apply (es. un `Mutex` per-root), applicando un worktree alla volta.
pub async fn apply_worktree_atomic(root: &Path, handle: &WorktreeHandle) -> Result<ApplyOutcome> {
    // 1. Stage di tutti i cambiamenti nel worktree.
    git("add -A", &["add", "-A"], &handle.path).await?;

    // Nessun cambiamento staged rispetto a HEAD del worktree (== base_commit)?
    // `git diff --cached --quiet` esce 0 se non ci sono differenze staged, 1 se
    // ce ne sono (segnale strutturato via exit_code, regola M).
    let diff_cached = run_cmd(
        "git",
        &["diff", "--cached", "--quiet"],
        &handle.path,
        GIT_TIMEOUT_SECS,
    )
    .await
    .map_err(|source| WorktreeError::Exec {
        op: "diff --cached --quiet".to_string(),
        source,
    })?;
    if diff_cached.success() {
        return Ok(ApplyOutcome::NoChanges);
    }

    // 2. Commit effimero del delta nel worktree (visibile dalla root via object
    //    store condiviso). --no-verify: nessun hook del progetto sul commit interno.
    git(
        "commit",
        &[
            "-c",
            "user.name=nexus-subagent",
            "-c",
            "user.email=subagent@nexus.local",
            "commit",
            "--no-verify",
            "-m",
            &format!("nexus-sub {}", handle.run_id),
        ],
        &handle.path,
    )
    .await?;

    // 3. Merge --no-ff --no-commit del branch effimero sulla root.
    let merge = run_cmd(
        "git",
        &["merge", "--no-ff", "--no-commit", &handle.branch],
        root,
        GIT_TIMEOUT_SECS,
    )
    .await
    .map_err(|source| WorktreeError::Exec {
        op: "merge".to_string(),
        source,
    })?;

    if merge.success() {
        // Stato pulito: consolida il merge con un commit sulla root.
        git(
            "commit (merge)",
            &[
                "-c",
                "user.name=nexus-subagent",
                "-c",
                "user.email=subagent@nexus.local",
                "commit",
                "--no-verify",
                "--no-edit",
            ],
            root,
        )
        .await?;
        Ok(ApplyOutcome::Applied)
    } else {
        // Conflitto: raccogli i file in conflitto PRIMA dell'abort (best-effort).
        let files = conflicted_files(root).await;
        // Rollback atomico: root riportata esattamente allo stato pre-apply.
        git("merge --abort", &["merge", "--abort"], root).await?;
        Ok(ApplyOutcome::Conflict { files })
    }
}

/// Elenca i file in conflitto durante un merge in corso, best-effort. In caso di
/// fallimento del probe ritorna una lista vuota (l'esito Conflict resta valido).
async fn conflicted_files(root: &Path) -> Vec<String> {
    match run_cmd(
        "git",
        &["diff", "--name-only", "--diff-filter=U"],
        root,
        GIT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() => out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Distrugge il worktree effimero e il suo branch. Best-effort e IDEMPOTENTE:
/// va chiamato su OGNI ramo di uscita (successo, timeout, errore, conflitto) e
/// non fallisce se il worktree e' gia' stato rimosso a monte.
///
/// - `git -C <root> worktree remove --force <path>`
/// - `git -C <root> branch -D <branch>`
/// - `git -C <root> worktree prune`
///
/// Ritorna sempre `Ok(())`: gli errori dei singoli comandi (worktree gia'
/// rimosso, branch inesistente) sono attesi in cleanup idempotente e vengono
/// ignorati. Un fallimento reale di teardown non deve mai propagarsi come errore
/// bloccante del sub-run.
pub async fn remove_worktree(handle: &WorktreeHandle) -> Result<()> {
    let root = &handle.project_root;
    let wt_path = handle.path.to_string_lossy();

    // remove --force: tollera file non-committati / lock transitori.
    let _ = run_cmd(
        "git",
        &["worktree", "remove", "--force", wt_path.as_ref()],
        root,
        GIT_TIMEOUT_SECS,
    )
    .await;

    // branch -D: elimina il branch effimero (best-effort).
    let _ = run_cmd("git", &["branch", "-D", &handle.branch], root, GIT_TIMEOUT_SECS).await;

    // prune: ripulisce i metadati di worktree orfani.
    let _ = run_cmd("git", &["worktree", "prune"], root, GIT_TIMEOUT_SECS).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Esegue un comando git sincrono nei test (setup del repo temp). Fuori dal
    /// path di produzione: qui `expect` e' ammesso (regola: unwrap/expect solo
    /// nei test).
    fn git_sync(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git spawn");
        assert!(status.success(), "git {args:?} fallito in {cwd:?}");
    }

    /// Crea un repo git temporaneo con un commit iniziale e un file `README.md`.
    /// Ritorna il [`tempfile::TempDir`] (da tenere vivo) e il path della root.
    /// La root e' una sottodir del tempdir cosi' che `worktree_base_dir` (che usa
    /// il parent) resti confinata nel tempdir (isolata per test).
    fn temp_repo() -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");
        git_sync(&root, &["init", "-q"]);
        git_sync(&root, &["config", "user.name", "nexus-test"]);
        git_sync(&root, &["config", "user.email", "test@nexus.local"]);
        // Evita che un default branch nome-dipendente rompa i comandi.
        git_sync(&root, &["checkout", "-q", "-B", "main"]);
        std::fs::write(root.join("README.md"), "riga iniziale\n").expect("write README");
        git_sync(&root, &["add", "-A"]);
        git_sync(&root, &["commit", "-q", "-m", "commit iniziale"]);
        (td, root)
    }

    #[tokio::test]
    async fn probe_true_su_repo_git() {
        let (_td, root) = temp_repo();
        assert!(probe_isolatable(&root).await, "un repo git deve essere isolabile");
    }

    #[tokio::test]
    async fn probe_false_su_dir_non_git() {
        let td = tempfile::tempdir().expect("tempdir");
        let plain = td.path().join("plain");
        std::fs::create_dir_all(&plain).expect("mkdir plain");
        assert!(
            !probe_isolatable(&plain).await,
            "una dir non-git NON deve essere isolabile (fail-closed)"
        );
    }

    #[tokio::test]
    async fn head_commit_ritorna_sha() {
        let (_td, root) = temp_repo();
        let sha = head_commit(&root).await.expect("head_commit");
        assert_eq!(sha.len(), 40, "SHA-1 esadecimale a 40 char, ottenuto: {sha:?}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn worktree_base_sotto_area_controllata() {
        let (_td, root) = temp_repo();
        let base = worktree_base_dir(&root).expect("base dir");
        // La base e' sibling della root (parent/.nexus-worktrees), regola E.
        assert_eq!(base.file_name().unwrap(), WORKTREE_BASE_NAME);
        assert_eq!(base.parent().unwrap(), root.parent().unwrap());
    }

    #[tokio::test]
    async fn ciclo_completo_applied() {
        let (_td, root) = temp_repo();
        let run_id = Uuid::new_v4();
        let base = head_commit(&root).await.expect("head");

        let handle = create_ephemeral_worktree(&root, run_id, &base)
            .await
            .expect("create worktree");
        assert!(handle.path.exists(), "il worktree deve esistere sul filesystem");

        // Scrive un NUOVO file nel worktree (scope disgiunto dalla root).
        std::fs::write(handle.path.join("feature.txt"), "contenuto sub-run\n")
            .expect("write nel worktree");
        // Il file NON deve ancora esistere nella root reale.
        assert!(
            !root.join("feature.txt").exists(),
            "prima dell'apply il file vive solo nel worktree, mai nella root"
        );

        let outcome = apply_worktree_atomic(&root, &handle)
            .await
            .expect("apply");
        assert_eq!(outcome, ApplyOutcome::Applied);

        // Dopo l'apply il file e' promosso alla root.
        let promoted = root.join("feature.txt");
        assert!(promoted.exists(), "il file deve essere promosso alla root");
        // Normalizza gli EOL: git puo' convertire \n in \r\n al checkout su
        // Windows (core.autocrlf) — il test asserisce sul contenuto logico.
        assert_eq!(
            std::fs::read_to_string(&promoted).unwrap().replace('\r', ""),
            "contenuto sub-run\n"
        );

        remove_worktree(&handle).await.expect("remove");
        assert!(!handle.path.exists(), "il worktree deve essere rimosso");
    }

    #[tokio::test]
    async fn no_changes_se_worktree_invariato() {
        let (_td, root) = temp_repo();
        let run_id = Uuid::new_v4();
        let base = head_commit(&root).await.expect("head");
        let handle = create_ephemeral_worktree(&root, run_id, &base)
            .await
            .expect("create worktree");

        // Nessuna scrittura nel worktree -> NoChanges, root intatta.
        let outcome = apply_worktree_atomic(&root, &handle).await.expect("apply");
        assert_eq!(outcome, ApplyOutcome::NoChanges);

        remove_worktree(&handle).await.expect("remove");
    }

    #[tokio::test]
    async fn conflict_lascia_root_pulita() {
        let (_td, root) = temp_repo();
        let run_id = Uuid::new_v4();
        let base = head_commit(&root).await.expect("head");

        let handle = create_ephemeral_worktree(&root, run_id, &base)
            .await
            .expect("create worktree");

        // Modifica lo STESSO file (README.md) sia nel worktree sia nella root,
        // in modo divergente -> merge in conflitto.
        std::fs::write(handle.path.join("README.md"), "modifica del sub-run\n")
            .expect("write worktree README");
        std::fs::write(root.join("README.md"), "modifica della root\n")
            .expect("write root README");
        // Committa la modifica divergente nella root (cosi' il merge diverge).
        git_sync(&root, &["add", "-A"]);
        git_sync(&root, &["commit", "-q", "-m", "modifica concorrente root"]);
        let root_head_pre = head_commit(&root).await.expect("head root pre-apply");

        let outcome = apply_worktree_atomic(&root, &handle).await.expect("apply");
        match outcome {
            ApplyOutcome::Conflict { files } => {
                assert!(
                    files.iter().any(|f| f.contains("README.md")),
                    "il file in conflitto deve essere segnalato, ottenuto: {files:?}"
                );
            }
            other => panic!("atteso Conflict, ottenuto {other:?}"),
        }

        // Root INTATTA: nessun merge in corso, HEAD invariato, contenuto originale.
        let root_head_post = head_commit(&root).await.expect("head root post-apply");
        assert_eq!(
            root_head_pre, root_head_post,
            "dopo il rollback HEAD della root non deve cambiare"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("README.md"))
                .unwrap()
                .replace('\r', ""),
            "modifica della root\n",
            "il contenuto della root deve restare quello pre-apply (rollback)"
        );
        // Nessun MERGE_HEAD residuo (merge --abort ha ripulito).
        assert!(
            !root.join(".git").join("MERGE_HEAD").exists(),
            "merge --abort deve aver rimosso MERGE_HEAD"
        );

        remove_worktree(&handle).await.expect("remove");
    }

    #[tokio::test]
    async fn remove_worktree_idempotente() {
        let (_td, root) = temp_repo();
        let run_id = Uuid::new_v4();
        let base = head_commit(&root).await.expect("head");
        let handle = create_ephemeral_worktree(&root, run_id, &base)
            .await
            .expect("create worktree");

        remove_worktree(&handle).await.expect("primo remove");
        // Seconda chiamata su handle gia' rimosso: non deve fallire.
        remove_worktree(&handle).await.expect("remove idempotente");
    }
}
