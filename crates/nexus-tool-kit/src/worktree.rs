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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

/// Timeout (secondi) per le singole invocazioni git di questo modulo. Le
/// operazioni sono locali (rev-parse, worktree add/remove, merge): un timeout
/// generoso copre repo grandi su Windows senza mai appendere all'infinito.
const GIT_TIMEOUT_SECS: u64 = 120;

/// Nome della directory base (sibling della project_root) sotto cui vivono tutti
/// i worktree effimeri Nexus. Sta un livello sopra la project_root perche' git
/// non consente di annidare un worktree dentro il working tree del repo stesso.
const WORKTREE_BASE_NAME: &str = ".nexus-worktrees";

/// Registry in-process dei lock per-root (punto unico di serializzazione, regola L).
/// Mappa `project_root` CANONICALIZZATA -> `Arc<tokio::sync::Mutex<()>>`. Il `Mutex`
/// std esterno protegge SOLO la HashMap (lock brevissimo: get/insert dell'`Arc`); la
/// mutua esclusione vera e propria e' sull'`Arc<tokio::sync::Mutex<()>>` (async-aware,
/// tenibile attraverso i `.await`). Il registry non viene mai svuotato: il numero di
/// root distinte in un processo e' piccolo e limitato ai progetti attivi.
static ROOT_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn root_locks() -> &'static Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> {
    ROOT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Chiave di lock per una root: la forma CANONICALIZZATA del path, cosi' che due
/// path equivalenti (relativi/assoluti, symlink, `.`/`..`) mappino allo STESSO lock.
/// Fallback al path pulito (`to_path_buf`) se `canonicalize` fallisce (root non ancora
/// esistente, permessi): resta deterministico per uno stesso input.
fn lock_key(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Acquisisce il lock per-root CROSS-batch (punto unico, regola L + H). Soddisfa il
/// contratto di serializzazione di [`apply_worktree_atomic`] a livello CROSS-batch:
/// un solo chiamante alla volta puo' toccare l'area `.git`/worktree condivisa di una
/// data `root`. Due batch isolati concorrenti sullo STESSO progetto (session distinte,
/// stessa `project_root`) vengono serializzati qui: il secondo attende che il primo
/// rilasci il guard.
///
/// Il guard va tenuto per TUTTA la sezione che tocca l'area condivisa (GC orfani ->
/// creazione worktree -> esecuzione ondate -> apply serializzato -> cleanup). Il
/// rilascio e' naturale a fine scope (Drop dell'[`tokio::sync::OwnedMutexGuard`]).
///
/// Restituisce un [`tokio::sync::OwnedMutexGuard`] (`lock_owned`, non `lock`): il
/// guard `owned` e' `'static` e `Send` (T = `()`), quindi puo' essere tenuto attraverso
/// i `.await` delle ondate parallele (`join_all`) senza vincoli di lifetime prestato.
///
/// Nessun rischio di deadlock/re-entrancy: i sub-run isolati girano in una
/// `working_root` DIVERSA (il worktree effimero), mai sulla stessa `root` di questo
/// lock, quindi non c'e' annidamento dello stesso lock nello stesso task.
pub async fn lock_project_root(root: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let key = lock_key(root);
    let arc = {
        // Lock std brevissimo: solo per leggere/creare l'Arc nella HashMap. In caso
        // di poison (panic di un altro thread mentre teneva questo lock std) si
        // recupera comunque la HashMap: il dato protetto e' un registry di Arc, non
        // uno stato che il panic possa aver lasciato incoerente.
        let mut map = root_locks().lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            map.entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    arc.lock_owned().await
}

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
/// gli apply, applicando un worktree alla volta. Il punto unico di serializzazione
/// per-root, valido anche CROSS-batch (batch concorrenti sullo stesso progetto), e'
/// [`lock_project_root`]: acquisirlo prima di toccare l'area `.git`/worktree e
/// tenerlo per l'intera sezione (GC + creazione + apply + cleanup).
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

/// Elenca i file che il branch effimero del sub-run ha modificato rispetto al
/// `base_commit`, in path RELATIVI alla root del progetto. Serve al chiamante per
/// il reindex-once post-apply (i sub-run isolati hanno il reindex per-scrittura
/// SOPPRESSO in PR3: l'indice del progetto si aggiorna UNA volta qui, solo sui
/// file realmente promossi).
///
/// Segnale strutturato dal `diff --name-only` (regola M): niente parsing di prosa,
/// solo la lista file emessa da git. Va chiamata DOPO un [`ApplyOutcome::Applied`]
/// (il branch effimero contiene gia' il commit del delta) e PRIMA del cleanup.
/// Best-effort: su fallimento del diff ritorna lista vuota (il reindex-once sara'
/// no-op, mai un errore bloccante del sub-run).
pub async fn promoted_files(handle: &WorktreeHandle) -> Vec<String> {
    match run_cmd(
        "git",
        &[
            "diff",
            "--name-only",
            &handle.base_commit,
            &handle.branch,
        ],
        &handle.project_root,
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

/// GC best-effort dei worktree effimeri ORFANI di un progetto (regola H: mai
/// accumulo su crash; regola E: filtrato per la root del progetto, mai un glob su
/// tutta l'area worktree condivisa fra progetti).
///
/// Scandisce `<project_root>/../.nexus-worktrees/` e rimuove ogni sottodir il cui
/// nome (un `run_id`) NON e' in `active_run_ids`: sono worktree di sub-run non piu'
/// attivi (terminati regolarmente ma non ripuliti per crash, oppure lasciati da un
/// `remove` fallito a runtime). Per ognuno esegue lo stesso teardown idempotente di
/// [`remove_worktree`] (worktree remove --force -> branch -D -> prune) e, se la dir
/// resiste (metadati git gia' assenti), la rimuove dal filesystem.
///
/// `git worktree prune` incondizionato ripulisce comunque i metadati di worktree la
/// cui dir e' gia' sparita. Ritorna il numero di worktree orfani effettivamente
/// bonificati (per audit/log). Nessun errore propagato: e' manutenzione.
pub async fn gc_orphan_worktrees(project_root: &Path, active_run_ids: &[Uuid]) -> usize {
    // prune incondizionato: ripulisce i metadati di worktree la cui dir e' gia'
    // stata rimossa (crash a meta' cleanup).
    let _ = run_cmd("git", &["worktree", "prune"], project_root, GIT_TIMEOUT_SECS).await;

    let base_dir = match worktree_base_dir(project_root) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let read_dir = match std::fs::read_dir(&base_dir) {
        Ok(rd) => rd,
        // Area worktree assente: niente da bonificare (caso comune).
        Err(_) => return 0,
    };

    let mut removed = 0usize;
    for entry in read_dir.flatten() {
        let dir_name = entry.file_name();
        let name = dir_name.to_string_lossy();
        // Il nome della dir e' il run_id del sub-run proprietario. Se non e' un
        // UUID (dir estranea), la lasciamo stare: regola E, non tocchiamo risorse
        // che non abbiamo creato noi.
        let Ok(run_id) = Uuid::parse_str(name.trim()) else {
            continue;
        };
        if active_run_ids.contains(&run_id) {
            // Sub-run ancora attivo: il suo worktree e' legittimo, non toccare.
            continue;
        }
        let wt_path = entry.path();
        // Teardown idempotente come remove_worktree, ricostruendo il branch dal
        // run_id (stesso schema di ephemeral_branch).
        let handle = WorktreeHandle {
            path: wt_path.clone(),
            base_commit: String::new(),
            project_root: project_root.to_path_buf(),
            run_id,
            branch: ephemeral_branch(run_id),
        };
        let _ = remove_worktree(&handle).await;
        // Se la dir resiste (worktree gia' scollegato dai metadati git), rimuovila
        // dal filesystem: best-effort, sotto l'area controllata del progetto.
        if wt_path.exists() {
            let _ = std::fs::remove_dir_all(&wt_path);
        }
        removed += 1;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Esegue un comando git sincrono nei test (setup del repo temp). Fuori dal
    /// path di produzione: qui `expect` e' ammesso (regola: unwrap/expect solo
    /// nei test).
    // `git_sync` e `temp_repo` erano duplicate identiche in
    // `mcp-core::session_autocommit`: la definizione vive dal 2026-08-05 in
    // `nexus-test-preconditions`, insieme a `seed_project_meta`, che vi era
    // sceso per lo stesso motivo — i crate estratti da mcp-core stanno sotto di
    // lui e i loro test hanno bisogno degli stessi helper.
    use nexus_test_preconditions::{git_sync, temp_repo};


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

    #[tokio::test]
    async fn promoted_files_elenca_delta_del_subrun() {
        let (_td, root) = temp_repo();
        let run_id = Uuid::new_v4();
        let base = head_commit(&root).await.expect("head");
        let handle = create_ephemeral_worktree(&root, run_id, &base)
            .await
            .expect("create worktree");

        // Due file nuovi nel worktree (scope disgiunto dalla root).
        std::fs::create_dir_all(handle.path.join("src")).expect("mkdir src");
        std::fs::write(handle.path.join("src/a.rs"), "// a\n").expect("write a");
        std::fs::write(handle.path.join("src/b.rs"), "// b\n").expect("write b");

        // Applica: commit effimero nel branch + merge sulla root.
        let outcome = apply_worktree_atomic(&root, &handle).await.expect("apply");
        assert_eq!(outcome, ApplyOutcome::Applied);

        let mut files = promoted_files(&handle).await;
        files.sort();
        assert_eq!(
            files,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            "i file promossi devono essere i soli modificati dal sub-run, relativi alla root"
        );

        remove_worktree(&handle).await.expect("remove");
    }

    #[tokio::test]
    async fn lock_stessa_root_canonicalizzata_stesso_arc() {
        // Due path che canonicalizzano alla STESSA root reale devono mappare allo
        // STESSO Arc (stesso lock): la mutua esclusione e' effettiva anche con path
        // scritti in forme equivalenti.
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("proj");
        std::fs::create_dir_all(&root).expect("mkdir proj");

        // Forma equivalente della stessa root: <root>/. (canonicalize la normalizza).
        let root_dot = root.join(".");

        // Recupera gli Arc dal registry (non teniamo i guard: qui misuriamo l'identita'
        // dell'Arc nel registry, non il blocco).
        let arc_a = {
            let key = lock_key(&root);
            let mut map = root_locks().lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                map.entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let arc_b = {
            let key = lock_key(&root_dot);
            let mut map = root_locks().lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                map.entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        assert!(
            Arc::ptr_eq(&arc_a, &arc_b),
            "path equivalenti (canonicalizzati) devono condividere lo stesso lock"
        );

        // Una root DIVERSA deve avere un Arc diverso.
        let other = td.path().join("altro");
        std::fs::create_dir_all(&other).expect("mkdir altro");
        let arc_other = {
            let key = lock_key(&other);
            let mut map = root_locks().lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                map.entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        assert!(
            !Arc::ptr_eq(&arc_a, &arc_other),
            "root diverse devono avere lock distinti"
        );
    }

    #[tokio::test]
    async fn lock_project_root_e_mutuamente_esclusivo() {
        // Acquisito il lock su una root, un secondo tentativo sulla STESSA root deve
        // restare bloccato finche' il primo guard non e' droppato.
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("proj");
        std::fs::create_dir_all(&root).expect("mkdir proj");

        let guard = lock_project_root(&root).await;

        // Secondo tentativo entro un timeout breve: deve andare in timeout (bloccato).
        let bloccato = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            lock_project_root(&root),
        )
        .await;
        assert!(
            bloccato.is_err(),
            "il secondo lock sulla stessa root deve restare bloccato finche' il primo e' vivo"
        );

        // Rilascia il primo guard: ora il secondo tentativo deve riuscire subito.
        drop(guard);
        let riuscito = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            lock_project_root(&root),
        )
        .await;
        assert!(
            riuscito.is_ok(),
            "dopo il drop del primo guard il lock deve essere riacquisibile"
        );
    }

    #[tokio::test]
    async fn gc_rimuove_orfani_ma_preserva_attivi() {
        let (_td, root) = temp_repo();
        let base = head_commit(&root).await.expect("head");
        let attivo = Uuid::new_v4();
        let orfano = Uuid::new_v4();

        let h_attivo = create_ephemeral_worktree(&root, attivo, &base)
            .await
            .expect("create attivo");
        let h_orfano = create_ephemeral_worktree(&root, orfano, &base)
            .await
            .expect("create orfano");
        assert!(h_attivo.path.exists());
        assert!(h_orfano.path.exists());

        // GC con solo `attivo` nella lista dei run attivi: l'orfano va rimosso.
        let removed = gc_orphan_worktrees(&root, &[attivo]).await;
        assert_eq!(removed, 1, "solo l'orfano deve essere bonificato");
        assert!(h_attivo.path.exists(), "il worktree attivo NON deve essere toccato");
        assert!(!h_orfano.path.exists(), "il worktree orfano deve essere rimosso");

        remove_worktree(&h_attivo).await.expect("cleanup attivo");
    }
}
