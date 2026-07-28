//! Project sandbox — isolamento Docker per ogni processo agente.
//!
//! Ogni progetto gira in un container Docker effimero (`nexus-sandbox:latest`).
//! Garantisce:
//! - **Filesystem**: visibile solo la directory del progetto (montata in /workspace).
//! - **Credenziali**: nessuna variabile di sistema Nexus (DATABASE_URL, REDIS_URL, ecc.)
//!   viene ereditata dai container.
//! - **Rete**: isolamento Docker standard — il container non vede localhost del host
//!   (quindi non può raggiungere PostgreSQL:5432, Redis:6379, gRPC:50051 del server).
//! - **Risorse**: limiti memory/cpu applicati via Docker cgroups.
//! - **Processo**: ogni container ha il proprio PID, IPC, UTS namespace.

use sqlx::PgPool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Nome dell'immagine Docker usata come base per la sandbox.
pub const SANDBOX_IMAGE: &str = "nexus-sandbox:latest";

// ─── Configurazione sandbox per-progetto ─────────────────────────────────────

/// Configurazione sandbox override per un singolo progetto.
/// Letta dalla colonna `sandbox_config` JSONB in `projects`.
/// Campi assenti = usa default globale da `settings`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProjectSandboxConfig {
    /// Limite memoria in MB. None = usa DEFAULT_MEMORY_MB.
    pub memory_mb: Option<u64>,
    /// Limite CPU core. None = usa DEFAULT_CPUS.
    pub cpus: Option<f64>,
    /// Modalità rete Docker: "none" | "bridge" | "host".
    /// None = "none" (isolamento totale — default sicuro).
    pub network_mode: Option<String>,
    /// Variabili d'ambiente extra iniettate in ogni processo del progetto.
    pub extra_env: Option<HashMap<String, String>>,
}

/// Carica la configurazione sandbox per-progetto dal DB.
/// Restituisce `Default` se il progetto non ha override.
pub async fn load_project_sandbox_config(
    db: &PgPool,
    project_id: uuid::Uuid,
) -> ProjectSandboxConfig {
    let row = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT sandbox_config FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();

    match row {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => ProjectSandboxConfig::default(),
    }
}

/// Salva la configurazione sandbox per-progetto nel DB.
pub async fn save_project_sandbox_config(
    db: &PgPool,
    project_id: uuid::Uuid,
    config: &ProjectSandboxConfig,
) -> Result<(), String> {
    let json = serde_json::to_value(config)
        .map_err(|e| format!("serializzazione sandbox config fallita: {e}"))?;
    sqlx::query("UPDATE projects SET sandbox_config = $1 WHERE id = $2")
        .bind(json)
        .bind(project_id)
        .execute(db)
        .await
        .map_err(|e| format!("aggiornamento sandbox config fallito: {e}"))?;
    Ok(())
}

// ─── Disponibilità sandbox ────────────────────────────────────────────────────

/// Flag globale: `true` se la sandbox Docker è stata inizializzata con successo all'avvio.
/// Impostato da `main.rs` tramite `set_sandbox_available()`. Consultabile da qualsiasi
/// call site senza dover propagare il parametro per tutta la catena di chiamata.
static SANDBOX_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Imposta il flag globale di disponibilità sandbox (chiamato una sola volta da `main.rs`).
pub fn set_sandbox_available(available: bool) {
    let _ = SANDBOX_AVAILABLE.set(available);
}

/// Restituisce `true` se la sandbox Docker è disponibile (flag globale).
/// Usato dai call site che non hanno accesso all'`AppState`.
pub fn sandbox_enabled() -> bool {
    *SANDBOX_AVAILABLE.get().unwrap_or(&false)
}

/// Prefisso dei container Docker creati dalla sandbox.
const CONTAINER_PREFIX: &str = "nx-sb-";

/// Memoria di default per ogni container sandbox (1 GB).
const DEFAULT_MEMORY_MB: u64 = 1024;

/// CPU di default per ogni container sandbox (2 core).
const DEFAULT_CPUS: f64 = 2.0;

/// Variabili d'ambiente del processo Nexus che NON devono mai
/// essere propagate ai container dei progetti.
const BLOCKED_ENV: &[&str] = &[
    "DATABASE_URL",
    "REDIS_URL",
    "NEURAL_CORE_URL",
    "NEXUS_ROOT",
    "NEXUS_TERMINAL_ROOT",
    "NEXUS_EXTRA_ROOTS",
    "JWT_SECRET",
    "TERMINAL_SESSION_SECRET",
    "SERVICE_TOKEN",
    "INTERNAL_SERVICE_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "GOOGLE_API_KEY",
    "LANGFUSE_SECRET_KEY",
    "LANGFUSE_HOST",
    "POSTGRES_PASSWORD",
    "POSTGRES_USER",
    "PGPASSWORD",
];

// ─── SandboxConfig ────────────────────────────────────────────────────────────

/// Configurazione sandbox per un singolo processo agente.
pub struct SandboxConfig {
    /// Root del filesystem del progetto, montata in lettura/scrittura in /workspace.
    pub project_root: PathBuf,
    /// ID processo per derivare il nome univoco del container.
    pub process_id: Uuid,
    /// Limite memoria in MB (default: 1024).
    pub memory_mb: u64,
    /// Limite CPU in core (default: 2.0).
    pub cpus: f64,
    /// Se `Some`, usa questa immagine Docker invece di `nexus-sandbox:latest`.
    pub project_image: Option<String>,
    /// Modalità rete Docker. None = Docker default (bridge).
    /// Imposta "none" per isolamento totale, "host" per condividere la rete host.
    pub network_mode: Option<String>,
    /// Variabili d'ambiente extra da iniettare nel container.
    pub extra_env: HashMap<String, String>,
}

impl SandboxConfig {
    /// Default sicuro: `network_mode = "none"` → isolamento totale di rete.
    /// Per i processi `kind = "service"` che devono accettare connessioni esterne,
    /// chiamare `with_service_network()` PRIMA di passare al builder Docker.
    ///
    /// Rollback temporaneo: bandiera ENV `NEXUS_SANDBOX_LEGACY_NETWORK=1` ripristina
    /// il vecchio default (Docker bridge). Rimuovere dopo 1 settimana di rollout.
    pub fn new(project_root: PathBuf, process_id: Uuid) -> Self {
        let default_network = if std::env::var("NEXUS_SANDBOX_LEGACY_NETWORK")
            .ok()
            .as_deref()
            == Some("1")
        {
            None
        } else {
            Some("none".to_string())
        };
        Self {
            project_root,
            process_id,
            memory_mb: DEFAULT_MEMORY_MB,
            cpus: DEFAULT_CPUS,
            project_image: None,
            network_mode: default_network,
            extra_env: HashMap::new(),
        }
    }

    pub fn with_image(mut self, image: String) -> Self {
        self.project_image = Some(image);
        self
    }

    /// Applica gli override dalla configurazione per-progetto.
    pub fn with_project_config(mut self, cfg: &ProjectSandboxConfig) -> Self {
        if let Some(mb) = cfg.memory_mb {
            self.memory_mb = mb;
        }
        if let Some(c) = cfg.cpus {
            self.cpus = c;
        }
        if cfg.network_mode.is_some() {
            self.network_mode = cfg.network_mode.clone();
        }
        if let Some(env) = &cfg.extra_env {
            self.extra_env.extend(env.clone());
        }
        self
    }

    /// Abilita la rete bridge per i processi `kind = "service"` che espongono
    /// porte. La porta dichiarata in `env_vars["PORT"]` viene gia' published
    /// dal builder Docker (cfr. riga ~395). Senza questa chiamata, il servizio
    /// gira con `--network=none` e non riceve connessioni.
    pub fn with_service_network(mut self) -> Self {
        self.network_mode = Some("bridge".to_string());
        self
    }

    /// Nome univoco del container Docker per questo processo.
    /// Formato: `nx-sb-<prime 8 cifre dell'UUID>`.
    pub fn container_name(&self) -> String {
        format!("{}{}", CONTAINER_PREFIX, &self.process_id.to_string()[..8])
    }
}

// ─── Host tool mounts ─────────────────────────────────────────────────────────

/// Un path dell'host montato nel container in sola lettura.
#[derive(Clone, Debug)]
struct HostMount {
    host: PathBuf,
    container: PathBuf,
}

impl HostMount {
    /// Monta lo stesso path dell'host anche dentro il container.
    fn same(path: impl Into<PathBuf>) -> Self {
        let p = path.into();
        HostMount {
            host: p.clone(),
            container: p,
        }
    }
    /// Monta `host` come `container` (path diversi).
    fn remap(host: impl Into<PathBuf>, container: impl Into<PathBuf>) -> Self {
        HostMount {
            host: host.into(),
            container: container.into(),
        }
    }
}

/// Lista di tool mounts dell'host, calcolata una sola volta all'avvio.
static HOST_MOUNTS: OnceLock<Vec<HostMount>> = OnceLock::new();

/// Monta ogni path esistente tra i `candidates`, con lo stesso path anche nel
/// container. I path assenti sull'host vengono semplicemente ignorati.
fn push_all_existing(mounts: &mut Vec<HostMount>, candidates: &[&str]) {
    for candidate in candidates {
        let p = PathBuf::from(*candidate);
        if p.exists() {
            mounts.push(HostMount::same(p));
        }
    }
}

/// Monta il PRIMO path esistente tra i `candidates` e ignora i restanti.
/// Serve per i tool con installazioni alternative (es. `node` vs `nodejs`), di
/// cui va montata una sola variante.
fn push_first_existing(mounts: &mut Vec<HostMount>, candidates: &[&str]) {
    for candidate in candidates {
        let p = PathBuf::from(*candidate);
        if p.exists() {
            mounts.push(HostMount::same(p));
            return;
        }
    }
}

/// Monta `host` (solo se esiste) sul path `container`, diverso da quello
/// dell'host.
fn push_remap_if_exists(mounts: &mut Vec<HostMount>, host: PathBuf, container: &str) {
    if host.exists() {
        mounts.push(HostMount::remap(host, container));
    }
}

/// Risolve la home di una toolchain: la variabile d'ambiente `env_key` se
/// valorizzata, altrimenti `$HOME/<fallback_subdir>` con `/root` come ultima
/// spiaggia. Punto unico della fallback, condivisa da CARGO_HOME e RUSTUP_HOME.
fn toolchain_home(env_key: &str, fallback_subdir: &str) -> PathBuf {
    std::env::var(env_key).map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
            .join(fallback_subdir)
    })
}

/// Node.js: interprete, package manager e moduli globali.
fn push_node_mounts(mounts: &mut Vec<HostMount>) {
    push_first_existing(mounts, &["/usr/bin/node", "/usr/bin/nodejs"]);
    push_first_existing(mounts, &["/usr/bin/npm", "/usr/local/bin/npm"]);
    push_first_existing(mounts, &["/usr/bin/npx", "/usr/local/bin/npx"]);
    push_first_existing(mounts, &["/usr/bin/pnpm", "/usr/local/bin/pnpm"]);
    // pnpm può essere uno script CJS
    push_all_existing(
        mounts,
        &[
            "/usr/bin/pnpm.cjs",
            "/usr/local/bin/pnpm.cjs",
            "/usr/local/lib/node_modules/pnpm/bin/pnpm.cjs",
        ],
    );
    // Moduli globali node
    push_all_existing(
        mounts,
        &["/usr/local/lib/node_modules", "/usr/lib/node_modules"],
    );
}

/// Cargo / Rust: binari nel PATH del container, registry e toolchain rustup.
fn push_rust_mounts(mounts: &mut Vec<HostMount>) {
    let cargo_home = toolchain_home("CARGO_HOME", ".cargo");
    // bin montato in /usr/local/cargo/bin (aggiunto al PATH container)
    push_remap_if_exists(mounts, cargo_home.join("bin"), "/usr/local/cargo/bin");
    push_remap_if_exists(
        mounts,
        cargo_home.join("registry"),
        "/usr/local/cargo/registry",
    );
    push_remap_if_exists(
        mounts,
        toolchain_home("RUSTUP_HOME", ".rustup"),
        "/usr/local/rustup",
    );
}

/// Python: interprete e libreria standard / dist-packages.
fn push_python_mounts(mounts: &mut Vec<HostMount>) {
    push_first_existing(
        mounts,
        &["/usr/bin/python3", "/usr/bin/python3.12", "/usr/bin/python"],
    );
    push_all_existing(
        mounts,
        &[
            "/usr/lib/python3",
            "/usr/lib/python3.12",
            "/usr/lib/python3/dist-packages",
        ],
    );
}

/// Git (con i suoi helper in git-core) e shell aggiuntive.
fn push_git_and_shell_mounts(mounts: &mut Vec<HostMount>) {
    push_first_existing(mounts, &["/usr/bin/git", "/usr/local/bin/git"]);
    push_all_existing(mounts, &["/usr/lib/git-core"]);
    push_all_existing(mounts, &["/bin/bash", "/usr/bin/bash"]);
}

/// Restituisce i tool dell'host da montare (read-only) in ogni container sandbox.
fn host_mounts() -> &'static Vec<HostMount> {
    HOST_MOUNTS.get_or_init(|| {
        let mut m: Vec<HostMount> = Vec::new();

        // ── Librerie condivise (necessarie per qualsiasi binario dinamico) ────
        push_all_existing(
            &mut m,
            &[
                "/lib/x86_64-linux-gnu",
                "/lib64",
                "/usr/lib/x86_64-linux-gnu",
            ],
        );
        push_node_mounts(&mut m);
        push_rust_mounts(&mut m);
        push_python_mounts(&mut m);
        push_git_and_shell_mounts(&mut m);

        info!(mounts = m.len(), "sandbox: host tool mounts calcolati");
        m
    })
}

// ─── Docker command builder ───────────────────────────────────────────────────

/// Costruisce il `Command` Docker per eseguire `shell_cmd` all'interno del
/// container sandbox del progetto.
///
/// Il container:
/// - Monta in lettura/scrittura solo `config.project_root` → `/workspace`
/// - Monta in sola lettura i tool dell'host (node, cargo, python, git, …)
/// - Non eredita nessuna variabile di sistema del processo Nexus
/// - Rispetta i limiti di memoria e CPU di `config`
/// - Ha il proprio PID/IPC/UTS namespace (via Docker)
/// - Non può raggiungere localhost del server host (Docker bridge isolato)
pub fn build_sandboxed_command(
    shell_cmd: &str,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    config: &SandboxConfig,
) -> Command {
    let mut docker = Command::new("docker");
    docker.args(["run", "--rm", "--init"]);

    // CRITICO: usa lo stesso UID/GID dell'utente host che esegue mcp-core,
    // altrimenti i file scritti nel volume bind-mount risultano root-owned
    // sull'host (vedi bug "delete_project lascia dir orfana": cleanup
    // impossibile senza sudo dopo che dotnet/npm/pnpm hanno scritto cache).
    // Su Linux: getuid()/getgid() del processo corrente.
    // Su altre piattaforme: skip (Docker Desktop gestisce mapping).
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getuid/getgid sono syscall thread-safe senza requisiti di unwind safety.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        docker.arg(format!("--user={uid}:{gid}"));
    }

    // Nome container per poterlo stoppare in seguito
    docker.args(["--name", &config.container_name()]);

    // Resource limits
    docker.arg(format!("--memory={}m", config.memory_mb));
    docker.arg(format!("--cpus={}", config.cpus));

    // Security hardening
    docker.args(["--security-opt", "no-new-privileges:true"]);
    docker.args(["--cap-drop", "ALL"]);

    // Rete: aggiunge il flag solo se esplicitamente configurato (default Docker = bridge)
    if let Some(nm) = &config.network_mode {
        docker.arg(format!("--network={nm}"));
    }

    // Filesystem: project dir montata in /workspace (rw)
    let workspace = "/workspace";
    docker.arg(format!(
        "-v={}:{}:rw",
        config.project_root.display(),
        workspace
    ));

    let using_project_image = config.project_image.is_some();

    if !using_project_image {
        // Solo per nexus-sandbox: monta tool dell'host (node, cargo, python, git…)
        for mount in host_mounts() {
            if mount.host.exists() {
                docker.arg(format!(
                    "-v={}:{}:ro",
                    mount.host.display(),
                    mount.container.display()
                ));
            }
        }
    }

    // Working directory: rimappa cwd relativo alla project_root in /workspace/<rel>
    let container_cwd = cwd
        .strip_prefix(&config.project_root)
        .ok()
        .map(|rel| PathBuf::from(workspace).join(rel))
        .unwrap_or_else(|| PathBuf::from(workspace));
    docker.arg(format!("--workdir={}", container_cwd.display()));

    if !using_project_image {
        // nexus-sandbox: PATH con cargo/bin
        docker.arg("--env=PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
        docker.arg(format!("--env=HOME={workspace}"));
        docker.arg("--env=CARGO_HOME=/usr/local/cargo");
        docker.arg("--env=RUSTUP_HOME=/usr/local/rustup");
    } else {
        // Immagine progetto: espone la porta configurata sull'host
        if let Some(port) = env_vars.get("PORT") {
            docker.arg(format!("-p={}:{}", port, port));
        }
        docker.arg(format!("--env=HOME={workspace}"));
    }

    // Variabili extra per-progetto (da ProjectSandboxConfig)
    for (k, v) in &config.extra_env {
        if !is_blocked_env(k) {
            let safe_v = v.replace(['\n', '\r'], "");
            docker.arg(format!("--env={k}={safe_v}"));
        }
    }

    // Solo le variabili esplicite del processo (mai quelle di sistema Nexus)
    for (k, v) in env_vars {
        if !is_blocked_env(k) {
            let safe_v = v.replace(['\n', '\r'], "");
            docker.arg(format!("--env={k}={safe_v}"));
        }
    }

    // Immagine e comando
    let image = config.project_image.as_deref().unwrap_or(SANDBOX_IMAGE);
    docker.arg(image);
    docker.args(["/bin/sh", "-c", shell_cmd]);

    docker.stdout(std::process::Stdio::piped());
    docker.stderr(std::process::Stdio::piped());

    debug!(
        container = config.container_name(),
        cwd = ?container_cwd,
        "sandbox: comando Docker costruito"
    );

    docker
}

// ─── Project image helpers ────────────────────────────────────────────────────

/// Cerca il `Dockerfile` più vicino alla directory del servizio, risalendo fino
/// a `project_root`. Ordine di ricerca: `service_cwd` → `project_root`.
pub fn find_project_dockerfile(project_root: &Path, service_cwd: &Path) -> Option<PathBuf> {
    // Cerca prima nella cwd del servizio
    let candidates = [service_cwd, project_root];
    for dir in &candidates {
        let df = dir.join("Dockerfile");
        if df.exists() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Controlla se esiste un'immagine Docker già buildata per il progetto.
/// NON esegue il build — per buildare usare `build_project_service_image`.
///
/// Restituisce `Some(tag)` se l'immagine è già disponibile localmente,
/// `None` se mancante o se il progetto non ha un Dockerfile.
pub async fn check_project_service_image(
    project_id: uuid::Uuid,
    project_root: &Path,
    service_cwd: &Path,
) -> Option<String> {
    find_project_dockerfile(project_root, service_cwd)?;
    let tag = format!("nexus-project-{}:latest", &project_id.to_string()[..8]);

    let exists = Command::new("docker")
        .args(["image", "inspect", &tag, "--format", "{{.Id}}"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if exists {
        info!(tag, "sandbox: immagine progetto trovata");
        Some(tag)
    } else {
        None
    }
}

/// Builda l'immagine Docker del progetto dalla sua directory Dockerfile.
/// Operazione lenta — da chiamare solo su richiesta esplicita dell'utente o dell'agente.
///
/// Tag usato: `nexus-project-<prime-8-cifre-uuid>:latest`
pub async fn build_project_service_image(
    project_id: uuid::Uuid,
    project_root: &Path,
    service_cwd: &Path,
) -> Result<String, String> {
    let dockerfile_dir = find_project_dockerfile(project_root, service_cwd)
        .ok_or_else(|| "Nessun Dockerfile trovato nel progetto".to_string())?;
    let tag = format!("nexus-project-{}:latest", &project_id.to_string()[..8]);

    info!(tag, dockerfile_dir = ?dockerfile_dir, "sandbox: build immagine progetto");

    let output = Command::new("docker")
        .args(["build", "-t", &tag, dockerfile_dir.to_str().unwrap_or(".")])
        .output()
        .await
        .map_err(|e| format!("docker build non eseguito: {e}"))?;

    if output.status.success() {
        info!(tag, "sandbox: immagine progetto built con successo");
        Ok(tag)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("docker build fallita: {stderr}"))
    }
}

// ─── Lifecycle helpers ────────────────────────────────────────────────────────

/// Verifica se l'immagine `nexus-sandbox:latest` è disponibile localmente.
pub async fn is_sandbox_available() -> bool {
    let ok = Command::new("docker")
        .args(["image", "inspect", SANDBOX_IMAGE, "--format", "{{.Id}}"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        warn!(
            "sandbox: immagine {SANDBOX_IMAGE} non trovata — \
             i processi gireranno senza isolamento Docker"
        );
    }
    ok
}

/// Ferma il container Docker associato a un processo sandbox.
/// Va chiamato da `stop_process` PRIMA del kill del PID del docker CLI.
pub async fn stop_sandbox_container(process_id: Uuid) {
    let name = format!("{}{}", CONTAINER_PREFIX, &process_id.to_string()[..8]);
    let _ = Command::new("docker")
        .args(["stop", "-t", "5", &name])
        .output()
        .await;
    debug!(container = name, "sandbox: container stoppato");
}

// ─── Env helpers ─────────────────────────────────────────────────────────────

/// `true` se la variabile è nella blacklist Nexus e NON deve
/// essere propagata ai processi figli (né Docker né diretti).
pub fn is_blocked_env(key: &str) -> bool {
    BLOCKED_ENV.iter().any(|&b| key.eq_ignore_ascii_case(b))
}

/// Filtra un env arbitrario con la blacklist `BLOCKED_ENV` Nexus. Estratto da
/// `safe_env_for_direct_spawn` per testare la proprieta' di isolamento con un
/// env sintetico, senza mutare l'env del processo di test (regola F).
pub fn filtered_safe_env(
    host_env: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, String> {
    host_env
        .into_iter()
        .filter(|(k, _)| !is_blocked_env(k))
        .collect()
}

/// Restituisce le variabili d'ambiente dell'host filtrate dalla blacklist.
pub fn safe_env_for_direct_spawn() -> HashMap<String, String> {
    filtered_safe_env(std::env::vars())
}

/// Crea un `tokio::process::Command` gia' isolato. PUNTO UNICO (regola L) per
/// spawnare comandi di progetto (one-shot e servizi diretti, no Docker). Due
/// proprieta' di isolamento, entrambe qui e mai nei call site:
///
/// 1. **Process group dedicato** (`process_group(0)` su Unix): un `kill -<pgid>`
///    su quel comando (es. cleanup di un dev-server duplicato) non puo' MAI
///    risalire a mcp-core. Difesa universale anti-suicidio, complementare alle
///    safety-net di `kill_process_tree`: invece di sperare che il killer
///    riconosca mcp-core, rendiamo strutturalmente impossibile che il padre
///    finisca nel gruppo killato.
/// 2. **Env isolato** (`env_clear` + host env filtrato da `BLOCKED_ENV`): il
///    figlio NON eredita i segreti di mcp-core (DATABASE_URL del meta,
///    JWT_SECRET, API key provider...) — incidente Beaty-Book 2026-07-02, dove
///    i one-shot ereditavano l'intero env e qualunque `env | grep` li esponeva.
///    Blacklist e NON whitelist: le variabili host legittime (PATH, HOME,
///    SYSTEMROOT, TEMP, npm_config_*...) passano. Le injection esplicite del
///    chiamante (`.env(...)`, es. DATABASE_URL del DB progetto) si applicano
///    sopra l'env gia' pulito.
pub fn isolated_command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.env_clear();
    cmd.envs(safe_env_for_direct_spawn());
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW (0x08000000): il figlio non apre/condivide una console
        // (niente finestre cmd lampeggianti per i comandi agente). NB: da solo NON
        // impedisce l'eredita' del socket :4000 (bInheritHandles resta TRUE per gli
        // stdio) -> per quello c'e' make_socket_non_inheritable sul listener.
        // creation_flags e' inerente a tokio::process::Command su Windows: nessun
        // import di std::os::windows::process::CommandExt necessario.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Windows: marca un socket listener come NON ereditabile dai processi figli.
/// PUNTO UNICO (regola L) contro il difetto per cui i figli spawnati da mcp-core
/// (dev server avviati dall'agente) ereditano l'handle del socket di ascolto (es.
/// :4000): un figlio orfano lo tiene dopo un crash/restart -> il nuovo mcp-core
/// fallisce il bind con WSAEADDRINUSE (os error 10048) -> crash loop WinSW.
/// `SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0)` azzera il flag di
/// ereditarieta' a prescindere da bInheritHandles del CreateProcess. Da chiamare
/// UNA volta subito dopo il bind, su `std::net::TcpListener`. Dichiarazione
/// kernel32 inline per non aggiungere la dipendenza windows-sys.
#[cfg(windows)]
pub fn make_socket_non_inheritable(listener: &std::net::TcpListener) {
    use std::os::windows::io::AsRawSocket;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    extern "system" {
        // HANDLE e' pointer-sized: usize e' ABI-compatibile e evita il cast a *mut.
        fn SetHandleInformation(h_object: usize, dw_mask: u32, dw_flags: u32) -> i32;
    }
    let handle = listener.as_raw_socket() as usize;
    // Best-effort: in caso di fallimento resta il comportamento attuale.
    unsafe {
        let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
    }
}

/// Shell con cui eseguire i comandi shell dell'agente (`run_command`, `run_tests`,
/// `execute_command`). PUNTO UNICO cross-platform (regola L). Gli agenti generano
/// comandi in sintassi bash (`mkdir -p a/{b,c}`, `pnpm install && pnpm build`,
/// pipe, `&&`), quindi su Windows usiamo **Git Bash** (non `cmd`/`powershell`, che
/// romperebbero quella sintassi). Senza questo, su Windows `/bin/bash` non esiste
/// e ogni comando agente fallisce con `os error 3` (path not found). Override
/// esplicito via env `NEXUS_SHELL`.
pub fn agent_shell() -> String {
    if let Ok(s) = std::env::var("NEXUS_SHELL") {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    shell_default()
}

#[cfg(unix)]
fn shell_default() -> String {
    // Bash (brace-expansion ecc.); fallback a sh.
    if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash".to_string()
    } else {
        "/bin/sh".to_string()
    }
}

#[cfg(windows)]
fn shell_default() -> String {
    // Git Bash nei path d'installazione standard; fallback a `bash` via PATH.
    for p in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    "bash".to_string()
}

// ─── Validation override env passate dall'agente ──────────────────────────────

/// Valida le `env_overrides` che l'agente vuole iniettare in un processo del
/// progetto. Difende il path critico spawn dai tentativi di puntare a risorse
/// Nexus (DB, Redis) o di bindare porte fuori dal bucket assegnato al progetto.
///
/// Regole hardcoded (in ordine di applicazione):
/// 1. Nessuna variabile nella `BLOCKED_ENV` di Nexus (`is_blocked_env`)
/// 2. `PORT` deve essere nel bucket del progetto OPPURE gia' allocata a questo
///    progetto in `nexus_port_allocations` (per run_config legacy)
/// 3. `DATABASE_URL` / `POSTGRES_URL` non puo' puntare al DB `nexus` o `postgres`
/// 4. `REDIS_URL` non puo' puntare al Redis Nexus (`:6379` su localhost o `ideai-redis`)
///
/// Ritorna `Err(messaggio)` al primo violation, `Ok(())` se tutto e' lecito.
pub async fn validate_env_overrides(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    for (k, v) in env {
        // 1. Blocked env (riusa is_blocked_env esistente)
        if is_blocked_env(k) {
            return Err(format!(
                "variabile '{k}' bloccata (BLOCKED_ENV Nexus). \
                 Non e' permesso ereditare credenziali di sistema verso processi del progetto."
            ));
        }
        // 2. PORT deve essere nel bucket del progetto
        if k.eq_ignore_ascii_case("PORT") {
            let port: u16 = v
                .parse()
                .map_err(|_| format!("PORT='{v}' non valido (atteso u16)"))?;
            validate_port_for_project(db, project_id, port).await?;
        }
        // 3. DATABASE_URL non deve puntare a DB Nexus
        if k.eq_ignore_ascii_case("DATABASE_URL") || k.eq_ignore_ascii_case("POSTGRES_URL") {
            let low = v.to_lowercase();
            // pattern: ...@<host>[:port]/nexus oppure /postgres oppure :5432
            let bad_db =
                low.contains("/nexus") || low.ends_with("/postgres") || low.contains("/postgres?");
            let bad_port = low.contains(":5432");
            if bad_db || bad_port {
                return Err(format!(
                    "{k} punta a infrastruttura Nexus (DB nexus o porta 5432). \
                     Usa il DB dedicato del progetto (project_database_config)."
                ));
            }
        }
        // 4. REDIS_URL non deve puntare a Redis Nexus
        if k.eq_ignore_ascii_case("REDIS_URL") {
            let low = v.to_lowercase();
            if low.contains(":6379") || low.contains("ideai-redis") {
                return Err("REDIS_URL punta al Redis Nexus (:6379 o ideai-redis). \
                     I progetti devono usare il proprio Redis allocato via wizard.".to_string());
            }
        }
    }
    Ok(())
}

/// Verifica che una porta sia nel bucket del progetto o gia' allocata ad esso.
/// Errore esplicativo se la porta e' riservata Nexus o appartiene a un altro progetto.
async fn validate_port_for_project(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    port: u16,
) -> Result<(), String> {
    use crate::ports::{port_in_project_bucket, project_bucket_range, NEXUS_RESERVED_PORTS};
    if NEXUS_RESERVED_PORTS.contains(&port) {
        return Err(format!(
            "porta {port} riservata Nexus (web-ide/microservizi/DB infrastruttura). \
             Usa request_port per allocarne una nel bucket del progetto."
        ));
    }
    let (bucket_start, bucket_end) = project_bucket_range(&project_id);
    if !port_in_project_bucket(&project_id, port) {
        // Tollerata SOLO se gia' allocata a questo progetto (caso run_config legacy)
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM nexus_port_allocations WHERE port = $1 AND project_id = $2)",
        )
        .bind(port as i32)
        .bind(project_id)
        .fetch_one(db)
        .await
        .unwrap_or(false);
        if !owned {
            return Err(format!(
                "porta {port} fuori dal bucket del progetto [{bucket_start}, {bucket_end}]. \
                 Chiama request_port per ottenere una porta valida."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests_validate_env {
    use super::*;

    #[test]
    fn blocked_env_rejected() {
        // is_blocked_env e' sync; verifichiamo la sua logica direttamente
        assert!(is_blocked_env("DATABASE_URL"));
        assert!(is_blocked_env("JWT_SECRET"));
        assert!(is_blocked_env("ANTHROPIC_API_KEY"));
        assert!(!is_blocked_env("PORT"));
        assert!(!is_blocked_env("HOST"));
    }
}

#[cfg(test)]
mod tests_isolated_env {
    use super::*;

    /// Env host come lo vede mcp-core in produzione: variabili di sistema
    /// necessarie ai figli + segreti Nexus che NON devono mai propagarsi.
    fn host_env_meta() -> Vec<(String, String)> {
        [
            ("DATABASE_URL", "postgresql://nexus:nexus@localhost:5433/nexus"),
            ("REDIS_URL", "redis://localhost:6379"),
            ("JWT_SECRET", "supersegreto"),
            ("ANTHROPIC_API_KEY", "sk-ant-xyz"),
            ("pgpassword", "case-insensitive-bypass"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/nexus"),
            ("SYSTEMROOT", r"C:\Windows"),
            ("TEMP", r"C:\Temp"),
            ("npm_config_registry", "https://registry.npmjs.org"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn filtered_safe_env_rimuove_i_segreti_e_tiene_le_var_di_sistema() {
        let env = filtered_safe_env(host_env_meta());
        // Blacklist fuori, anche con case diverso (pgpassword vs PGPASSWORD)
        for blocked in ["DATABASE_URL", "REDIS_URL", "JWT_SECRET", "ANTHROPIC_API_KEY", "pgpassword"]
        {
            assert!(!env.contains_key(blocked), "'{blocked}' non filtrata");
        }
        // Variabili host legittime dentro (blacklist, non whitelist)
        for kept in ["PATH", "HOME", "SYSTEMROOT", "TEMP", "npm_config_registry"] {
            assert!(env.contains_key(kept), "'{kept}' filtrata per errore");
        }
    }

    #[test]
    fn injection_esplicita_sopravvive_al_filtro() {
        // Il contratto dei call site (run_command, spawn_agent_process): l'env
        // pulito dal punto unico + .env("DATABASE_URL", <url progetto>) sopra.
        let mut cmd = isolated_command("echo");
        cmd.env("DATABASE_URL", "postgresql://app:app@localhost:5434/beaty_book_app");
        let injected = cmd
            .as_std()
            .get_envs()
            .find(|(k, _)| k.to_string_lossy() == "DATABASE_URL")
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()));
        assert_eq!(
            injected.as_deref(),
            Some("postgresql://app:app@localhost:5434/beaty_book_app")
        );
    }

    #[test]
    fn isolated_command_imposta_esattamente_l_env_filtrato() {
        // env_clear() non e' ispezionabile via API std: verifichiamo il wiring
        // osservabile — le variabili impostate esplicitamente sul Command sono
        // ESATTAMENTE l'host env filtrato, nessuna chiave bloccata inclusa.
        let cmd = isolated_command("echo");
        let expected = safe_env_for_direct_spawn();
        let got: HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        assert_eq!(got, expected);
        for k in got.keys() {
            assert!(!is_blocked_env(k), "variabile bloccata '{k}' propagata");
        }
    }
}
