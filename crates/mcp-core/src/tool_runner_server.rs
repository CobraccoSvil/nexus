// Server gRPC ToolRunner: superficie minima esposta da mcp-core verso il
// brain (Python/LangGraph). Il brain orchestra il loop agent; qui
// eseguiamo solo tool singoli, stateless, correlati a una chat session.
//
// Contratto: vedi proto/tool_runner.proto.

use std::net::SocketAddr;
use std::path::PathBuf;
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
use crate::orchestrator::{AutomationMode, NeuralCoreClient};
use crate::prompt_templates::TemplateCache;
use crate::{AgentChannels, TerminalConsumers};

/// Dipendenze condivise iniettate nel service. E' una slice sottile di
/// AppState: teniamo qui solo cio' che serve per costruire un
/// AgentToolContext e per eseguire un tool.
#[derive(Clone)]
pub struct ToolRunnerDeps {
    pub db: PgPool,
    pub neural: NeuralCoreClient,
    pub agent_channels: AgentChannels,
    pub playwright_channels: crate::playwright_live::PlaywrightChannels,
    pub terminal_consumers: TerminalConsumers,
    pub template_cache: TemplateCache,
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

impl ToolRunnerService {
    pub fn new(deps: ToolRunnerDeps) -> Self {
        Self { deps }
    }

    /// Risolve la sessione chat in (project_id, user_id, root_path,
    /// is_git_repo, can_write, user_role) con una singola query.
    /// Il brain e' trusted: non applichiamo qui il check di accesso
    /// (avviene gia' a monte in chat-service al momento dell'invio
    /// del messaggio utente).
    async fn resolve_session(&self, session_id: Uuid) -> Result<SessionInfo, Status> {
        let row = sqlx::query(
            r#"
            SELECT
                s.project_id,
                s.user_id,
                COALESCE(r.root_path, w.absolute_path) AS root_path,
                COALESCE(r.is_git_repo, FALSE)         AS is_git_repo,
                CASE
                    WHEN p.owner_user_id = s.user_id THEN 'owner'
                    ELSE COALESCE(pm.role, 'viewer')
                END AS role
            FROM chat_sessions s
            JOIN projects p ON p.id = s.project_id
            LEFT JOIN project_members pm
                ON pm.project_id = s.project_id AND pm.user_id = s.user_id
            LEFT JOIN workspaces w
                ON w.project_id = s.project_id AND w.is_primary = TRUE
            LEFT JOIN repositories r
                ON r.project_id = s.project_id
            WHERE s.id = $1
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.deps.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("session non trovata"))?;

        let project_id: Uuid = row
            .try_get("project_id")
            .map_err(|e| Status::internal(format!("project_id: {e}")))?;
        let user_id: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
        let user_id = user_id.ok_or_else(|| Status::failed_precondition("session senza user"))?;
        let root_path: String = row
            .try_get("root_path")
            .map_err(|_| Status::failed_precondition("workspace non configurato"))?;
        let is_git_repo: bool = row.try_get("is_git_repo").unwrap_or(false);
        let role: String = row.try_get("role").unwrap_or_else(|_| "viewer".to_string());
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

    async fn build_ctx(&self, session_id: Uuid) -> Result<AgentToolContext, Status> {
        let info = self.resolve_session(session_id).await?;
        let long_running_patterns = crate::long_running::load_enabled_patterns(&self.deps.db).await;
        Ok(AgentToolContext {
            root_path: info.root_path,
            user_id: info.user_id,
            is_git_repo: info.is_git_repo,
            can_write: info.can_write,
            project_id: info.project_id,
            session_id: Some(session_id),
            db: Arc::new(self.deps.db.clone()),
            parent_run_id: None,
            agent_channels: self.deps.agent_channels.clone(),
            playwright_channels: self.deps.playwright_channels.clone(),
            neural: self.deps.neural.clone(),
            automation_mode: AutomationMode::Automatic,
            terminal_consumers: self.deps.terminal_consumers.clone(),
            long_running_patterns,
            template_cache: self.deps.template_cache.clone(),
            user_role: info.user_role.clone(),
            is_nexus_operator: matches!(info.user_role.as_str(), "owner" | "admin"),
            dependency_status: self.deps.dependency_status.clone(),
            project_channels: self.deps.project_channels.clone(),
            monitor_registry: self.deps.monitor_registry.clone(),
            port_registry: self.deps.port_registry.clone(),
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

/// Estrae l'exit code dal testo "EXIT CODE: N" emesso da run_command &c.
/// (formato CONTROLLATO da noi in agent_tools/command.rs — questo e' un parser
/// del nostro stesso output, non un'euristica sul testo del modello). Punto
/// unico Rust della traduzione testo->strutturato: prima ogni consumer Python
/// ri-parsava la stringa con regex (`EXIT CODE: N`); ora il valore viaggia
/// strutturato nel proto (contratto dati A, censimento 2026-06-10).
fn extract_exit_code(result: &str) -> Option<i32> {
    let marker = "EXIT CODE: ";
    let start = result.find(marker)? + marker.len();
    let rest = &result[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].trim().parse::<i32>().ok()
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
        // con il carattere '❌'. Lo mappiamo su is_error=true.
        let is_error = result.trim_start().starts_with('\u{274C}');
        let duration_ms = started.elapsed().as_millis() as u64;

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
            match Server::builder()
                .add_service(tool_runner_svc)
                .serve(addr)
                .await
            {
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
        assert_eq!(extract_exit_code("output di read_file senza exit code"), None);
        assert_eq!(extract_exit_code(""), None);
    }
}
