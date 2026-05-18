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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use tracing::{debug, info, warn};
use sqlx::PgPool;
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
        "SELECT sandbox_config FROM projects WHERE id = $1"
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
    sqlx::query(
        "UPDATE projects SET sandbox_config = $1 WHERE id = $2"
    )
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
        let default_network = if std::env::var("NEXUS_SANDBOX_LEGACY_NETWORK").ok().as_deref() == Some("1") {
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
        if let Some(mb) = cfg.memory_mb { self.memory_mb = mb; }
        if let Some(c) = cfg.cpus { self.cpus = c; }
        if cfg.network_mode.is_some() { self.network_mode = cfg.network_mode.clone(); }
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
        HostMount { host: p.clone(), container: p }
    }
    /// Monta `host` come `container` (path diversi).
    fn remap(host: impl Into<PathBuf>, container: impl Into<PathBuf>) -> Self {
        HostMount { host: host.into(), container: container.into() }
    }
}

/// Lista di tool mounts dell'host, calcolata una sola volta all'avvio.
static HOST_MOUNTS: OnceLock<Vec<HostMount>> = OnceLock::new();

/// Restituisce i tool dell'host da montare (read-only) in ogni container sandbox.
fn host_mounts() -> &'static Vec<HostMount> {
    HOST_MOUNTS.get_or_init(|| {
        let mut m: Vec<HostMount> = Vec::new();

        // ── Librerie condivise (necessarie per qualsiasi binario dinamico) ────
        for lib in &[
            "/lib/x86_64-linux-gnu",
            "/lib64",
            "/usr/lib/x86_64-linux-gnu",
        ] {
            let p = PathBuf::from(lib);
            if p.exists() { m.push(HostMount::same(p)); }
        }

        // ── Node.js ───────────────────────────────────────────────────────────
        for bin in &["/usr/bin/node", "/usr/bin/nodejs"] {
            if PathBuf::from(bin).exists() { m.push(HostMount::same(*bin)); break; }
        }
        for bin in &["/usr/bin/npm", "/usr/local/bin/npm"] {
            if PathBuf::from(bin).exists() { m.push(HostMount::same(*bin)); break; }
        }
        for bin in &["/usr/bin/npx", "/usr/local/bin/npx"] {
            if PathBuf::from(bin).exists() { m.push(HostMount::same(*bin)); break; }
        }
        for bin in &["/usr/bin/pnpm", "/usr/local/bin/pnpm"] {
            if PathBuf::from(bin).exists() { m.push(HostMount::same(*bin)); break; }
        }
        // pnpm può essere uno script CJS
        for p in &[
            "/usr/bin/pnpm.cjs",
            "/usr/local/bin/pnpm.cjs",
            "/usr/local/lib/node_modules/pnpm/bin/pnpm.cjs",
        ] {
            if PathBuf::from(p).exists() { m.push(HostMount::same(*p)); }
        }
        // Moduli globali node
        for gm in &["/usr/local/lib/node_modules", "/usr/lib/node_modules"] {
            let p = PathBuf::from(gm);
            if p.exists() { m.push(HostMount::same(p)); }
        }

        // ── Cargo / Rust ──────────────────────────────────────────────────────
        let cargo_home = std::env::var("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".cargo")
            });
        let cargo_bin = cargo_home.join("bin");
        if cargo_bin.exists() {
            // bin montato in /usr/local/cargo/bin (aggiunto al PATH container)
            m.push(HostMount::remap(&cargo_bin, "/usr/local/cargo/bin"));
        }
        let cargo_registry = cargo_home.join("registry");
        if cargo_registry.exists() {
            m.push(HostMount::remap(&cargo_registry, "/usr/local/cargo/registry"));
        }
        let rustup_home = std::env::var("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".rustup")
            });
        if rustup_home.exists() {
            m.push(HostMount::remap(&rustup_home, "/usr/local/rustup"));
        }

        // ── Python ────────────────────────────────────────────────────────────
        for py in &["/usr/bin/python3", "/usr/bin/python3.12", "/usr/bin/python"] {
            if PathBuf::from(py).exists() { m.push(HostMount::same(*py)); break; }
        }
        for stdlib in &["/usr/lib/python3", "/usr/lib/python3.12", "/usr/lib/python3/dist-packages"] {
            let p = PathBuf::from(stdlib);
            if p.exists() { m.push(HostMount::same(p)); }
        }

        // ── Git ───────────────────────────────────────────────────────────────
        for git in &["/usr/bin/git", "/usr/local/bin/git"] {
            if PathBuf::from(git).exists() { m.push(HostMount::same(*git)); break; }
        }
        let git_core = PathBuf::from("/usr/lib/git-core");
        if git_core.exists() { m.push(HostMount::same(git_core)); }

        // ── Shell extras ──────────────────────────────────────────────────────
        for sh in &["/bin/bash", "/usr/bin/bash"] {
            if PathBuf::from(sh).exists() { m.push(HostMount::same(*sh)); }
        }

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
    docker.arg(format!("-v={}:{}:rw", config.project_root.display(), workspace));

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

/// Restituisce le variabili d'ambiente dell'host filtrate dalla blacklist.
/// Usato da `exec.rs` per i processi diretti (non Docker).
pub fn safe_env_for_direct_spawn() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| !is_blocked_env(k))
        .collect()
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
            let port: u16 = v.parse().map_err(|_| format!("PORT='{v}' non valido (atteso u16)"))?;
            validate_port_for_project(db, project_id, port).await?;
        }
        // 3. DATABASE_URL non deve puntare a DB Nexus
        if k.eq_ignore_ascii_case("DATABASE_URL") || k.eq_ignore_ascii_case("POSTGRES_URL") {
            let low = v.to_lowercase();
            // pattern: ...@<host>[:port]/nexus oppure /postgres oppure :5432
            let bad_db = low.contains("/nexus") || low.ends_with("/postgres") || low.contains("/postgres?");
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
                return Err(format!(
                    "REDIS_URL punta al Redis Nexus (:6379 o ideai-redis). \
                     I progetti devono usare il proprio Redis allocato via wizard."
                ));
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
    use crate::project_workspace::services::{
        project_bucket_start, NEXUS_RESERVED_PORTS, PROJECT_PORT_BUCKET_SIZE,
    };
    if NEXUS_RESERVED_PORTS.contains(&port) {
        return Err(format!(
            "porta {port} riservata Nexus (web-ide/microservizi/DB infrastruttura). \
             Usa request_port per allocarne una nel bucket del progetto."
        ));
    }
    let bucket_start = project_bucket_start(&project_id);
    let bucket_end = bucket_start + PROJECT_PORT_BUCKET_SIZE;
    let in_bucket = port >= bucket_start && port < bucket_end;
    if !in_bucket {
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
                "porta {port} fuori dal bucket del progetto [{bucket_start}, {bucket_end}). \
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

