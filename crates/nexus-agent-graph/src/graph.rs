//! Assemblaggio del grafo agentico Nexus: cabla i nodi concreti nel motore puro
//! `nexus_graph::GraphEngine`.
//!
//! Il MOTORE (loop superstep, merge, route, checkpoint, recursion_limit,
//! interrupt su HITL) vive in `nexus-graph` (crate puro): qui NON lo
//! re-implementiamo (regola L). Questo modulo costruisce SOLO la TOPOLOGIA — la
//! mappa `NodeId -> nodo concreto` e gli `Edge` uscenti — replicando 1:1
//! `brain/agents/graph.py` (`create_agent_graph`).
//!
//! ## Topologia (1:1 con graph.py)
//!
//! Entry: `router`.
//!
//! Edge fissi:
//! ```text
//!   router        -> clarify_or_expand
//!   tool_dispatch -> executor          (loop agentico)
//!   reflection    -> learner
//!   learner       -> END
//! ```
//!
//! Edge condizionali (delegano alle `route_after_*` di `routing/mod.rs`, gia'
//! portate 1:1):
//! ```text
//!   clarify_or_expand -> understanding | END (pending_clarify -> END: terminale)
//!   understanding     -> planner | executor (PlannerConfig::is_eligible)
//!   planner           -> route_after_planner     -> todo_runner | executor
//!   todo_runner       -> route_after_todo_runner -> todo_runner | final_gate | executor | reflection
//!   executor          -> route_after_executor    -> tool_dispatch | verifier | final_gate | reflection | executor(g1)
//!   final_gate        -> route_after_final_gate  -> executor | reflection
//!   verifier          -> route_after_verifier    -> executor | reflection
//! ```
//!
//! ## Differenze STRUTTURALI volute rispetto a graph.py (regola H, non toppe)
//!
//! - **`g1_continue` eliminato**: in graph.py e' un nodo passthrough che esiste
//!   solo per spezzare il self-loop `executor -> executor` non materializzato dal
//!   checkpointer custom Python. Nel runtime Rust il `GraphEngine` gestisce
//!   nativamente l'auto-arco di un nodo (e fa checkpoint a ogni superstep),
//!   quindi `NodeTarget::G1Continue` mappa DIRETTAMENTE su `NodeId::Executor`
//!   (vedi `node.rs`, commento sull'assenza di `g1_continue`).
//! - **`reflection` come destinazione di "learner"**: in graph.py il target
//!   `"learner"` delle `route_after_*` e' rimappato al nodo `reflection`
//!   (`{"learner": "reflection"}`), che poi va a `learner -> END`. Replicato:
//!   `NodeTarget::Learner -> NodeId::Reflection`.
//! ## `clarify_or_expand` -> END su `pending_clarify`: terminale, non interrupt
//!
//! In graph.py e' un edge CONDIZIONALE (`_route_after_clarify_or_expand`): con
//! `pending_clarify` instrada a `END` e il run CHIUDE (Completed). Il prossimo
//! messaggio utente avvia un NUOVO run dall'entry `router`. Replicato 1:1 con un
//! `Edge::conditional` a `NodeId::End` (vedi `build_edges`). NON e' un
//! interrupt-resume: il motore (engine.rs) sospende SOLO su
//! `awaiting_confirmation` (HITL vero, `interrupt_before=["executor"]` di
//! graph.py). Distinzione load-bearing: un interrupt riprenderebbe lo STESSO run
//! dal nodo instradato (saltando router+clarify), divergendo da graph.py.
//!
//! ## `route_after_router` (eligibilita' planner)
//!
//! L'edge `understanding -> {planner|executor}` delega al PUNTO UNICO
//! [`crate::nodes::PlannerConfig::is_eligible`] (regola L: non re-implementiamo
//! la decisione). La variante `is_eligible_adaptive` Python (segnali classifier)
//! e' un superset OPZIONALE non ancora portato (come il classifier LLM del
//! `RouterNode`): qui si usa il gate base, comportamento definito e testabile.

use std::collections::HashMap;
use std::sync::Arc;

use nexus_graph::edge::Edge;
use nexus_graph::engine::GraphEngine;
use nexus_graph::node::{GraphNode, NodeId};

use crate::nodes::PlannerConfig;
use crate::routing::{
    self, route_after_executor, route_after_final_gate, route_after_planner,
    route_after_todo_runner, route_after_verifier, NodeTarget, RoutingConfig,
};
use crate::runtime::AgentNodeCtx;
use crate::state::AgentState;

/// Alias del nodo del grafo agentico (stato `AgentState`, contesto `AgentNodeCtx`).
pub type AgentGraphNode = dyn GraphNode<AgentState, AgentNodeCtx>;

/// Alias del motore del grafo agentico gia' istanziato sui tipi Nexus.
pub type AgentGraphEngine = GraphEngine<AgentState, AgentNodeCtx>;

/// I nodi concreti del grafo, gia' costruiti (con le porte I/O iniettate).
///
/// Il chiamante (mcp-core, PR di integrazione) li costruisce con le impl
/// CONCRETE dei trait; i test li costruiscono con gli stub. Il builder li riceve
/// gia' pronti: non conosce ne' le config dei singoli nodi ne' le porte.
pub struct AgentGraphNodes {
    /// Nodo router (passthrough/classificazione).
    pub router: Arc<AgentGraphNode>,
    /// Gate di disambiguazione/espansione.
    pub clarify_or_expand: Arc<AgentGraphNode>,
    /// Comprensione pre-planning (pass-through se OFF).
    pub understanding: Arc<AgentGraphNode>,
    /// Pianificatore (pass-through se non eligibile).
    pub planner: Arc<AgentGraphNode>,
    /// Esecuzione sequenziale isolata dei todo.
    pub todo_runner: Arc<AgentGraphNode>,
    /// Executor principale del turno.
    pub executor: Arc<AgentGraphNode>,
    /// Dispatch dei tool pendenti (loop agentico).
    pub tool_dispatch: Arc<AgentGraphNode>,
    /// Meta-reasoner di recovery-da-stallo (superstep dedicato; self-loop verso
    /// l'executor). Inerte a flag OFF (porta `MetaReasonerPort` che ritorna
    /// `Ok(None)`).
    pub stall_recovery: Arc<AgentGraphNode>,
    /// Verifica plan-phase (DoD).
    pub verifier: Arc<AgentGraphNode>,
    /// Verifica E2E pre-chiusura.
    pub final_gate: Arc<AgentGraphNode>,
    /// Self-reflection (gate sampling).
    pub reflection: Arc<AgentGraphNode>,
    /// Chiusura del run (learning/persistenza).
    pub learner: Arc<AgentGraphNode>,
}

/// Mappa il bersaglio di routing (`NodeTarget`, ritorno delle `route_after_*`) sul
/// nodo del motore (`NodeId`). PURA (punto unico della rimappatura, regola L):
/// l'unico posto in cui si traduce il contratto di routing nella topologia del
/// motore. Le due differenze strutturali volute (vedi doc di modulo) sono qui:
///   - `G1Continue -> Executor` (no nodo passthrough: self-loop nativo);
///   - `Learner -> Reflection` (graph.py rimappa `"learner"` al nodo reflection).
pub fn node_target_to_node_id(target: NodeTarget) -> NodeId {
    match target {
        NodeTarget::ToolDispatch => NodeId::ToolDispatch,
        NodeTarget::Verifier => NodeId::Verifier,
        NodeTarget::FinalGate => NodeId::FinalGate,
        NodeTarget::Executor => NodeId::Executor,
        NodeTarget::TodoRunner => NodeId::TodoRunner,
        NodeTarget::StallRecovery => NodeId::StallRecovery,
        // Self-loop G1 nativo nel motore (no nodo passthrough, regola H).
        NodeTarget::G1Continue => NodeId::Executor,
        // graph.py: il target "learner" delle route_after_* va al nodo reflection
        // (reflection -> learner -> END). Punto di chiusura del run.
        NodeTarget::Learner => NodeId::Reflection,
    }
}

/// Costruisce la mappa degli `Edge` uscenti da ogni nodo (la topologia).
///
/// Riceve la `RoutingConfig` (catturata dalle closure condizionali) e la
/// `PlannerConfig` (per l'eligibilita' planner dell'edge understanding).
/// Entrambe sono clonate nelle closure (`'static`), come nel runtime reale dove
/// vengono risolte a monte (regola G).
fn build_edges(
    routing_cfg: RoutingConfig,
    planner_cfg: PlannerConfig,
) -> HashMap<NodeId, Edge<AgentState>> {
    let mut edges: HashMap<NodeId, Edge<AgentState>> = HashMap::new();

    // ── Edge fissi (graph.py:164, 237, 246, 247) ─────────────────────────────
    // router -> clarify_or_expand (sempre).
    edges.insert(NodeId::Router, Edge::Static(NodeId::ClarifyOrExpand));
    // tool_dispatch -> executor (rientro nel loop agentico).
    edges.insert(NodeId::ToolDispatch, Edge::Static(NodeId::Executor));
    // stall_recovery -> executor (rientro nel loop agentico dopo il superstep di
    // recovery). Il nodo emette sempre StopReason::StallResolved e torna
    // nell'executor, che consuma la RecoveryMove eventualmente persistita in extra
    // (self-loop, analogo a `G1Escalated -> executor`). INERTE oggi: nessun
    // detector emette StallReason, quindi il nodo non e' mai raggiunto.
    edges.insert(NodeId::StallRecovery, Edge::Static(NodeId::Executor));
    // reflection -> learner -> END (chiusura del run).
    edges.insert(NodeId::Reflection, Edge::Static(NodeId::Learner));
    edges.insert(NodeId::Learner, Edge::End);

    // ── clarify_or_expand -> understanding | END (graph.py:171-180) ───────────
    // Edge CONDIZIONALE 1:1 con `_route_after_clarify_or_expand` di graph.py:
    //   - pending_clarify -> END: il run e' TERMINALE (Completed). Il turno si
    //     ferma in attesa dell'utente; il prossimo messaggio avvia un NUOVO run
    //     dall'entry `router` (NON un resume dello stesso run).
    //   - altrimenti -> understanding (Cluster 2).
    // NB: pending_clarify NON e' un interrupt nativo del motore (vedi engine.rs):
    // il solo interrupt-resume e' `awaiting_confirmation` (HITL vero). Qui la
    // chiusura e' pura topologia, come in Python.
    edges.insert(
        NodeId::ClarifyOrExpand,
        Edge::conditional(|state: &AgentState| {
            if state.is_pending_clarify() {
                NodeId::End
            } else {
                NodeId::Understanding
            }
        }),
    );

    // ── understanding -> planner | executor (graph.py:182-186, route_after_router) ─
    // Delega al punto unico PlannerConfig::is_eligible (regola L). I tre segnali
    // (behavior_mode, intent, token_budget) sono letti dallo stato post-router.
    edges.insert(
        NodeId::Understanding,
        Edge::conditional(move |state: &AgentState| {
            let eligible = planner_cfg.is_eligible(
                state.behavior_mode.as_deref(),
                state.user_intent.as_deref(),
                state.token_budget.unwrap_or(0),
            );
            if eligible {
                NodeId::Planner
            } else {
                NodeId::Executor
            }
        }),
    );

    // ── planner -> route_after_planner (graph.py:191-195) ────────────────────
    let cfg_planner = routing_cfg.clone();
    edges.insert(
        NodeId::Planner,
        Edge::conditional(move |state: &AgentState| {
            node_target_to_node_id(route_after_planner(state, &cfg_planner))
        }),
    );

    // ── todo_runner -> route_after_todo_runner (graph.py:198-207) ────────────
    let cfg_todo = routing_cfg.clone();
    edges.insert(
        NodeId::TodoRunner,
        Edge::conditional(move |state: &AgentState| {
            node_target_to_node_id(route_after_todo_runner(state, &cfg_todo))
        }),
    );

    // ── executor -> route_after_executor (graph.py:216-226) ──────────────────
    let cfg_exec = routing_cfg.clone();
    edges.insert(
        NodeId::Executor,
        Edge::conditional(move |state: &AgentState| {
            node_target_to_node_id(route_after_executor(state, &cfg_exec))
        }),
    );

    // ── final_gate -> route_after_final_gate (graph.py:231-235) ──────────────
    edges.insert(
        NodeId::FinalGate,
        Edge::conditional(move |state: &AgentState| {
            node_target_to_node_id(route_after_final_gate(state))
        }),
    );

    // ── verifier -> route_after_verifier (graph.py:240-244) ──────────────────
    let cfg_verifier = routing_cfg;
    edges.insert(
        NodeId::Verifier,
        Edge::conditional(move |state: &AgentState| {
            node_target_to_node_id(route_after_verifier(state, &cfg_verifier))
        }),
    );

    edges
}

/// Costruisce la mappa `NodeId -> nodo concreto`.
fn build_node_map(
    nodes: AgentGraphNodes,
) -> HashMap<NodeId, Arc<AgentGraphNode>> {
    let mut map: HashMap<NodeId, Arc<AgentGraphNode>> = HashMap::new();
    map.insert(NodeId::Router, nodes.router);
    map.insert(NodeId::ClarifyOrExpand, nodes.clarify_or_expand);
    map.insert(NodeId::Understanding, nodes.understanding);
    map.insert(NodeId::Planner, nodes.planner);
    map.insert(NodeId::TodoRunner, nodes.todo_runner);
    map.insert(NodeId::Executor, nodes.executor);
    map.insert(NodeId::ToolDispatch, nodes.tool_dispatch);
    map.insert(NodeId::StallRecovery, nodes.stall_recovery);
    map.insert(NodeId::Verifier, nodes.verifier);
    map.insert(NodeId::FinalGate, nodes.final_gate);
    map.insert(NodeId::Reflection, nodes.reflection);
    map.insert(NodeId::Learner, nodes.learner);
    map
}

/// Assembla il grafo agentico completo nel motore puro.
///
/// - `nodes`: i nodi concreti gia' costruiti (porte iniettate dal chiamante).
/// - `routing_cfg`: config DB-driven delle `route_after_*` (regola G, passata).
/// - `planner_cfg`: config del planner, usata SOLO per l'eligibilita' dell'edge
///   `understanding -> planner|executor` (gli altri usi del planner sono dentro
///   il nodo).
/// - `checkpointer`: persistenza dello stato per-superstep (Postgres in
///   produzione, in-memory nei test).
///
/// Ritorna il [`GraphEngine`] pronto a `run_until_interrupt`. L'entry point e'
/// `router`, identico a graph.py (`set_entry_point("router")`).
pub fn build_agent_graph(
    nodes: AgentGraphNodes,
    routing_cfg: RoutingConfig,
    planner_cfg: PlannerConfig,
    checkpointer: Arc<dyn nexus_graph::checkpoint::Checkpointer<AgentState>>,
) -> AgentGraphEngine {
    let node_map = build_node_map(nodes);
    let edges = build_edges(routing_cfg, planner_cfg);
    GraphEngine::new(node_map, edges, NodeId::Router, checkpointer)
}

/// Riferimento al modulo di routing per i doc-link (`routing::NodeTarget`).
#[allow(unused_imports)]
use routing as _routing_doc_anchor;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;
    use nexus_graph::checkpoint::{CheckpointError, Checkpointer};
    use nexus_graph::outcome::StepOutcome;
    use nexus_graph::GraphError;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use crate::nodes::{
        ClarifyConfig, ClarifyOrExpandNode, ExecutorConfig, ExecutorNode, FinalGateConfig,
        FinalGateNode, LearnerConfig, LearnerNode, PlannerNode, ReflectionConfig, ReflectionNode,
        RouterNode, StallRecoveryNode, TodoRunnerConfig, TodoRunnerNode, ToolDispatchConfig,
        ToolDispatchNode, UnderstandingConfig, UnderstandingNode, VerifierConfig, VerifierNode,
    };
    use crate::runtime::ports::{
        AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, CriterionResult,
        EscalationPort, ExecMode, LlmGateway, LlmRequest, LlmResponse, LlmUsage, MetaStepStore,
        ModelUpscalePort, NextActionsDeriver, PortError, RunControlStore, SummaryStore, TodoStore,
        ToolCall, ToolExecutor, ToolOutcome, VerifierRunStore,
    };
    use crate::runtime::test_doubles::{
        NullEventSink, StubAgentStepStore, StubBillingCooldownPort, StubContextOffload,
        StubCriteriaRunner, StubEscalationPort, StubMetaStepStore, StubModelUpscalePort,
        StubNextActionsDeriver, StubRunControlStore, StubSummaryStore, StubTodoStore,
        StubVerifierRunStore,
    };
    use crate::runtime::StubMetaReasonerPort;
    use crate::state::{Message, MessageContent, StopReason, ToolUse};

    // ── Checkpointer in-memory (zero DB nel test) ─────────────────────────────

    /// Persistenza in memoria: registra ogni `put` e ritorna l'ultimo superstep
    /// in `load`. Replica la semantica del `PgCheckpointer` senza DB. Conta le
    /// `put` per asserire che il checkpoint persiste a ogni superstep.
    #[derive(Default)]
    struct MemoryCheckpointer {
        // (run_id, superstep) -> (state-json, next-label)
        store: Mutex<HashMap<(Uuid, i64), (serde_json::Value, String)>>,
    }

    #[async_trait]
    impl Checkpointer<AgentState> for MemoryCheckpointer {
        async fn put(
            &self,
            run_id: Uuid,
            superstep: i64,
            next: NodeId,
            state: &AgentState,
        ) -> Result<(), CheckpointError> {
            let json = serde_json::to_value(state)
                .map_err(|e| CheckpointError::Store(e.to_string()))?;
            self.store
                .lock()
                .expect("lock checkpoint store")
                .insert((run_id, superstep), (json, next.as_label().to_string()));
            Ok(())
        }

        async fn load(
            &self,
            run_id: Uuid,
        ) -> Result<Option<(AgentState, NodeId)>, CheckpointError> {
            let guard = self.store.lock().expect("lock checkpoint store");
            let latest = guard
                .iter()
                .filter(|((rid, _), _)| *rid == run_id)
                .max_by_key(|((_, step), _)| *step);
            match latest {
                None => Ok(None),
                Some((_, (json, label))) => {
                    let state: AgentState = serde_json::from_value(json.clone())
                        .map_err(|e| CheckpointError::Store(e.to_string()))?;
                    let node = NodeId::from_label(label)
                        .ok_or_else(|| CheckpointError::UnknownNode(label.clone()))?;
                    Ok(Some((state, node)))
                }
            }
        }
    }

    impl MemoryCheckpointer {
        fn superstep_count(&self, run_id: Uuid) -> usize {
            self.store
                .lock()
                .expect("lock checkpoint store")
                .keys()
                .filter(|(rid, _)| *rid == run_id)
                .count()
        }
    }

    // ── Gateway LLM scriptato (turni in sequenza) ─────────────────────────────

    /// Gateway LLM che ritorna risposte SCRIPTATE in sequenza: una `LlmResponse`
    /// per chiamata, nell'ordine dato. Esaurita la lista ripete l'ultima (turno
    /// terminale stabile). Cosi' il test pilota il loop executor<->tool_dispatch:
    /// 1a chiamata = tool_use, 2a = end_turn.
    struct ScriptedLlmGateway {
        turns: Vec<LlmResponse>,
        calls: Mutex<usize>,
    }

    impl ScriptedLlmGateway {
        fn new(turns: Vec<LlmResponse>) -> Self {
            Self {
                turns,
                calls: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("lock calls")
        }
    }

    #[async_trait]
    impl LlmGateway for ScriptedLlmGateway {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            let mut n = self.calls.lock().expect("lock calls");
            let idx = (*n).min(self.turns.len().saturating_sub(1));
            *n += 1;
            Ok(self.turns[idx].clone())
        }
    }

    /// Risposta che emette UNA tool call PRODUTTIVA (write_file, mutator del
    /// filesystem): loop -> tool_dispatch -> executor. Produttiva (non
    /// exploration-only) cosi' al turno end_turn il routing NON ricade nel
    /// re-routing G1 (ramo "resoconto finale legittimo"), e il task e' "software"
    /// (mutazione fs) -> attraversa il final_gate prima di chiudere.
    fn turn_tool_use() -> LlmResponse {
        LlmResponse {
            content: String::new(),
            tool_calls: vec![ToolUse {
                id: "tc-1".to_string(),
                name: "write_file".to_string(),
                input: json!({"path": "src/main.rs", "content": "fn main() {}"}),
            }],
            usage: LlmUsage::default(),
            stop_reason: Some("tool_use".to_string()),
            ..Default::default()
        }
    }

    /// Risposta finale testuale (end_turn -> chiusura). Testo senza elenco di
    /// passi pendenti -> `unfulfilled_signal` falso (solo strutturale, ADR 0018).
    fn turn_end() -> LlmResponse {
        LlmResponse {
            content: "Lavoro concluso: ho scritto il file richiesto.".to_string(),
            tool_calls: vec![],
            usage: LlmUsage::default(),
            stop_reason: Some("end_turn".to_string()),
            ..Default::default()
        }
    }

    // ── Costruzione dei nodi con stub ─────────────────────────────────────────

    /// Esecutore tool stub condiviso (Arc cosi' lo usano piu' nodi).
    fn stub_tools() -> Arc<dyn ToolExecutor> {
        Arc::new(StubToolForGraph)
    }

    /// Tool executor che ritorna un esito di successo per qualunque chiamata
    /// (il loop deve solo girare, non verificare il contenuto del tool).
    struct StubToolForGraph;

    #[async_trait]
    impl ToolExecutor for StubToolForGraph {
        async fn execute(
            &self,
            call: ToolCall,
            _mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
            Ok(ToolOutcome {
                tool_call_id: call.id,
                content: json!("contenuto del file letto"),
                is_error: false,
                exit_code: None,
                ..Default::default()
            })
        }
    }

    /// Criteria runner stub: nessun criterio fallisce (final_gate/verifier passano
    /// se mai raggiunti). Lista vuota -> all_passed (vacuamente vero lato nodo).
    fn stub_criteria() -> Arc<dyn CriteriaRunner> {
        Arc::new(StubCriteriaRunner::with_results(Vec::<CriterionResult>::new()))
    }

    /// PgPool LAZY: `connect_lazy` NON apre connessioni finche' non si interroga
    /// il DB. I nodi del percorso testato non lo toccano (reflection e' pass-through
    /// per via del gate <reflection>; learner fa la persistenza in tokio::spawn
    /// fire-and-forget, non bloccante e non atteso dal test). Non e' un fallback
    /// hardcoded di produzione (regola G): serve solo a soddisfare il tipo PgPool.
    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette davvero")
    }

    /// Costruisce gli 11 nodi con gli stub. L'executor risolve provider/model dai
    /// `routing_provider`/`routing_model` della config (qui valorizzati, cosi' non
    /// cade in `NoProvider`).
    fn build_stub_nodes(tools: Arc<dyn ToolExecutor>) -> AgentGraphNodes {
        let run_control: Arc<dyn RunControlStore> = Arc::new(StubRunControlStore::default());
        let steps: Arc<dyn AgentStepStore> = Arc::new(StubAgentStepStore::default());
        let meta_steps: Arc<dyn MetaStepStore> = Arc::new(StubMetaStepStore::default());
        let offload: Arc<dyn ContextOffload> = Arc::new(StubContextOffload::default());
        let todos: Arc<dyn TodoStore> = Arc::new(StubTodoStore::with_todos(vec![]));
        let verifier_runs: Arc<dyn VerifierRunStore> = Arc::new(StubVerifierRunStore::default());
        let escalation: Arc<dyn EscalationPort> = Arc::new(StubEscalationPort::default());
        let next_actions: Arc<dyn NextActionsDeriver> =
            Arc::new(StubNextActionsDeriver::default());
        let billing: Arc<dyn BillingCooldownPort> = Arc::new(StubBillingCooldownPort::default());
        let upscale: Arc<dyn ModelUpscalePort> = Arc::new(StubModelUpscalePort::default());
        let summary_store: Arc<dyn SummaryStore> = Arc::new(StubSummaryStore::default());

        let exec_cfg = ExecutorConfig {
            routing_provider: "stub-provider".to_string(),
            routing_model: "stub-model".to_string(),
            ..ExecutorConfig::default()
        };

        // Meta-reasoner INERTE (Ok(None)) CONDIVISO tra planner (gate orchestrazione)
        // e stall_recovery (recovery): UNA sola istanza (regola L, non duplicata).
        let reasoner: Arc<dyn crate::runtime::ports::MetaReasonerPort> =
            Arc::new(StubMetaReasonerPort);

        AgentGraphNodes {
            router: Arc::new(RouterNode),
            clarify_or_expand: Arc::new(ClarifyOrExpandNode::new(ClarifyConfig::default())),
            understanding: Arc::new(UnderstandingNode::new(UnderstandingConfig::default())),
            planner: Arc::new(PlannerNode::new(
                PlannerConfig::default(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                todos.clone(),
                meta_steps.clone(),
                reasoner.clone(),
            )),
            todo_runner: Arc::new(TodoRunnerNode::new(
                TodoRunnerConfig::default(),
                todos.clone(),
                tools.clone(),
            )),
            executor: Arc::new(ExecutorNode::new(
                exec_cfg,
                run_control.clone(),
                meta_steps.clone(),
                steps.clone(),
                escalation.clone(),
                next_actions.clone(),
                billing.clone(),
                upscale.clone(),
                summary_store.clone(),
            )),
            tool_dispatch: Arc::new(ToolDispatchNode::new(
                ToolDispatchConfig::default(),
                tools.clone(),
                steps.clone(),
                run_control.clone(),
                todos.clone(),
                offload.clone(),
                meta_steps.clone(),
            )),
            // Reasoner INERTE (Ok(None)): il nodo, se raggiunto, degrada alla
            // gerarchia fissa. Non e' mai raggiunto nei test del grafo (nessun
            // detector emette StallReason). STESSA istanza del planner (regola L).
            stall_recovery: Arc::new(StallRecoveryNode::new(reasoner.clone())),
            verifier: Arc::new(VerifierNode::new(
                VerifierConfig::default(),
                FinalGateConfig::default(),
                RoutingConfig::default(),
                todos.clone(),
                stub_criteria(),
                verifier_runs,
                meta_steps.clone(),
            )),
            final_gate: Arc::new(FinalGateNode::new(
                FinalGateConfig::default(),
                RoutingConfig::default(),
                stub_criteria(),
                meta_steps.clone(),
            )),
            reflection: Arc::new(ReflectionNode::new(ReflectionConfig::default())),
            learner: Arc::new(LearnerNode::new(LearnerConfig::default())),
        }
    }

    /// Ctx con il gateway scriptato + stub. `shadow=false`: run primario (Real).
    fn ctx_with(
        llm: Arc<dyn LlmGateway>,
        tools: Arc<dyn ToolExecutor>,
        run_id: Uuid,
    ) -> AgentNodeCtx {
        AgentNodeCtx {
            db: lazy_pool(),
            llm,
            tools,
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id,
            session_id: Uuid::new_v4(),
            thread_id: run_id,
            shadow: false,
        }
    }

    /// Stato iniziale: un messaggio umano + thread_id valorizzato (i nodi lo usano
    /// come run_id). `intent_hint="chat"` -> il router fa PASSTHROUGH deterministico
    /// (niente classificazione LLM non ancora portata): user_intent=chat,
    /// action_oriented=true. Niente <reflection> nel system -> reflection
    /// pass-through (zero I/O LLM nel nodo di chiusura). Niente plan_phase ->
    /// final_gate eleggibile sul task software (write_file e' mutator fs).
    fn initial_state(run_id: Uuid) -> AgentState {
        AgentState {
            messages: vec![Message::Human {
                content: MessageContent::text("Scrivi src/main.rs con uno scheletro."),
            }],
            thread_id: Some(run_id.to_string()),
            intent_hint: Some("chat".to_string()),
            ..Default::default()
        }
    }

    // ── Mapping NodeTarget -> NodeId ──────────────────────────────────────────

    #[test]
    fn mapping_g1continue_e_learner_strutturale() {
        // Le due differenze strutturali volute (regola H): g1_continue -> executor
        // (self-loop nativo), learner -> reflection (graph.py rimappa).
        assert_eq!(
            node_target_to_node_id(NodeTarget::G1Continue),
            NodeId::Executor
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::Learner),
            NodeId::Reflection
        );
        // Gli altri sono 1:1.
        assert_eq!(
            node_target_to_node_id(NodeTarget::ToolDispatch),
            NodeId::ToolDispatch
        );
        assert_eq!(node_target_to_node_id(NodeTarget::Verifier), NodeId::Verifier);
        assert_eq!(
            node_target_to_node_id(NodeTarget::FinalGate),
            NodeId::FinalGate
        );
        assert_eq!(node_target_to_node_id(NodeTarget::Executor), NodeId::Executor);
        assert_eq!(
            node_target_to_node_id(NodeTarget::TodoRunner),
            NodeId::TodoRunner
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::StallRecovery),
            NodeId::StallRecovery
        );
    }

    // ── Topologia: copertura edge ─────────────────────────────────────────────

    #[test]
    fn topologia_copre_ogni_nodo_non_terminale() {
        let edges = build_edges(RoutingConfig::default(), PlannerConfig::default());
        // Ogni nodo non terminale ha un edge uscente dichiarato.
        for id in [
            NodeId::Router,
            NodeId::ClarifyOrExpand,
            NodeId::Understanding,
            NodeId::Planner,
            NodeId::TodoRunner,
            NodeId::Executor,
            NodeId::ToolDispatch,
            NodeId::StallRecovery,
            NodeId::Verifier,
            NodeId::FinalGate,
            NodeId::Reflection,
            NodeId::Learner,
        ] {
            assert!(edges.contains_key(&id), "edge mancante per {id:?}");
        }
        // Edge fissi attesi (graph.py).
        assert!(matches!(
            edges.get(&NodeId::Router),
            Some(Edge::Static(NodeId::ClarifyOrExpand))
        ));
        assert!(matches!(
            edges.get(&NodeId::ToolDispatch),
            Some(Edge::Static(NodeId::Executor))
        ));
        // stall_recovery -> executor (self-loop di rientro dopo il superstep).
        assert!(matches!(
            edges.get(&NodeId::StallRecovery),
            Some(Edge::Static(NodeId::Executor))
        ));
        assert!(matches!(
            edges.get(&NodeId::Reflection),
            Some(Edge::Static(NodeId::Learner))
        ));
        assert!(matches!(edges.get(&NodeId::Learner), Some(Edge::End)));
    }

    // ── Run end-to-end con MOCK ───────────────────────────────────────────────

    #[tokio::test]
    async fn run_completo_attraversa_il_loop_e_chiude() {
        // Gateway scriptato: 1o turno tool_use, 2o turno end_turn. Teniamo l'Arc
        // CONCRETO per leggere il conteggio chiamate (oltre al trait object).
        let gateway = Arc::new(ScriptedLlmGateway::new(vec![turn_tool_use(), turn_end()]));
        let llm: Arc<dyn LlmGateway> = gateway.clone();
        let tools = stub_tools();
        let run_id = Uuid::new_v4();

        let nodes = build_stub_nodes(tools.clone());
        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let engine = build_agent_graph(
            nodes,
            RoutingConfig::default(),
            PlannerConfig::default(),
            checkpointer.clone(),
        );

        let ctx = ctx_with(llm, tools, run_id);
        let outcome = engine
            .run_until_interrupt(run_id, Some(initial_state(run_id)), &ctx)
            .await
            .expect("il run end-to-end deve completare senza errore");

        // Il run chiude attraversando: router -> clarify_or_expand -> understanding
        // -> executor(tool_use write_file) -> tool_dispatch -> executor(end_turn)
        // -> final_gate(passed, task software) -> reflection -> learner -> END.
        let state = match outcome {
            StepOutcome::Completed(s) => s,
            other => panic!("atteso Completed, ottenuto {other:?}"),
        };

        // Il loop executor<->tool_dispatch ha girato: il gateway e' stato chiamato
        // due volte (tool_use poi end_turn).
        assert_eq!(
            gateway.call_count(),
            2,
            "executor deve aver chiamato l'LLM due volte"
        );

        // Lo stato finale e' coerente: stop_reason end_turn, result valorizzato dal
        // 2o turno, niente pending residuo.
        assert_eq!(state.stop_reason, Some(StopReason::EndTurn));
        assert!(
            state
                .result
                .as_deref()
                .unwrap_or("")
                .contains("Lavoro concluso"),
            "il result finale deve venire dal turno end_turn, era {:?}",
            state.result
        );
        assert!(
            state.pending_tool_uses.as_ref().map(|p| p.is_empty()).unwrap_or(true),
            "nessun tool pending residuo a fine run"
        );

        // Il checkpoint ha persistito ogni superstep (almeno: router, clarify,
        // understanding, executor, tool_dispatch, executor, reflection, learner).
        assert!(
            checkpointer.superstep_count(run_id) >= 8,
            "il checkpoint deve persistere uno snapshot per superstep, trovati {}",
            checkpointer.superstep_count(run_id)
        );

        // RESUME da checkpoint: init=None deve trovare l'ultimo checkpoint (next=end)
        // e chiudere subito con Completed (il checkpoint persiste/ripristina).
        let resumed = engine
            .run_until_interrupt(run_id, None, &ctx)
            .await
            .expect("il resume deve trovare il checkpoint");
        assert!(
            matches!(resumed, StepOutcome::Completed(_)),
            "resume da checkpoint a fine run -> Completed immediato"
        );
    }

    // ── Scenario clarify: pending_clarify e' TERMINALE, non interrupt ─────────

    /// Stub del nodo `clarify_or_expand` che emette `pending_clarify=true` (ramo
    /// `ask` del nodo reale, vedi `ClarifyOrExpandNode::build_ask_delta`). Isola la
    /// TOPOLOGIA sotto test (edge condizionale -> End) dal pilotaggio LLM/routing
    /// del nodo reale, che richiederebbe confidence bassa + mode=ask scriptati.
    struct ClarifyEmitsPendingNode;

    #[async_trait]
    impl GraphNode<AgentState, AgentNodeCtx> for ClarifyEmitsPendingNode {
        fn id(&self) -> NodeId {
            NodeId::ClarifyOrExpand
        }

        async fn run(
            &self,
            _state: &AgentState,
            _ctx: &AgentNodeCtx,
        ) -> Result<nexus_graph::StateDelta, nexus_graph::node::NodeError> {
            let delta = crate::state::StateDelta {
                pending_clarify: Some(Some(true)),
                clarify_attempts: Some(Some(1)),
                ..Default::default()
            };
            Ok(delta.into_opaque())
        }
    }

    /// Stub del nodo `understanding` che, se eseguito, MARCA lo stato (steps=999):
    /// serve a PROVARE che il run con pending_clarify NON attraversa understanding.
    struct UnderstandingTripwireNode;

    #[async_trait]
    impl GraphNode<AgentState, AgentNodeCtx> for UnderstandingTripwireNode {
        fn id(&self) -> NodeId {
            NodeId::Understanding
        }

        async fn run(
            &self,
            _state: &AgentState,
            _ctx: &AgentNodeCtx,
        ) -> Result<nexus_graph::StateDelta, nexus_graph::node::NodeError> {
            // Scrive un marcatore nello schema aperto: se understanding gira, il
            // test lo rileva. NON deve mai accadere su pending_clarify.
            let mut marker = serde_json::Map::new();
            marker.insert("understanding_visited".to_string(), json!(true));
            let delta = crate::state::StateDelta {
                extra: Some(marker),
                ..Default::default()
            };
            Ok(delta.into_opaque())
        }
    }

    #[tokio::test]
    async fn clarify_pending_e_terminale_non_interrupt() {
        // Topologia REALE (build_agent_graph) con il nodo clarify sostituito da uno
        // stub che emette pending_clarify=true (ramo ask) e understanding sostituito
        // da una tripwire. Atteso: router -> clarify_or_expand(pending) -> END.
        // Il run deve risultare COMPLETED (terminale, come graph.py), NON
        // Interrupted{resume_at: understanding}, e NON deve attraversare understanding.
        let tools = stub_tools();
        let run_id = Uuid::new_v4();

        let mut nodes = build_stub_nodes(tools.clone());
        // Sostituisce clarify e understanding con gli stub dello scenario.
        nodes.clarify_or_expand = Arc::new(ClarifyEmitsPendingNode);
        nodes.understanding = Arc::new(UnderstandingTripwireNode);

        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let engine = build_agent_graph(
            nodes,
            RoutingConfig::default(),
            PlannerConfig::default(),
            checkpointer.clone(),
        );

        // L'LLM non viene mai chiamato in questo scenario (clarify e' uno stub e il
        // run chiude prima dell'executor): gateway minimale.
        let llm: Arc<dyn LlmGateway> = Arc::new(ScriptedLlmGateway::new(vec![turn_end()]));
        let ctx = ctx_with(llm, tools, run_id);

        let outcome = engine
            .run_until_interrupt(run_id, Some(initial_state(run_id)), &ctx)
            .await
            .expect("lo scenario clarify deve concludersi senza errore");

        // Il run e' TERMINALE: Completed, NON Interrupted (la divergenza fixata).
        let state = match outcome {
            StepOutcome::Completed(s) => s,
            StepOutcome::Interrupted { resume_at, .. } => panic!(
                "pending_clarify deve CHIUDERE il run (Completed), non sospenderlo \
                 (Interrupted resume_at={resume_at:?})"
            ),
        };

        // pending_clarify e' valorizzato (il turno si e' fermato in attesa utente).
        assert_eq!(
            state.pending_clarify,
            Some(true),
            "lo stato finale deve riportare pending_clarify=true"
        );

        // PROVA che understanding NON e' stato attraversato (tripwire non scattata).
        assert!(
            !state
                .extra
                .get("understanding_visited")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "il run con pending_clarify NON deve attraversare understanding"
        );

        // RESUME con init=None: l'ultimo checkpoint punta a End (run gia' concluso)
        // -> Completed immediato. NON riprende dallo STESSO run a understanding: il
        // prossimo messaggio utente avviera' un NUOVO run dall'entry router (fuori
        // dallo scope di questo motore, che riconosce solo che il run e' chiuso).
        let resumed = engine
            .run_until_interrupt(run_id, None, &ctx)
            .await
            .expect("il resume deve trovare il checkpoint terminale");
        assert!(
            matches!(resumed, StepOutcome::Completed(_)),
            "resume di un run clarify chiuso -> Completed immediato (non riparte da understanding)"
        );
    }

    // ── recursion_limit: un grafo che ciclerebbe all'infinito si ferma ────────

    /// Nodo che instrada sempre a se' stesso (self-loop puro) SENZA produrre lo
    /// stato che farebbe scattare gli anti-loop dei nodi concreti: isola il cap
    /// del MOTORE (GraphEngine) dagli anti-loop applicativi dell'executor.
    struct SelfLoopNode;

    #[async_trait]
    impl GraphNode<AgentState, AgentNodeCtx> for SelfLoopNode {
        fn id(&self) -> NodeId {
            NodeId::Router
        }

        async fn run(
            &self,
            _state: &AgentState,
            _ctx: &AgentNodeCtx,
        ) -> Result<nexus_graph::StateDelta, nexus_graph::node::NodeError> {
            // Delta vuoto: lo stato non cambia, il routing torna sempre qui.
            Ok(crate::state::StateDelta::default().into_opaque())
        }
    }

    #[tokio::test]
    async fn loop_infinito_si_ferma_al_recursion_limit() {
        // Mini-grafo che cicla all'infinito: Router(self-loop) -> Router -> ...
        // Il motore (istanziato sui tipi Nexus AgentState/AgentNodeCtx, gli stessi
        // del cablaggio reale) deve fermarsi al recursion_limit invece di girare
        // per sempre. Si usa GraphEngine::new direttamente perche' il grafo Nexus
        // completo CONVERGE prima (anti-loop dei nodi): qui isoliamo il cap del
        // MOTORE, che e' la rete di sicurezza ultima contro un grafo non
        // convergente.
        let mut node_map: HashMap<NodeId, Arc<AgentGraphNode>> = HashMap::new();
        node_map.insert(NodeId::Router, Arc::new(SelfLoopNode));
        let mut edges: HashMap<NodeId, Edge<AgentState>> = HashMap::new();
        edges.insert(NodeId::Router, Edge::Static(NodeId::Router));

        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let engine: AgentGraphEngine =
            GraphEngine::new(node_map, edges, NodeId::Router, checkpointer);

        let run_id = Uuid::new_v4();
        // recursion_limit basso (12) via la RoutingConfig del ctx: il motore lo
        // legge da AgentNodeCtx::recursion_limit() -> cfg.recursion_limit.
        let routing_cfg = RoutingConfig {
            recursion_limit: 12,
            ..RoutingConfig::default()
        };
        let tools = stub_tools();
        let llm: Arc<dyn LlmGateway> = Arc::new(ScriptedLlmGateway::new(vec![turn_end()]));
        let mut ctx = ctx_with(llm, tools, run_id);
        ctx.cfg = routing_cfg;

        let err = engine
            .run_until_interrupt(run_id, Some(AgentState::default()), &ctx)
            .await
            .expect_err("un grafo che cicla deve superare il recursion_limit");

        match err {
            GraphError::RecursionLimit(limit) => assert_eq!(limit, 12),
            other => panic!("atteso RecursionLimit, ottenuto {other:?}"),
        }
    }
}
