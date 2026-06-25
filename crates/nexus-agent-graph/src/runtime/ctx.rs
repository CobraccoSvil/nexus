//! Contesto di esecuzione dei nodi: il tipo `C` passato a `GraphNode::run`.
//!
//! Aggrega tutte le dipendenze di un nodo: il pool DB condiviso, le PORTE I/O
//! astratte (LLM/tool/eventi, vedi `ports.rs`), la config DB-driven (regola G:
//! PASSATA, mai letta qui dentro), il cancellation token cooperativo e gli id
//! del run/sessione/thread. Lo `shadow` flag distingue il run primario da quello
//! shadow (read-only): i nodi lo leggono per decidere la `ExecMode` dei tool.
//!
//! Le porte sono `Arc<dyn Trait>`: il ctx e' clonabile a basso costo e
//! condivisibile fra task. mcp-core costruira' questo ctx iniettando le proprie
//! implementazioni concrete (inversione di dipendenza, vedi `ports.rs`).

use std::sync::Arc;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::routing::config::RoutingConfig;
use crate::runtime::ports::{EventSink, LlmGateway, ToolExecutor};

/// Contesto di esecuzione condiviso da tutti i nodi del grafo agentico.
///
/// E' il parametro `C` del trait `GraphNode<AgentState, AgentNodeCtx>`. Clona a
/// basso costo (DB pool e porte sono `Arc` internamente; `cfg` e' piccolo).
#[derive(Clone)]
pub struct AgentNodeCtx {
    /// Pool Postgres condiviso del processo (riusato, mai connection string
    /// hardcoded — regola G).
    pub db: PgPool,
    /// Gateway LLM astratto (mcp-core delega a nexus-gateway).
    pub llm: Arc<dyn LlmGateway>,
    /// Esecutore di tool astratto (Real -> ToolRunner gRPC, Replay -> shadow).
    pub tools: Arc<dyn ToolExecutor>,
    /// Canale eventi verso il frontend (no-op nel ctx shadow).
    pub emit: Arc<dyn EventSink>,
    /// Config DB-driven del routing, PASSATA (regola G): nessuna lettura DB qui.
    pub cfg: RoutingConfig,
    /// Token di cancellazione cooperativa: i nodi lo controllano per fermarsi
    /// in modo pulito su stop/supersede (niente kill -9, regola H).
    pub cancel: CancellationToken,
    /// Id del run Nexus (= thread LangGraph durante la coesistenza).
    pub run_id: Uuid,
    /// Id della sessione chat.
    pub session_id: Uuid,
    /// Id del thread del grafo (allineato a run_id nel runtime Rust).
    pub thread_id: Uuid,
    /// `true` se questo e' un run SHADOW (read-only, tool in Replay, no eventi):
    /// confronta col primario senza side-effect ne' output verso l'utente.
    pub shadow: bool,
}

impl AgentNodeCtx {
    /// Modalita' d'esecuzione tool derivata dal flag shadow (punto unico, regola
    /// L): un run shadow usa SEMPRE `Replay` (zero side-effect), il primario
    /// `Real`. I nodi non duplicano questo `if`: chiamano `exec_mode()`.
    pub fn exec_mode(&self) -> crate::runtime::ports::ExecMode {
        if self.shadow {
            crate::runtime::ports::ExecMode::Replay
        } else {
            crate::runtime::ports::ExecMode::Real
        }
    }
}
