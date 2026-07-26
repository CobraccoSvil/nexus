//! Contesto di esecuzione dei nodi: il tipo `C` passato a `GraphNode::run`.
//!
//! Aggrega tutte le dipendenze di un nodo: il pool DB condiviso, le PORTE I/O
//! astratte (LLM/tool/eventi, vedi `ports.rs`), la config DB-driven (regola G:
//! PASSATA, mai letta qui dentro), il cancellation token cooperativo e gli id
//! del run/sessione/thread.
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
    /// Esecutore di tool astratto (delega al ToolRunner gRPC).
    pub tools: Arc<dyn ToolExecutor>,
    /// Canale eventi verso il frontend.
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
    /// `true` se l'isolamento fisico dei sub-run (worktree git effimeri) e'
    /// DISPONIBILE per questo run: flag `orchestrator.subagent_isolation_enabled`
    /// ON E la root del progetto e' un repo git isolabile (probe fail-closed).
    /// Calcolato UNA volta da mcp-core al init del run (I/O: settings + probe git,
    /// regola M) e passato qui; i nodi puri (planner -> gate di orchestrazione)
    /// lo LEGGONO senza fare I/O. `false` (default) -> ogni `ParallelIsolated`
    /// degrada a `Sequential` (comportamento invariato). Con flag OFF il probe
    /// NON viene eseguito (corto-circuito): costo zero sul percorso normale.
    pub isolation_available: bool,
    /// Osservatore della BARRIERA DI SCRITTURA advisory (overlap consiglio ∥ run,
    /// mig 0606). `None` = nessun overlap (ramo legacy: il run parte solo DOPO i
    /// panel, e i loro verdetti sono gia' nello stato iniziale) -> il gate del
    /// ToolDispatchNode e' inerte, comportamento bit-identico.
    ///
    /// `Some(rx)`: il run e' partito SUBITO, mentre i panel deliberano in
    /// parallelo. I tool read-only girano liberi (la ricognizione e' il grosso
    /// del lavoro iniziale e non ha bisogno del consiglio); il primo tool
    /// MUTATIVO attende che la barriera si sciolga. E' un `watch`: leggerlo non
    /// consuma nulla, e chi arriva tardi vede subito l'ultimo stato.
    pub advisory_gate: Option<tokio::sync::watch::Receiver<crate::nodes::AdvisoryGateState>>,
}

/// Adattatore al motore di grafo (`nexus_graph::GraphEngine`): il motore richiede
/// solo `recursion_limit()` dal contesto. Lo legge dalla `RoutingConfig`
/// DB-driven (regola G: PASSATA nel ctx, mai letta dal DB qui), cosi' il cap
/// anti-loop del grafo e' configurabile senza toccare il motore puro.
impl nexus_graph::engine::NodeCtxLike for AgentNodeCtx {
    fn recursion_limit(&self) -> u32 {
        self.cfg.recursion_limit
    }
}
