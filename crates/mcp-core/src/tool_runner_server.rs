// Server gRPC ToolRunner: superficie minima esposta da mcp-core verso il
// brain (Python/LangGraph). Il brain orchestra il loop agent; qui
// eseguiamo solo tool singoli, stateless, correlati a una chat session.
//
// Contratto: vedi proto/tool_runner.proto.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use mcp_proto::tool_runner::tool_runner_server::{ToolRunner, ToolRunnerServer};
use mcp_proto::tool_runner::{ExecuteToolRequest, ExecuteToolResponse, ToolChunk};
use serde_json::Value;
use sqlx::PgPool;
use sqlx::Row;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;

use crate::agent_tools::{execute_agent_tool, AgentToolContext};
use crate::orchestrator::NeuralCoreClient;

/// Dipendenze condivise iniettate nel service. E' una slice sottile di
/// AppState: teniamo qui solo cio' che serve per costruire un
/// AgentToolContext e per eseguire un tool.
#[derive(Clone)]
pub struct ToolRunnerDeps {
    pub db: PgPool,
    pub neural: NeuralCoreClient,
    pub playwright_channels: crate::playwright_live::PlaywrightChannels,
    pub dependency_status: crate::task_watchdog::DependencyStatusRef,
    pub project_channels: nexus_events::ProjectChannels,
    pub monitor_registry: std::sync::Arc<
        parking_lot::RwLock<
            std::collections::HashMap<Uuid, std::collections::HashMap<String, serde_json::Value>>,
        >,
    >,
    pub port_registry: crate::port_registry::PortRegistryCache,
}

#[derive(Clone)]
pub struct ToolRunnerService {
    deps: ToolRunnerDeps,
}

/// Mappa l'errore del punto unico pool-progetto sullo `Status` gRPC del
/// contratto ToolRunner: `NotFound` solo quando l'assenza dell'entita' e'
/// dimostrata su tutti i DB-progetto, `Unavailable` per ogni indisponibilita'
/// (condizione transitoria: il chiamante ritenta). Niente fallback al meta:
/// a separazione attiva le tabelle chat sul meta sono vuote e la sessione
/// "sparirebbe" in silenzio (regola M).
fn project_db_status(e: crate::project_db_routes::ProjectDbError) -> Status {
    match &e {
        crate::project_db_routes::ProjectDbError::EntityNotFound { .. } => {
            Status::not_found(e.to_string())
        }
        _ => Status::unavailable(e.to_string()),
    }
}

impl ToolRunnerService {
    pub fn new(deps: ToolRunnerDeps) -> Self {
        Self { deps }
    }

    /// Risolve la sessione chat in (project_id, user_id, root_path,
    /// is_git_repo, can_write, user_role) con una singola query.
    /// Il brain e' trusted: non applichiamo qui il check di accesso
    /// (avviene gia' a monte negli handler chat di mcp-core al momento
    /// dell'invio del messaggio utente).
    async fn resolve_session(&self, session_id: Uuid) -> Result<SessionInfo, Status> {
        // Separazione DB (sempre attiva, mig 0527): `chat_sessions` vive nel DB
        // per-progetto, mentre `projects`/`project_members`/`workspaces`/
        // `repositories` restano nel meta. Il JOIN cross-DB non e' eseguibile su
        // un solo pool: lo spezziamo e ricomponiamo in Rust preservando la
        // semantica del JOIN originale.
        let project_pool =
            crate::project_db_routes::project_data_pool_by_session_from(&self.deps.db, session_id)
                .await
                .map_err(project_db_status)?;

        // (1) Parte migrata: sessione dal pool per-progetto (nessun JOIN).
        let session_row = sqlx::query(
            r#"
            SELECT s.project_id, s.user_id
            FROM chat_sessions s
            WHERE s.id = $1
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .fetch_optional(&project_pool)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("session non trovata"))?;

        let project_id: Uuid = session_row
            .try_get("project_id")
            .map_err(|e| Status::internal(format!("project_id: {e}")))?;
        let user_id: Option<Uuid> = session_row.try_get("user_id").unwrap_or(None);
        let user_id = user_id.ok_or_else(|| Status::failed_precondition("session senza user"))?;

        // (2) Parte globale: project/workspace/repository/membership dal meta,
        // keyed dal project_id risolto. Riproduce i LEFT JOIN del query originale.
        let meta_row = sqlx::query(
            r#"
            SELECT
                COALESCE(r.root_path, w.absolute_path) AS root_path,
                COALESCE(r.is_git_repo, FALSE)         AS is_git_repo,
                CASE
                    WHEN p.owner_user_id = $2 THEN 'owner'
                    ELSE COALESCE(pm.role, 'viewer')
                END AS role
            FROM projects p
            LEFT JOIN project_members pm
                ON pm.project_id = p.id AND pm.user_id = $2
            LEFT JOIN workspaces w
                ON w.project_id = p.id AND w.is_primary = TRUE
            LEFT JOIN repositories r
                ON r.project_id = p.id
            WHERE p.id = $1
            LIMIT 1
            "#,
        )
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(&self.deps.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("progetto non trovato"))?;

        // (3) Ricomposizione dei campi come faceva il JOIN.
        let root_path: String = meta_row
            .try_get("root_path")
            .map_err(|_| Status::failed_precondition("workspace non configurato"))?;
        let is_git_repo: bool = meta_row.try_get("is_git_repo").unwrap_or(false);
        let role: String = meta_row
            .try_get("role")
            .unwrap_or_else(|_| "viewer".to_string());
        let can_write = matches!(role.as_str(), "owner" | "admin" | "editor");

        Ok(SessionInfo {
            project_id,
            user_id,
            root_path: PathBuf::from(root_path),
            is_git_repo,
            can_write,
            user_role: role,
        })
    }

    /// Costruisce l'`AgentToolContext` per una sessione chat (root del progetto
    /// risolta dalla sessione). Delega al PUNTO UNICO
    /// [`ToolRunnerService::build_ctx_with_root`] con `override_root=None`.
    ///
    /// `pub(crate)`: riusato dall'adapter `ToolExecutor` del grafo Rust
    /// (`agent_graph_adapter::tool_executor`) per eseguire i tool IN-PROCESS sullo
    /// STESSO contesto (root_path/permessi/canali) del path gRPC — un solo punto di
    /// costruzione del ctx (regola L), nessuna divergenza di permessi o reindex.
    pub(crate) async fn build_ctx(&self, session_id: Uuid) -> Result<AgentToolContext, Status> {
        self.build_ctx_with_root(session_id, None).await
    }

    /// Costruisce il ctx tool per un pre-step legato a un run primario gia' persistito
    /// (es. Consiglio delle Competenze a monte di `spawn_agent_run`). Ancora depth/cost
    /// e catena sub-run al `run_id` del padre, NON alla sessione: ogni run primario
    /// parte con budget e profondita' isolati.
    pub(crate) async fn build_ctx_for_primary_run(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<AgentToolContext, Status> {
        let mut ctx = self.build_ctx(session_id).await?;
        ctx.core.run_id = Some(run_id);
        ctx.core.parent_run_id = Some(run_id);
        Ok(ctx)
    }

    /// PUNTO UNICO (regola L) della costruzione dell'`AgentToolContext`: risolve la
    /// sessione in project/root/permessi/canali. Se `override_root` e' `Some`,
    /// SOVRASCRIVE la sola `root_path` con quel valore (FASE 2 orchestrazione: un
    /// sub-run isolato scrive in un git worktree effimero, non nella project_root
    /// condivisa) e imposta `isolated_subrun=true` nel ctx — la leva che SOPPRIME
    /// autocommit di sessione e reindex per-scrittura (hook keyed su
    /// session/project condivisi, race con l'isolamento). Con `override_root=None`
    /// il ctx e' IDENTICO a quello di `build_ctx` (root della sessione,
    /// `isolated_subrun=false`) -> comportamento invariato: nessun call site di PR3
    /// passa un override (l'accensione e' PR4).
    ///
    /// Tutto il resto (project_id, permessi, pool, canali, reindexer) e' risolto
    /// UNA volta qui: `build_ctx` vi delega, nessuna logica duplicata.
    pub(crate) async fn build_ctx_with_root(
        &self,
        session_id: Uuid,
        override_root: Option<&Path>,
    ) -> Result<AgentToolContext, Status> {
        let info = self.resolve_session(session_id).await?;
        let long_running_patterns = crate::long_running::load_enabled_patterns(&self.deps.db).await;
        // Separazione DB: run_db = pool del progetto per i tool che toccano il
        // dominio run (plans/todos/worklog); `db` resta il meta per la config.
        let run_db =
            crate::project_db_routes::project_data_pool_by_session_from(&self.deps.db, session_id)
                .await
                .map_err(project_db_status)?;
        // Root effettiva del ctx + flag isolamento: decisi dal PUNTO UNICO puro
        // `resolve_ctx_root` (testabile senza DB) — override del sub-run isolato
        // quando presente, altrimenti la root del progetto (path invariato).
        let (root_path, isolated_subrun) = resolve_ctx_root(info.root_path, override_root);
        Ok(AgentToolContext {
            core: nexus_agent_tools::ToolContextCore {
                root_path,
                user_id: info.user_id,
                is_git_repo: info.is_git_repo,
                can_write: info.can_write,
                project_id: info.project_id,
                session_id: Some(session_id),
                db: Arc::new(self.deps.db.clone()),
                run_db: Arc::new(run_db),
                parent_run_id: None,
                // Il run CORRENTE non e' noto a questo punto (il ctx e' costruito
                // dalla sola sessione): lo valorizza il path Real del grafo
                // (`ToolRunnerExecutorAdapter::execute_real`) dalla narrazione del
                // run invocante. Fuori dal grafo resta `None`.
                run_id: None,
                long_running_patterns,
                user_role: info.user_role.clone(),
                is_nexus_operator: matches!(info.user_role.as_str(), "owner" | "admin"),
                project_channels: self.deps.project_channels.clone(),
                monitor_registry: self.deps.monitor_registry.clone(),
                hooks: Arc::new(crate::agent_tools::context::NeuralFileReindexer {
                    db: Arc::new(self.deps.db.clone()),
                    neural: self.deps.neural.clone(),
                }),
                embedder: Arc::new(self.deps.neural.clone()),
                isolated_subrun,
            },
            playwright_channels: self.deps.playwright_channels.clone(),
            neural: self.deps.neural.clone(),
            dependency_status: self.deps.dependency_status.clone(),
            port_registry: self.deps.port_registry.clone(),
            // Narrazione verso il run invocante: il ctx base non la conosce (il
            // contratto porta solo session_id). La valorizza il SOLO path Real
            // del grafo nativo (ToolRunnerExecutorAdapter::execute_real).
            parent_narration: None,
        })
    }
}

struct SessionInfo {
    project_id: Uuid,
    user_id: Uuid,
    root_path: PathBuf,
    is_git_repo: bool,
    can_write: bool,
    user_role: String,
}

/// PUNTO UNICO (regola L) della decisione root + isolamento del ctx tool.
///
/// - `override_root=Some(p)` (sub-run ISOLATO, FASE 2): la root del ctx e' `p`
///   (git worktree effimero) e `isolated_subrun=true` -> autocommit/reindex
///   soppressi (buco B2).
/// - `override_root=None` (default): la root e' `session_root` (la project_root
///   risolta dalla sessione) e `isolated_subrun=false` -> comportamento invariato.
///
/// Funzione pura (nessun I/O): la regola di override e' definita in un solo posto
/// e coperta da unit test senza DB.
fn resolve_ctx_root(session_root: PathBuf, override_root: Option<&Path>) -> (PathBuf, bool) {
    match override_root {
        Some(p) => (p.to_path_buf(), true),
        None => (session_root, false),
    }
}

/// Estrae l'exit code dal testo "EXIT CODE: N" emesso da run_command &c.
/// (formato CONTROLLATO da noi in agent_tools/command.rs — questo e' un parser
/// del nostro stesso output, non un'euristica sul testo del modello). Punto
/// unico Rust della traduzione testo->strutturato: prima ogni consumer Python
/// ri-parsava la stringa con regex (`EXIT CODE: N`); ora il valore viaggia
/// strutturato nel proto (contratto dati A, censimento 2026-06-10).
///
/// `pub(crate)`: riusato dall'adapter `ToolExecutor` del grafo Rust
/// (`agent_graph_adapter::tool_executor`) per estrarre lo stesso `exit_code`
/// strutturato sia in Real (dal risultato di `execute_agent_tool`) sia in Replay
/// (dal `tool_result` riletto da `agent_steps`). Un solo parser (regola L).
pub(crate) fn extract_exit_code(result: &str) -> Option<i32> {
    let marker = "EXIT CODE: ";
    let start = result.find(marker)? + marker.len();
    let rest = &result[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].trim().parse::<i32>().ok()
}

/// `true` se il risultato testuale di un tool e' un ERRORE applicativo.
///
/// PUNTO UNICO (regola L) della derivazione `is_error` dal testo del tool: il
/// contratto e' "il risultato inizia col marker `\u{274C}`" (prodotto dal
/// resolver `agent_tools::tool_not_found` e dai tool su fallimento). Prima questa
/// stessa condizione (`trim_start().starts_with('\u{274C}')`) era scritta in
/// `execute_tool`; ora la usano sia il path gRPC sia l'adapter `ToolExecutor` del
/// grafo Rust, cosi' Real (output di `execute_agent_tool`) e Replay (testo riletto
/// da `agent_steps`) classificano l'errore allo stesso modo.
pub(crate) fn tool_result_is_error(result: &str) -> bool {
    result.trim_start().starts_with('\u{274C}')
}

#[tonic::async_trait]
impl ToolRunner for ToolRunnerService {
    async fn execute_tool(
        &self,
        request: Request<ExecuteToolRequest>,
    ) -> Result<Response<ExecuteToolResponse>, Status> {
        let req = request.into_inner();
        let started = Instant::now();

        let session_id = Uuid::parse_str(&req.session_id)
            .map_err(|_| Status::invalid_argument("session_id non valido"))?;

        let input: Value = if req.tool_input_json.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&req.tool_input_json)
                .map_err(|e| Status::invalid_argument(format!("tool_input_json: {e}")))?
        };

        let ctx = self.build_ctx(session_id).await?;

        // ── ADR 0016 Fase A.5: cache cross-turn tool_result ───────────────
        // Lookup PRIMA dell'esecuzione. Hit → ritorna payload cached con
        // marker [cache_ref] nel content. Miss → esegue, cacha (se non in
        // skiplist) e ritorna. I tool con side-effect (run_command,
        // write_file, ecc.) non vengono cachati: cache_cfg.skip_for.
        let cache_cfg = crate::agent_tool_result_cache::CacheConfig::load(&self.deps.db).await;
        let cache_eligible = cache_cfg.should_cache(&req.tool_name);

        if cache_eligible {
            if let Some(hit) =
                crate::agent_tool_result_cache::lookup(&self.deps.db, &req.tool_name, &input).await
            {
                let duration_ms = started.elapsed().as_millis() as u64;
                tracing::info!(
                    tool = %req.tool_name,
                    age_s = hit.age_seconds,
                    cache_key = %&hit.cache_key[..12.min(hit.cache_key.len())],
                    "tool_result_cache: hit"
                );
                let exit_code = extract_exit_code(&hit.payload);
                return Ok(Response::new(ExecuteToolResponse {
                    tool_use_id: req.tool_use_id,
                    tool_result_json: hit.payload,
                    is_error: false,
                    duration_ms,
                    has_exit_code: exit_code.is_some(),
                    exit_code: exit_code.unwrap_or(0),
                }));
            }
        }

        let result = execute_agent_tool(&ctx, &req.tool_name, &input).await;

        // execute_agent_tool codifica l'errore come stringa che inizia
        // con il carattere '❌'. Lo mappiamo su is_error=true (punto unico
        // `tool_result_is_error`, riusato dall'adapter ToolExecutor del grafo).
        let is_error = tool_result_is_error(&result);
        let duration_ms = started.elapsed().as_millis() as u64;

        // Coerenza cache dopo mutazione (incidente Beauty-Book 2026-06-11): un
        // tool che muta il filesystem invalida le LETTURE cacheate (list_files,
        // read_file, ...), altrimenti il modello vede listing/contenuti
        // antecedenti alla propria modifica e produce resoconti falsi.
        // Best-effort in background: non allunga la risposta del tool.
        if !is_error && cache_cfg.is_mutator(&req.tool_name) {
            let db = self.deps.db.clone();
            let readers = cache_cfg.readers.clone();
            let mutator = req.tool_name.clone();
            tokio::spawn(async move {
                let n = crate::agent_tool_result_cache::invalidate_readers(&db, &readers).await;
                if n > 0 {
                    tracing::info!(
                        tool = %mutator,
                        invalidated = n,
                        "tool_result_cache: letture invalidate dopo mutazione"
                    );
                }
            });
        }

        // Store cache (best-effort, solo se ok e tool cacheable).
        if cache_eligible && !is_error {
            let db = self.deps.db.clone();
            let tool_name = req.tool_name.clone();
            let args_for_cache = input.clone();
            let payload = result.clone();
            let ttl = cache_cfg.ttl_seconds;
            tokio::spawn(async move {
                if let Err(e) = crate::agent_tool_result_cache::store(
                    &db,
                    &tool_name,
                    &args_for_cache,
                    &payload,
                    ttl,
                )
                .await
                {
                    tracing::debug!("tool_result_cache store fallita: {e}");
                }
            });
        }

        let exit_code = extract_exit_code(&result);
        Ok(Response::new(ExecuteToolResponse {
            tool_use_id: req.tool_use_id,
            tool_result_json: result,
            is_error,
            duration_ms,
            has_exit_code: exit_code.is_some(),
            exit_code: exit_code.unwrap_or(0),
        }))
    }

    type StreamToolOutputStream = ReceiverStream<Result<ToolChunk, Status>>;

    async fn stream_tool_output(
        &self,
        request: Request<ExecuteToolRequest>,
    ) -> Result<Response<Self::StreamToolOutputStream>, Status> {
        // Implementazione iniziale: esegue il tool in modalita' unaria
        // e invia un unico chunk "final" con il risultato. Lo streaming
        // incrementale vero (stdout tailing per run_service) verra'
        // agganciato in una iterazione successiva, riusando i
        // broadcast channel di AgentChannels.
        let req = request.into_inner();
        let started = Instant::now();
        let tool_use_id = req.tool_use_id.clone();
        let session_id = Uuid::parse_str(&req.session_id)
            .map_err(|_| Status::invalid_argument("session_id non valido"))?;
        let input: Value = if req.tool_input_json.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&req.tool_input_json)
                .map_err(|e| Status::invalid_argument(format!("tool_input_json: {e}")))?
        };
        let ctx = self.build_ctx(session_id).await?;

        let (tx, rx) = mpsc::channel(8);
        let tool_name = req.tool_name.clone();
        tokio::spawn(async move {
            let result = execute_agent_tool(&ctx, &tool_name, &input).await;
            let ts_ms = chrono::Utc::now().timestamp_millis() as u64;
            let _ = tx
                .send(Ok(ToolChunk {
                    tool_use_id: tool_use_id.clone(),
                    kind: "final".to_string(),
                    data: result,
                    ts_ms,
                }))
                .await;
            let _ = started; // silence unused
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Avvia il server tonic in un task dedicato. Restituisce immediatamente
/// dopo il bind. Se il bind fallisce, logga e ritorna Err (il caller
/// decide se abortire).
pub async fn spawn_tool_runner_server(
    deps: ToolRunnerDeps,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    let svc = ToolRunnerService::new(deps);
    tracing::info!("ToolRunner gRPC server in ascolto su {addr}");
    tokio::spawn(async move {
        // Limite gRPC esplicito: 64MB encoding/decoding per coerenza col client Python.
        // Il tool search_in_files tronca a 500KB, ma altri tool (read_file su file grandi)
        // possono legittimamente produrre risposte da qualche MB.
        const MAX_MSG: usize = 64 * 1024 * 1024;
        // Fix M33: retry con backoff su transport error.
        // Sintomo originale: dopo un restart di mcp-core, il bind tcp su :50071
        // poteva fallire con "transport error" se la porta era in stato TIME_WAIT
        // o se il processo precedente non l'aveva rilasciata. Senza retry il
        // ToolRunner restava down e tutti i tool agente fallivano gRPC UNAVAILABLE.
        const MAX_ATTEMPTS: u32 = 6;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let tool_runner_svc = ToolRunnerServer::new(svc.clone())
                .max_decoding_message_size(MAX_MSG)
                .max_encoding_message_size(MAX_MSG);
            // Bind via std::net per marcare il socket NON ereditabile su Windows: i
            // figli spawnati dall'agente non devono ereditare la porta gRPC, altrimenti
            // un orfano dopo un crash blocca il re-bind (come per :4000). Bind+serve in
            // un blocco unico: un errore di bind (es. TIME_WAIT) confluisce nel retry.
            let serve_result: Result<(), String> = async {
                let std_listener =
                    std::net::TcpListener::bind(addr).map_err(|e| format!("bind: {e}"))?;
                std_listener
                    .set_nonblocking(true)
                    .map_err(|e| format!("set_nonblocking: {e}"))?;
                #[cfg(windows)]
                crate::sandbox::make_socket_non_inheritable(&std_listener);
                let listener = tokio::net::TcpListener::from_std(std_listener)
                    .map_err(|e| format!("from_std: {e}"))?;
                let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
                Server::builder()
                    .add_service(tool_runner_svc)
                    .serve_with_incoming(incoming)
                    .await
                    .map_err(|e| format!("serve: {e}"))
            }
            .await;
            match serve_result {
                Ok(_) => {
                    tracing::info!("ToolRunner server terminato regolarmente");
                    return;
                }
                Err(e) => {
                    tracing::error!(
                        "ToolRunner server terminato con errore (tentativo {}/{}): {e}",
                        attempt,
                        MAX_ATTEMPTS
                    );
                    if attempt >= MAX_ATTEMPTS {
                        tracing::error!(
                            "ToolRunner: raggiunto limite {} tentativi, arresto definitivo",
                            MAX_ATTEMPTS
                        );
                        return;
                    }
                    // Backoff esponenziale capped a 30s: 2, 4, 8, 16, 30s
                    let secs = std::cmp::min(2u64.pow(attempt), 30);
                    tracing::warn!("ToolRunner: retry tra {}s", secs);
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests_ctx_root {
    use super::resolve_ctx_root;
    use std::path::{Path, PathBuf};

    // Override presente (sub-run isolato): la root del ctx e' quella del worktree
    // e isolated_subrun=true (leva soppressione autocommit/reindex, buco B2).
    #[test]
    fn override_root_impone_worktree_e_isolamento() {
        let session_root = PathBuf::from("/progetti/beaty");
        let worktree = Path::new("/tmp/.nexus-worktrees/beaty/run-1");
        let (root, isolated) = resolve_ctx_root(session_root, Some(worktree));
        assert_eq!(root, worktree, "root sovrascritta col worktree");
        assert!(isolated, "override presente -> isolated_subrun=true");
    }

    // Nessun override (default PR3): root invariata (project_root della sessione)
    // e isolated_subrun=false -> comportamento BIT-IDENTICO a oggi.
    #[test]
    fn nessun_override_root_invariata_e_non_isolato() {
        let session_root = PathBuf::from("/progetti/beaty");
        let (root, isolated) = resolve_ctx_root(session_root.clone(), None);
        assert_eq!(root, session_root, "root invariata (sessione)");
        assert!(!isolated, "nessun override -> isolated_subrun=false");
    }
}

#[cfg(test)]
mod tests_exit_code {
    use super::extract_exit_code;

    #[test]
    fn estrae_exit_code_da_output_run_command() {
        let out = "hints\nEXIT CODE: 0\nSTDOUT:\nok\nSTDERR:\n";
        assert_eq!(extract_exit_code(out), Some(0));
        let fail = "EXIT CODE: 1\nSTDOUT:\n\nSTDERR:\nboom";
        assert_eq!(extract_exit_code(fail), Some(1));
        let neg = "EXIT CODE: -1\nSTDOUT:";
        assert_eq!(extract_exit_code(neg), Some(-1));
        let big = "EXIT CODE: 127\nSTDOUT:";
        assert_eq!(extract_exit_code(big), Some(127));
    }

    #[test]
    fn nessun_marker_ritorna_none() {
        assert_eq!(
            extract_exit_code("output di read_file senza exit code"),
            None
        );
        assert_eq!(extract_exit_code(""), None);
    }
}
