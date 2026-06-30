// Server gRPC AgentRouter: espone il router Q-Learning di nexus-orchestrator
// al brain Python/LangGraph. Il brain chiama SelectAgent come sub-nodo del
// router semantico; al termine dell'esecuzione chiama SubmitFeedback per
// aggiornare il Q-value.
//
// Contratto: vedi proto/agent_router.proto.
//
// Implementazione: delega a NexusBridge (singleton globale dentro mcp-core)
// che gia' mantiene QLearningRouter + HNSW + persistenza. Se NexusBridge
// non e' inizializzato (ambienti test/staging senza DB), le RPC ritornano
// un fallback deterministico (agent vuoto / q_value 0).

use std::net::SocketAddr;

use mcp_proto::agent_router::agent_router_server::{AgentRouter, AgentRouterServer};
use mcp_proto::agent_router::{
    CandidateAgent as PbCandidate, FeedbackRequest, FeedbackResponse, SelectAgentRequest,
    SelectAgentResponse,
};
use nexus_orchestrator::AgentType;
use nexus_orchestrator::SelectionStrategy;
use tonic::{transport::Server, Request, Response, Status};

use crate::nexus_bridge::NexusBridge;

/// Converte un nome "brain-style" (snake_case, es. "coder", "github_pr_manager")
/// nel nome PascalCase atteso da `AgentType::from_name`. Idempotente: se il
/// nome e' gia' PascalCase viene restituito invariato.
fn snake_to_pascal(name: &str) -> String {
    if name.chars().next().map(char::is_uppercase).unwrap_or(false) && !name.contains('_') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    // Casi speciali per sigle note: "github" -> "GitHub", "sre" -> "SRE",
    // "api" -> "API", "ml" -> "ML", "qa" -> "QA", "ui" -> "UI".
    // La normalizzazione base capitalizza solo la prima lettera di ogni
    // segmento; il registry Rust ha varianti enum gia' allineate (es.
    // "GitHubPRManager"): qui applichiamo override minimi.
    out.replace("Github", "GitHub")
        .replace("Sre", "SRE")
        .replace("Api", "API")
        .replace("Ml", "ML")
        .replace("Qa", "QA")
        .replace("Ui", "UI")
        .replace("Etl", "ETL")
        .replace("PrManager", "PRManager")
}

/// Converte un nome PascalCase (AgentType::name()) in snake_case per il
/// matching con i profili brain/agents/profiles/<name>.yaml.
fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            // Evita doppio underscore in acronimi consecutivi (es. PRManager).
            if !out.ends_with('_') {
                out.push('_');
            }
        }
        out.extend(ch.to_lowercase());
    }
    out
}

fn strategy_to_string(s: &SelectionStrategy) -> &'static str {
    match s {
        SelectionStrategy::Exploitation => "EXPLOITATION",
        SelectionStrategy::Exploration => "EXPLORATION",
        SelectionStrategy::ColdStart => "COLD_START",
        SelectionStrategy::Forced => "FORCED",
    }
}

pub struct AgentRouterService;

impl AgentRouterService {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentRouterService {
    fn default() -> Self {
        Self::new()
    }
}

#[tonic::async_trait]
impl AgentRouter for AgentRouterService {
    async fn select_agent(
        &self,
        request: Request<SelectAgentRequest>,
    ) -> Result<Response<SelectAgentResponse>, Status> {
        let req = request.into_inner();

        // Forced: converte direttamente il nome e bypassa il Q-learning.
        if !req.forced_agent_type.is_empty() {
            let pascal = snake_to_pascal(&req.forced_agent_type);
            let agent_type = AgentType::from_name(&pascal);
            let snake = pascal_to_snake(agent_type.name());
            let candidates = vec![PbCandidate {
                agent_type: snake.clone(),
                similarity_score: 1.0,
                q_value: 1.0,
            }];
            return Ok(Response::new(SelectAgentResponse {
                agent_type: snake,
                q_value: 1.0,
                confidence: 1.0,
                candidates,
                strategy: "FORCED".to_string(),
                decision_time_us: 0,
            }));
        }

        let bridge = match NexusBridge::global() {
            Some(b) => b,
            None => {
                tracing::debug!(
                    "agent_router: NexusBridge non inizializzato, rispondo fallback vuoto"
                );
                return Ok(Response::new(SelectAgentResponse {
                    agent_type: String::new(),
                    q_value: 0.0,
                    confidence: 0.0,
                    candidates: vec![],
                    strategy: "COLD_START".to_string(),
                    decision_time_us: 0,
                }));
            }
        };

        let project_id = if req.context_json.is_empty() {
            "default".to_string()
        } else {
            // context_json puo' contenere project_id: best-effort estrazione.
            serde_json::from_str::<serde_json::Value>(&req.context_json)
                .ok()
                .and_then(|v| {
                    v.get("project_id")
                        .and_then(|x| x.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "default".to_string())
        };

        match bridge.suggest_agent(&req.task_type, &req.instructions, &project_id) {
            Some(decision) => {
                let candidates: Vec<PbCandidate> = decision
                    .candidates
                    .iter()
                    .map(|c| PbCandidate {
                        agent_type: pascal_to_snake(c.agent_type.name()),
                        similarity_score: c.similarity_score,
                        q_value: c.q_value,
                    })
                    .collect();
                Ok(Response::new(SelectAgentResponse {
                    agent_type: pascal_to_snake(decision.agent_type.name()),
                    q_value: decision.q_value,
                    confidence: decision.confidence,
                    candidates,
                    strategy: strategy_to_string(&decision.strategy).to_string(),
                    decision_time_us: decision.decision_time_us,
                }))
            }
            None => Ok(Response::new(SelectAgentResponse {
                agent_type: String::new(),
                q_value: 0.0,
                confidence: 0.0,
                candidates: vec![],
                strategy: "COLD_START".to_string(),
                decision_time_us: 0,
            })),
        }
    }

    async fn submit_feedback(
        &self,
        request: Request<FeedbackRequest>,
    ) -> Result<Response<FeedbackResponse>, Status> {
        let req = request.into_inner();

        let bridge = match NexusBridge::global() {
            Some(b) => b,
            None => {
                tracing::debug!("agent_router: feedback ricevuto ma NexusBridge assente, no-op");
                return Ok(Response::new(FeedbackResponse { new_q_value: 0.0 }));
            }
        };

        let pascal = snake_to_pascal(&req.agent_type);
        let agent_type = AgentType::from_name(&pascal);
        let reward = req.reward.clamp(0.0, 1.0);
        let success = reward >= 0.5;
        let new_q = bridge.record_outcome(
            &req.task_id,
            &req.task_type,
            agent_type,
            success,
            reward,
            req.duration_ms,
            if success {
                None
            } else {
                Some("reward<0.5".to_string())
            },
        );

        Ok(Response::new(FeedbackResponse { new_q_value: new_q }))
    }

    async fn get_agent_metrics(
        &self,
        request: Request<mcp_proto::agent_router::GetAgentMetricsRequest>,
    ) -> Result<Response<mcp_proto::agent_router::GetAgentMetricsResponse>, Status> {
        let req = request.into_inner();

        let bridge = match NexusBridge::global() {
            Some(b) => b,
            None => {
                tracing::debug!("agent_router: GetAgentMetrics ma NexusBridge assente");
                return Ok(Response::new(
                    mcp_proto::agent_router::GetAgentMetricsResponse {
                        average_latency_ms: 0.0,
                        average_cost_usd: 0.0,
                        average_reward: 0.0,
                        q_value: 0.0,
                        success_count: 0,
                        failure_count: 0,
                        total_tokens_processed: 0,
                        last_updated_at: String::new(),
                    },
                ));
            }
        };

        let pascal = snake_to_pascal(&req.agent_type);
        let agent_type = AgentType::from_name(&pascal);
        let limit = req.limit.clamp(1, 100) as usize;

        let metrics = bridge.get_agent_metrics(&agent_type, limit);

        Ok(Response::new(
            mcp_proto::agent_router::GetAgentMetricsResponse {
                average_latency_ms: metrics.average_latency_ms,
                average_cost_usd: metrics.average_cost_usd,
                average_reward: metrics.average_reward,
                q_value: metrics.q_value,
                success_count: metrics.success_count,
                failure_count: metrics.failure_count,
                total_tokens_processed: metrics.total_tokens_processed,
                last_updated_at: metrics.last_updated_at,
            },
        ))
    }
}

/// Avvia il server tonic AgentRouter. Torna immediatamente dopo il bind.
pub async fn spawn_agent_router_server(addr: SocketAddr) -> anyhow::Result<()> {
    let svc = AgentRouterService::new();
    tracing::info!("AgentRouter gRPC server in ascolto su {addr}");
    tokio::spawn(async move {
        // Bind via std::net per marcare il socket NON ereditabile su Windows (i figli
        // dell'agente non ereditano la porta gRPC -> niente blocco al re-bind dopo un
        // crash, come per :4000), poi serve_with_incoming.
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
                .add_service(AgentRouterServer::new(svc))
                .serve_with_incoming(incoming)
                .await
                .map_err(|e| format!("serve: {e}"))
        }
        .await;
        if let Err(e) = serve_result {
            tracing::error!("AgentRouter server terminato con errore: {e}");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snake_to_pascal_basic() {
        assert_eq!(snake_to_pascal("coder"), "Coder");
        assert_eq!(snake_to_pascal("tech_writer"), "TechWriter");
    }

    #[test]
    fn test_snake_to_pascal_github() {
        assert_eq!(snake_to_pascal("github_pr_manager"), "GitHubPRManager");
        assert_eq!(
            snake_to_pascal("github_code_reviewer"),
            "GitHubCodeReviewer"
        );
    }

    #[test]
    fn test_snake_to_pascal_acronyms() {
        assert_eq!(snake_to_pascal("sre_engineer"), "SREEngineer");
        assert_eq!(snake_to_pascal("api_designer"), "APIDesigner");
        assert_eq!(snake_to_pascal("ml_engineer"), "MLEngineer");
        assert_eq!(snake_to_pascal("ui_designer"), "UIDesigner");
    }

    #[test]
    fn test_snake_to_pascal_idempotent_on_pascal() {
        assert_eq!(snake_to_pascal("Coder"), "Coder");
    }

    #[test]
    fn test_pascal_to_snake_basic() {
        assert_eq!(pascal_to_snake("Coder"), "coder");
        assert_eq!(pascal_to_snake("TechWriter"), "tech_writer");
    }

    #[test]
    fn test_pascal_to_snake_github() {
        // GitHubPRManager -> github_pr_manager (gli acronimi consecutivi
        // si comprimono in un unico segmento snake).
        let out = pascal_to_snake("GitHubPRManager");
        assert!(out.starts_with("git"));
        assert!(out.contains("manager"));
    }
}
