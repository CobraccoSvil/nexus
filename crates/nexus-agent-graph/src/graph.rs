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
//!   tool_dispatch -> executor | learner (loop agentico o veto panel terminale)
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
//! la decisione), che decide su `behavior_mode`, `user_intent` e `token_budget`.
//! Non esiste una variante che pesi anche i segnali fini del classifier
//! (complexity, agentic_score): quei segnali arrivano nello stato ma questo gate
//! non li legge.

use std::collections::HashMap;
use std::sync::Arc;

use nexus_graph::edge::Edge;
use nexus_graph::engine::GraphEngine;
use nexus_graph::node::{GraphNode, NodeId};

use crate::decisions::supervisor::{detect_anomalies, should_invoke, SupervisorConfig};
use crate::nodes::supervisor::SUPERVISOR_ABANDON_KEY;
use crate::nodes::PlannerConfig;
use crate::routing::{
    self, route_after_executor, route_after_final_gate, route_after_planner,
    route_after_todo_runner, route_after_verifier, NodeTarget, RoutingConfig,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, SupervisorMode};

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
    /// l'executor). E' raggiunto quando l'executor emette `StallReason` (gate
    /// `agent.stall_recovery.enabled`); se la porta `MetaReasonerPort` ritorna
    /// `Ok(None)` il superstep gira a vuoto e rientra senza mossa.
    pub stall_recovery: Arc<AgentGraphNode>,
    /// Scale-controller (superstep dedicato; self-loop verso l'executor). Gemello di
    /// `stall_recovery`. E' raggiunto quando l'executor emette `ScaleReason` (gate
    /// `agent.scale.enabled`): senza quel segnale il grafo si comporta come se il
    /// nodo non ci fosse.
    pub scale_control: Arc<AgentGraphNode>,
    /// Supervisore worker (monitoraggio periodico post tool_dispatch).
    pub supervisor: Arc<AgentGraphNode>,
    /// Verifica plan-phase (DoD).
    pub verifier: Arc<AgentGraphNode>,
    /// Verifica E2E pre-chiusura.
    pub final_gate: Arc<AgentGraphNode>,
    /// Review adversariale come gate di chiusura (rimando in correzione su
    /// bocciatura, gemello del final_gate).
    pub review_gate: Arc<AgentGraphNode>,
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
        NodeTarget::ScaleControl => NodeId::ScaleControl,
        // Self-loop G1 nativo nel motore (no nodo passthrough, regola H).
        NodeTarget::G1Continue => NodeId::Executor,
        // graph.py: il target "learner" delle route_after_* chiudeva su
        // reflection; ora la chiusura ONESTA passa PRIMA dal ReviewGate
        // (review_gate -> reflection -> learner -> END), che su bocciatura
        // rimanda in correzione invece di lasciar chiudere un lavoro bocciato.
        NodeTarget::Learner => NodeId::ReviewGate,
    }
}

/// Costruisce la mappa degli `Edge` uscenti da ogni nodo (la topologia).
///
/// Riceve la `RoutingConfig` (catturata dalle closure condizionali) e la
/// `PlannerConfig` (per l'eligibilita' planner dell'edge understanding).
/// Entrambe sono clonate nelle closure (`'static`), come nel runtime reale dove
/// vengono risolte a monte (regola G).
pub(crate) fn build_edges(
    routing_cfg: RoutingConfig,
    planner_cfg: PlannerConfig,
    supervisor_cfg: SupervisorConfig,
) -> HashMap<NodeId, Edge<AgentState>> {
    let mut edges: HashMap<NodeId, Edge<AgentState>> = HashMap::new();
    let sup_cfg = supervisor_cfg;

    // ── Edge fissi (graph.py:164, 237, 246, 247) ─────────────────────────────
    // router -> clarify_or_expand (sempre).
    edges.insert(NodeId::Router, Edge::Static(NodeId::ClarifyOrExpand));
    // tool_dispatch -> supervisor | executor | learner (veto panel terminale).
    edges.insert(
        NodeId::ToolDispatch,
        Edge::conditional(move |state: &AgentState| {
            let terminal_panel_veto = state
                .extra
                .get(crate::nodes::PANEL_ENFORCEMENT_KEY)
                .and_then(|v| v.get("terminal"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if terminal_panel_veto {
                return NodeId::Learner;
            }
            let mode = state.supervisor_mode.unwrap_or(SupervisorMode::None);
            if mode != SupervisorMode::None {
                let iterations = state.iterations.unwrap_or(0);
                let anomalies = detect_anomalies(state, sup_cfg);
                if should_invoke(mode, iterations, sup_cfg, &anomalies) {
                    return NodeId::Supervisor;
                }
            }
            // ADR 0034: task_complete e' azione TERMINALE. Se il batch appena
            // dispatchato ha dichiarato un esito onesto terminale (done/blocked/
            // needs_input), il grafo va alla VERIFICA finale e NON rientra
            // nell'executor: rientrare permetterebbe al modello di emettere altri
            // tool e AUTO-INVALIDARE la dichiarazione (tool_dispatch.rs stale-
            // invalidation), come nel run cc01d06d (task_complete(done) valido
            // ignorato dal routing -> loop -> failed_diagnosed). `partial` resta
            // sull'executor (dichiarazione onesta di lavoro incompleto: prosegue).
            // Punto unico (regola L): declared_outcome_kind, nessuna nuova logica.
            if matches!(
                crate::routing::declared_outcome_kind(state).as_deref(),
                Some("done") | Some("blocked") | Some("needs_input")
            ) {
                return NodeId::FinalGate;
            }
            // Stessa regola per le FIGURE, il cui deliverable non e' un task
            // completato ma un giudizio (review_verdict / advisory_verdict /
            // debate_position): emesso quello, la figura ha finito cio' che le e'
            // chiesto. Senza questo ramo ricadeva sull'executor e girava a vuoto
            // fino al wall-clock, dove il verdetto veniva scartato e sostituito da
            // "[Sub-agent timeout]". Punto unico: declared_role_channel. Il
            // final_gate a sua volta NON applica i criteri d'ambiente alla figura
            // (stesso punto unico, guard nel nodo): la sua e' solo la strada di
            // chiusura, non una verifica del codice sotto giudizio.
            if crate::routing::declared_role_channel(state).is_some() {
                return NodeId::FinalGate;
            }
            NodeId::Executor
        }),
    );
    // supervisor -> executor (continue/redirect) | learner (abandon).
    edges.insert(
        NodeId::Supervisor,
        Edge::conditional(|state: &AgentState| {
            let abandon = state
                .extra
                .get(SUPERVISOR_ABANDON_KEY)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || matches!(
                    state.stop_reason,
                    Some(crate::state::StopReason::SupervisorAbandon)
                );
            if abandon {
                NodeId::Learner
            } else {
                NodeId::Executor
            }
        }),
    );
    // stall_recovery -> executor (rientro nel loop agentico dopo il superstep di
    // recovery). Il nodo emette sempre StopReason::StallResolved e torna
    // nell'executor, che consuma la RecoveryMove eventualmente persistita in extra
    // (self-loop, analogo a `G1Escalated -> executor`). Il nodo e' raggiunto
    // quando l'executor emette StallReason (gate `agent.stall_recovery.enabled`).
    edges.insert(NodeId::StallRecovery, Edge::Static(NodeId::Executor));
    // scale_control -> executor (self-loop di rientro dopo il superstep di scala,
    // gemello di stall_recovery). Il nodo emette sempre StopReason::ScaleResolved e
    // torna nell'executor, che consuma la ScaleMove eventualmente persistita in extra
    // (rientro-applicazione). Il nodo e' raggiunto quando l'executor emette
    // ScaleReason (gate `agent.scale.enabled`).
    edges.insert(NodeId::ScaleControl, Edge::Static(NodeId::Executor));
    // review_gate -> executor (bocciatura rimandata in correzione, stesso
    // predicato del final_gate: punto unico gate_rimanda_in_correzione) oppure
    // -> reflection (chiusura). ATTENZIONE: il ramo di chiusura punta a
    // NodeId::Reflection ESPLICITO, mai via node_target_to_node_id(Learner) --
    // quella mappatura ora porta QUI e creerebbe un self-loop infinito.
    edges.insert(
        NodeId::ReviewGate,
        Edge::conditional(|state: &AgentState| {
            if crate::routing::gate_rimanda_in_correzione(state) {
                NodeId::Executor
            } else {
                NodeId::Reflection
            }
        }),
    );
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
    // Delega al punto unico PlannerConfig::is_eligible (regola L). I QUATTRO segnali
    // (behavior_mode, intent, token_budget, prodotto_del_run) sono letti dallo stato
    // post-router. Il quarto non parla del compito ma del run: una figura convocata
    // per dare un parere non entra nella plan-phase, perche' delegare i passi a
    // sub-run che scrivono e' produrre il lavoro per interposta figura (vedi
    // `decisions::prodotto_del_run`, misura del 10/08/2026).
    edges.insert(
        NodeId::Understanding,
        Edge::conditional(move |state: &AgentState| {
            let eligible = planner_cfg.is_eligible(
                state.behavior_mode.as_deref(),
                state.user_intent.as_deref(),
                state.token_budget.unwrap_or(0),
                state.prodotto_del_run.unwrap_or_default(),
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
fn build_node_map(nodes: AgentGraphNodes) -> HashMap<NodeId, Arc<AgentGraphNode>> {
    let mut map: HashMap<NodeId, Arc<AgentGraphNode>> = HashMap::new();
    map.insert(NodeId::Router, nodes.router);
    map.insert(NodeId::ClarifyOrExpand, nodes.clarify_or_expand);
    map.insert(NodeId::Understanding, nodes.understanding);
    map.insert(NodeId::Planner, nodes.planner);
    map.insert(NodeId::TodoRunner, nodes.todo_runner);
    map.insert(NodeId::Executor, nodes.executor);
    map.insert(NodeId::ToolDispatch, nodes.tool_dispatch);
    map.insert(NodeId::StallRecovery, nodes.stall_recovery);
    map.insert(NodeId::ScaleControl, nodes.scale_control);
    map.insert(NodeId::Supervisor, nodes.supervisor);
    map.insert(NodeId::Verifier, nodes.verifier);
    map.insert(NodeId::FinalGate, nodes.final_gate);
    map.insert(NodeId::ReviewGate, nodes.review_gate);
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
    supervisor_cfg: SupervisorConfig,
    checkpointer: Arc<dyn nexus_graph::checkpoint::Checkpointer<AgentState>>,
) -> AgentGraphEngine {
    let node_map = build_node_map(nodes);
    let edges = build_edges(routing_cfg, planner_cfg, supervisor_cfg);
    GraphEngine::new(node_map, edges, NodeId::Router, checkpointer)
}

/// Riferimento al modulo di routing per i doc-link (`routing::NodeTarget`).
#[allow(unused_imports)]
use routing as _routing_doc_anchor;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
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
        FinalGateNode, LearnerNode, PlannerNode, ReflectionConfig, ReflectionNode,
        RouterNode, ScaleControlNode, StallRecoveryNode, SupervisorNode, TodoRunnerConfig,
        TodoRunnerNode, ToolDispatchConfig, ToolDispatchNode, UnderstandingConfig,
        UnderstandingNode, VerifierConfig, VerifierNode,
    };
    use crate::runtime::ports::{
        AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, CriterionResult,
        EscalationPort, LlmGateway, LlmRequest, LlmResponse, LlmUsage, MetaStepStore,
        ModelUpscalePort, NextActionsDeriver, PortError, RunControlStore, SummaryStore, TodoStore,
        ToolCall, ToolExecutor, ToolOutcome, VerifierRunStore,
    };
    use crate::runtime::test_doubles::{
        NullEventSink, StubAgentStepStore, StubBillingCooldownPort, StubContextOffload,
        StubCriteriaRunner, StubEscalationPort, StubMetaStepStore, StubModelUpscalePort,
        StubNextActionsDeriver, StubRunControlStore, StubSummaryStore, StubTodoStore,
        StubVerifierRunStore,
    };
    use crate::runtime::{StubMetaReasonerPort, StubReviewPanelPort};
    use crate::state::{AutomationMode, Message, MessageContent, StateDelta, StopReason, ToolUse};

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
            let json =
                serde_json::to_value(state).map_err(|e| CheckpointError::Store(e.to_string()))?;
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
        /// System prompt di OGNI richiesta, nell'ordine in cui e' stata fatta:
        /// e' cio' che il modello legge davvero, l'unico modo per verificare le
        /// iniezioni nel system senza rifarne il calcolo nel test (regola O).
        systems: Mutex<Vec<Option<String>>>,
    }

    impl ScriptedLlmGateway {
        fn new(turns: Vec<LlmResponse>) -> Self {
            Self {
                turns,
                calls: Mutex::new(0),
                systems: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("lock calls")
        }

        fn systems(&self) -> Vec<Option<String>> {
            self.systems.lock().expect("lock systems").clone()
        }
    }

    #[async_trait]
    impl LlmGateway for ScriptedLlmGateway {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.systems
                .lock()
                .expect("lock systems")
                .push(req.system_text.clone());
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
                thought_signature: None,
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

    /// Tool stub che conta le esecuzioni (per verificare HITL: zero prima, uno dopo).
    struct TrackingStubTool {
        exec_count: AtomicUsize,
    }

    #[async_trait]
    impl ToolExecutor for TrackingStubTool {
        async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
            self.exec_count.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutcome {
                tool_call_id: call.id,
                content: json!("ok"),
                is_error: false,
                exit_code: None,
                ..Default::default()
            })
        }
    }

    /// Delta di resume HITL (parita' `build_resume_delta` in mcp-core native_engine).
    fn hitl_resume_delta() -> nexus_graph::StateDelta {
        StateDelta {
            awaiting_confirmation: Some(Some(false)),
            approved: Some(Some(true)),
            messages: Some(vec![Message::Human {
                content: MessageContent::text(
                    "Azioni confermate dall'utente. Esegui le operazioni approvate.",
                ),
            }]),
            ..Default::default()
        }
        .into_opaque()
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
        async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
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
        Arc::new(StubCriteriaRunner::with_results(
            Vec::<CriterionResult>::new(),
        ))
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
        let next_actions: Arc<dyn NextActionsDeriver> = Arc::new(StubNextActionsDeriver::default());
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
            clarify_or_expand: Arc::new(ClarifyOrExpandNode::new(
                ClarifyConfig::default(),
                Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default()),
            )),
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
                run_control.clone(),
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
            // Scale-controller INERTE (STESSA istanza reasoner, regola L): con
            // Ok(None) e nessun detector che emette ScaleReason il nodo non e' mai
            // raggiunto nei test del grafo (bit-identico).
            scale_control: Arc::new(ScaleControlNode::new(reasoner.clone())),
            supervisor: Arc::new(SupervisorNode::new(
                reasoner.clone(),
                SupervisorConfig::default(),
            )),
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
            review_gate: Arc::new(crate::nodes::review_gate::ReviewGateNode::new(
                crate::nodes::review_gate::ReviewGateConfig {
                    enabled: false, // fixture: gate inerte nei test topologici
                    max_cycles: 1,
                },
                Arc::new(StubReviewPanelPort),
                meta_steps.clone(),
            )),
            reflection: Arc::new(ReflectionNode::new(ReflectionConfig::default())),
            learner: Arc::new(LearnerNode::new()),
        }
    }

    /// Ctx con il gateway scriptato + stub.
    fn ctx_with(
        llm: Arc<dyn LlmGateway>,
        tools: Arc<dyn ToolExecutor>,
        run_id: Uuid,
    ) -> AgentNodeCtx {
        AgentNodeCtx {
            isolation_available: false,
            db: lazy_pool(),
            llm,
            tools,
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id,
            session_id: Uuid::new_v4(),
            thread_id: run_id,
            advisory_gate: None,
        step_gate: None,
        }
    }

    /// Stato iniziale: un messaggio umano + thread_id valorizzato (i nodi lo usano
    /// come run_id). `intent_hint="chat"` -> il router fa PASSTHROUGH deterministico
    /// (niente classificazione LLM non ancora portata): user_intent=chat,
    /// action_oriented=true. Niente <reflection> nel system -> reflection
    /// pass-through (zero I/O LLM nel nodo di chiusura). Niente plan_phase ->
    /// final_gate eleggibile sul task software (write_file e' mutator fs).
    fn initial_state(run_id: Uuid) -> AgentState {
        // `extra[ORIGINAL_TASK_KEY]` come lo fissa `native_engine::build_initial_state`
        // su OGNI run: e' da li' che supervisore e focus del turno leggono la
        // richiesta (punto unico `decisions::turn_task`). Ometterlo qui darebbe
        // un motore che nei test non ha il dato che in produzione ha sempre.
        let mut extra = serde_json::Map::new();
        extra.insert(
            crate::decisions::turn_task::ORIGINAL_TASK_KEY.to_string(),
            json!(RICHIESTA_DEL_TURNO),
        );
        AgentState {
            messages: vec![Message::Human {
                content: MessageContent::text(RICHIESTA_DEL_TURNO),
            }],
            thread_id: Some(run_id.to_string()),
            intent_hint: Some("chat".to_string()),
            automation_mode: Some(AutomationMode::Automatic),
            extra,
            ..Default::default()
        }
    }

    /// La richiesta dell'utente del turno: una sola definizione, cosi' i test che
    /// la cercano nel prompt non ne ricopiano una variante.
    const RICHIESTA_DEL_TURNO: &str = "Scrivi src/main.rs con uno scheletro.";

    // ── Mapping NodeTarget -> NodeId ──────────────────────────────────────────

    #[test]
    fn mapping_g1continue_e_learner_strutturale() {
        // Le differenze strutturali volute (regola H): g1_continue -> executor
        // (self-loop nativo); learner -> REVIEW_GATE (la chiusura onesta passa
        // dalla review adversariale prima di reflection: su bocciatura il gate
        // rimanda in correzione invece di lasciar chiudere un lavoro bocciato).
        assert_eq!(
            node_target_to_node_id(NodeTarget::G1Continue),
            NodeId::Executor
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::Learner),
            NodeId::ReviewGate
        );
        // Gli altri sono 1:1.
        assert_eq!(
            node_target_to_node_id(NodeTarget::ToolDispatch),
            NodeId::ToolDispatch
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::Verifier),
            NodeId::Verifier
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::FinalGate),
            NodeId::FinalGate
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::Executor),
            NodeId::Executor
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::TodoRunner),
            NodeId::TodoRunner
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::StallRecovery),
            NodeId::StallRecovery
        );
        assert_eq!(
            node_target_to_node_id(NodeTarget::ScaleControl),
            NodeId::ScaleControl
        );
    }

    // ── Topologia: copertura edge ─────────────────────────────────────────────

    #[test]
    fn task_complete_terminale_va_a_final_gate_non_executor() {
        // REGRESSIONE run cc01d06d: un task_complete(done) VALIDO deve portare al
        // FINAL_GATE (verifica oggettiva), NON rientrare nell'executor -- dove il
        // modello continuerebbe e auto-invaliderebbe la dichiarazione, ciclando fino
        // a failed_diagnosed. Deterministico, nessun LLM.
        let edges = build_edges(
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
        );
        let edge = edges
            .get(&NodeId::ToolDispatch)
            .expect("edge tool_dispatch presente");

        for outcome in ["done", "blocked", "needs_input"] {
            let s = AgentState {
                declared_outcome: Some(serde_json::json!({ "outcome": outcome, "summary": "x" })),
                ..Default::default()
            };
            assert_eq!(
                edge.resolve(&s),
                NodeId::FinalGate,
                "esito terminale '{outcome}' deve andare al final_gate, non all'executor"
            );
        }

        // `partial` = lavoro incompleto onesto -> prosegue sull'executor.
        let partial = AgentState {
            declared_outcome: Some(serde_json::json!({ "outcome": "partial" })),
            ..Default::default()
        };
        assert_eq!(edge.resolve(&partial), NodeId::Executor);

        // Nessuna dichiarazione -> executor come oggi (nessuna regressione).
        assert_eq!(edge.resolve(&AgentState::default()), NodeId::Executor);
    }

    /// REGRESSIONE (il difetto chiesto due volte: "se non superata dovrebbe
    /// tentare di sistemare"): la chiusura onesta passa dal ReviewGate, e il suo
    /// edge rimanda all'Executor su bocciatura o prosegue su Reflection.
    ///
    /// TRAPPOLA LETALE coperta: la mappatura `NodeTarget::Learner` ora punta a
    /// ReviewGate; se l'edge del ReviewGate usasse `node_target_to_node_id`
    /// (invece di Reflection ESPLICITO) il nodo ricircolerebbe su se stesso
    /// all'infinito. Il test asserisce entrambe le direzioni sul GRAFO REALE.
    ///
    /// Lo stato porta SOLO `gate_routing`, il campo che il gate dichiara e che
    /// l'edge legge. Prima portava `stop_reason: ToolUse` con la didascalia "dal
    /// delta del nodo": non veniva da nessun nodo, era un letterale, e fissava
    /// l'assunto che rendeva il difetto invisibile — quel valore lo scrive
    /// l'executor a ogni turno, quindi il test restava verde mentre in
    /// produzione l'edge rispediva all'executor anche le review APPROVATE. La
    /// verifica che attraversa il produttore (nodo reale -> delta -> edge) e'
    /// `approvazione_chiude_e_non_riconvoca_i_revisori` in `nodes::review_gate`.
    #[test]
    fn review_gate_rimanda_o_chiude_senza_ricircolare() {
        use crate::state::GateRouting;
        let edges = build_edges(
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
        );
        // La chiusura onesta (target Learner) entra nel ReviewGate.
        assert_eq!(
            node_target_to_node_id(crate::routing::NodeTarget::Learner),
            NodeId::ReviewGate,
            "la chiusura deve passare dal gate della review"
        );
        let edge = edges.get(&NodeId::ReviewGate).expect("edge review_gate");
        // Bocciatura rimandata: il gate lo DICHIARA -> Executor.
        let rimandato = AgentState {
            gate_routing: Some(GateRouting::RimandaInCorrezione),
            ..Default::default()
        };
        assert_eq!(edge.resolve(&rimandato), NodeId::Executor);
        // Chiusura (approvato/non applicabile/cap) -> Reflection, MAI ReviewGate.
        for dichiarazione in [Some(GateRouting::Chiude), None] {
            let chiude = AgentState {
                gate_routing: dichiarazione,
                // Il turno dell'executor che precede il gate lascia sempre
                // questi due: nessuno dei due deve poter dirottare l'edge.
                stop_reason: Some(crate::state::StopReason::ToolUse),
                pending_tool_uses: Some(vec![serde_json::json!({
                    "type": "tool_use", "id": "t1", "name": "read_file", "input": {}
                })]),
                ..Default::default()
            };
            let next = edge.resolve(&chiude);
            assert_eq!(next, NodeId::Reflection, "chiusura su Reflection");
            assert_ne!(next, NodeId::ReviewGate, "mai un self-loop del gate");
        }
    }

    #[test]
    fn topologia_copre_ogni_nodo_non_terminale() {
        let edges = build_edges(
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
        );
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
            NodeId::ScaleControl,
            NodeId::Supervisor,
            NodeId::Verifier,
            NodeId::FinalGate,
            NodeId::ReviewGate,
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
        let tool_dispatch_edge = edges
            .get(&NodeId::ToolDispatch)
            .expect("edge tool_dispatch presente");
        assert_eq!(
            tool_dispatch_edge.resolve(&AgentState::default()),
            NodeId::Executor
        );
        let mut veto_state = AgentState::default();
        veto_state.extra.insert(
            crate::nodes::PANEL_ENFORCEMENT_KEY.to_string(),
            serde_json::json!({"terminal": true}),
        );
        assert_eq!(tool_dispatch_edge.resolve(&veto_state), NodeId::Learner);
        // stall_recovery -> executor (self-loop di rientro dopo il superstep).
        assert!(matches!(
            edges.get(&NodeId::StallRecovery),
            Some(Edge::Static(NodeId::Executor))
        ));
        // scale_control -> executor (self-loop di rientro, gemello di stall_recovery).
        assert!(matches!(
            edges.get(&NodeId::ScaleControl),
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
            SupervisorConfig::default(),
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
            state
                .pending_tool_uses
                .as_ref()
                .map(|p| p.is_empty())
                .unwrap_or(true),
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

    // ── Testa del prompt: stabilita' fra turni consecutivi ────────────────────

    /// Gateway che REGISTRA la richiesta di ogni turno, oltre a scriptare le
    /// risposte come [`ScriptedLlmGateway`].
    ///
    /// Perche' esiste: la testa del prompt (system + catalogo tool) e' il prefisso
    /// su cui il fornitore fa cache, e la sua stabilita' fra turni consecutivi non
    /// e' osservabile da nessun'altra parte — l'executor ricompone il system a
    /// ogni turno in una variabile LOCALE che non torna nello stato, quindi
    /// leggere `state.system_text` misurerebbe cio' che il motore ha RICEVUTO, non
    /// cio' che MANDA (regola O). Qui la richiesta arriva dal produttore reale.
    struct GatewayCheRegistra {
        turns: Vec<LlmResponse>,
        richieste: Mutex<Vec<LlmRequest>>,
    }

    impl GatewayCheRegistra {
        fn new(turns: Vec<LlmResponse>) -> Self {
            Self {
                turns,
                richieste: Mutex::new(Vec::new()),
            }
        }

        /// Testa di ogni turno dell'EXECUTOR: `(system, nomi dei tool NELL'ORDINE
        /// dichiarato)`. L'ordine conta: il riuso e' per prefisso, non per insieme.
        ///
        /// Il filtro su `purpose` dichiara da dove guarda la misura: nel grafo
        /// chiamano l'LLM anche altri nodi (understanding, clarify), con un
        /// system diverso PER COSTRUZIONE. Confrontare le loro teste con quelle
        /// dell'executor misurerebbe una differenza legittima e direbbe "instabile"
        /// di un prefisso sano.
        fn teste(&self) -> Vec<(String, Vec<String>)> {
            self.richieste
                .lock()
                .expect("lock richieste")
                .iter()
                .filter(|r| r.purpose.as_deref() == Some("executor"))
                .map(|r| {
                    let sys = r.system_text.clone().unwrap_or_default();
                    let tools = r
                        .tools
                        .as_ref()
                        .map(|ts| {
                            ts.iter()
                                .map(|t| {
                                    t.get("name")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("?")
                                        .to_string()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (sys, tools)
                })
                .collect()
        }
    }

    #[async_trait]
    impl LlmGateway for GatewayCheRegistra {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            let idx = {
                let mut g = self.richieste.lock().expect("lock richieste");
                g.push(req);
                g.len() - 1
            };
            Ok(self.turns[idx.min(self.turns.len() - 1)].clone())
        }
    }

    /// Turno che chiama `write_file` su un path dato (path diversi fra turni: due
    /// chiamate identiche farebbero scattare la loop-detection e chiuderebbero il
    /// run prima di poter misurare i turni successivi).
    fn turn_tool_use_su(path: &str) -> LlmResponse {
        LlmResponse {
            content: String::new(),
            tool_calls: vec![ToolUse {
                id: format!("tc-{path}"),
                name: "write_file".to_string(),
                input: json!({"path": path, "content": "fn main() {}"}),
                thought_signature: None,
            }],
            usage: LlmUsage::default(),
            stop_reason: Some("tool_use".to_string()),
            ..Default::default()
        }
    }

    // Numero di caratteri iniziali in comune fra due teste: dice DOVE divergono,
    // non solo che divergono. Un numero senza la sua premessa e' un'opinione.
    //
    // La misura sta nel punto unico del prefisso (`nexus_types::system_prompt`,
    // accanto a `parte_stabile`): la stessa domanda se la pone il test del
    // compositore di RUN in mcp-core (`compose_agent_system_text`), e due
    // conteggi separati darebbero due idee diverse di "quanto e' comune".
    use nexus_types::system_prompt::prefisso_comune;

    #[tokio::test]
    async fn la_testa_del_prompt_resta_identica_fra_i_turni() {
        // CONTRATTO: in un run agentico il prefisso (system + catalogo tool) e' la
        // parte che NON deve cambiare da un turno all'altro — cresce solo la coda
        // della conversazione. E' la condizione perche' il fornitore riusi il
        // prefisso; se la testa cambia, ogni turno paga il prompt intero.
        let gateway = Arc::new(GatewayCheRegistra::new(vec![
            turn_tool_use_su("src/a.rs"),
            turn_tool_use_su("src/b.rs"),
            turn_tool_use_su("src/c.rs"),
            turn_end(),
        ]));
        let llm: Arc<dyn LlmGateway> = gateway.clone();
        let tools = stub_tools();
        let run_id = Uuid::new_v4();

        let nodes = build_stub_nodes(tools.clone());
        let engine = build_agent_graph(
            nodes,
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
            Arc::new(MemoryCheckpointer::default()),
        );

        // System e catalogo tool come li passa mcp-core: sono la testa del prompt.
        const SYSTEM: &str =
            "Sei l'agente di sviluppo del progetto Nexus. Lavora sul repository indicato.";
        let mut stato = initial_state(run_id);
        stato.system_text = Some(SYSTEM.to_string());
        stato.tools_json = Some(vec![
            json!({"name": "write_file"}),
            json!({"name": "read_file"}),
            json!({"name": "run_command"}),
        ]);

        let ctx = ctx_with(llm, tools, run_id);
        engine
            .run_until_interrupt(run_id, Some(stato), &ctx)
            .await
            .expect("il run deve completare senza errore");

        let teste = gateway.teste();
        assert!(
            teste.len() >= 3,
            "servono almeno 3 turni per misurare la stabilita', ottenuti {}",
            teste.len()
        );

        // Il criterio e' la CONSEGUENZA, non l'uguaglianza: le direttive di turno
        // possono cambiare (e devono, e' il loro mestiere), ma il tratto iniziale
        // che i turni condividono deve CONTENERE il system che il chiamante ha
        // fornito. Misurato sui caratteri effettivamente comuni, non chiedendo al
        // codice sotto misura dove passa il proprio confine.
        //
        // Perche' il contenimento e non la lunghezza: `comune >= len(SYSTEM)` e'
        // cieco per aritmetica. Le direttive di turno hanno un preambolo FISSO
        // (l'intestazione del focus ne vale ~100 caratteri, il SYSTEM di questo
        // test 76), quindi due prompt entrambi difettosi — con un blocco
        // variabile ANTEPOSTO — condividerebbero piu' caratteri del system stesso
        // e il test passerebbe col difetto in produzione. Se il tratto comune
        // deve invece arrivare a coprire il system per intero, un blocco che
        // diverge prima taglia il prefisso PRIMA di esso e il test cade.
        //
        // Cosa NON copre, dichiarato: un blocco anteposto che resti IDENTICO in
        // tutti i turni di questo run (il focus, ora che nasce dal task fissato
        // all'origine, e' di quelli). Dentro un run non fa danno — e' il riuso fra
        // RUN diversi che perde, e li' il metro e' un altro: due run con task
        // diverso, misurato in `planner::tests::il_focus_del_turno_non_apre_il_
        // prompt_del_planner`, dove la testa non deve nemmeno cominciare con un
        // blocco di turno. Pretendere qui `starts_with(SYSTEM)` sarebbe piu'
        // severo ma falso: il promemoria di lingua, quando la config lo accende,
        // precede il system per progetto ed e' fisso.
        let attesi = SYSTEM.chars().count();
        let (sys0, tool0) = teste[0].clone();
        for (i, (sys, tools)) in teste.iter().enumerate().skip(1) {
            assert_eq!(
                tools, &tool0,
                "turno {i}: il catalogo tool e' cambiato rispetto al primo turno \
                 ({tool0:?} -> {tools:?}): il prefisso non e' piu' riusabile"
            );
            let comune = prefisso_comune(&sys0, sys);
            let condiviso: String = sys0.chars().take(comune).collect();
            assert!(
                condiviso.contains(SYSTEM),
                "turno {i}: la testa condivisa col primo turno e' di {comune} caratteri \
                 (il system del run ne vale {attesi}) e NON lo contiene per intero. Un \
                 blocco variabile e' finito PRIMA della parte stabile: da li' in poi il \
                 fornitore non ha nulla da riusare.\ncondiviso: {condiviso:?}\nprimo \
                 turno: {sys0:?}\nturno {i}: {sys:?}"
            );
        }
    }

    // ── HITL: sospensione, resume, esecuzione pending, chiusura ───────────────

    #[tokio::test]
    async fn hitl_confirm_resume_esegue_pending_e_completa() {
        let gateway = Arc::new(ScriptedLlmGateway::new(vec![turn_tool_use(), turn_end()]));
        let llm: Arc<dyn LlmGateway> = gateway.clone();
        let tools = Arc::new(TrackingStubTool {
            exec_count: AtomicUsize::new(0),
        });
        let tools_trait: Arc<dyn ToolExecutor> = tools.clone();
        let run_id = Uuid::new_v4();

        let nodes = build_stub_nodes(tools_trait.clone());
        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let engine = build_agent_graph(
            nodes,
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
            checkpointer,
        );

        let ctx = ctx_with(llm, tools_trait, run_id);
        let mut state = initial_state(run_id);
        state.automation_mode = Some(AutomationMode::Confirm);

        let suspended = engine
            .run_until_interrupt(run_id, Some(state), &ctx)
            .await
            .expect("il run deve sospendersi su HITL");

        match &suspended {
            StepOutcome::Interrupted { state, resume_at } => {
                assert!(state.is_awaiting_confirmation());
                assert_eq!(*resume_at, NodeId::ToolDispatch);
                assert!(
                    state
                        .pending_tool_uses
                        .as_ref()
                        .is_some_and(|p| !p.is_empty())
                );
            }
            other => panic!("atteso Interrupted (HITL), ottenuto {other:?}"),
        }
        assert_eq!(
            tools.exec_count.load(Ordering::SeqCst),
            0,
            "HITL: nessun tool mutativo eseguito prima della conferma"
        );
        assert_eq!(gateway.call_count(), 1, "un solo turno LLM pre-sospensione");

        let completed = engine
            .resume_until_interrupt(run_id, hitl_resume_delta(), &ctx)
            .await
            .expect("resume HITL deve completare il run");

        let final_state = match completed {
            StepOutcome::Completed(s) => s,
            other => panic!("atteso Completed dopo resume HITL, ottenuto {other:?}"),
        };

        assert_eq!(final_state.stop_reason, Some(StopReason::EndTurn));
        assert!(
            final_state
                .pending_tool_uses
                .as_ref()
                .map(|p| p.is_empty())
                .unwrap_or(true)
        );
        assert_eq!(
            tools.exec_count.load(Ordering::SeqCst),
            1,
            "dopo conferma il tool mutativo pending deve essere eseguito una volta"
        );
        assert_eq!(
            gateway.call_count(),
            2,
            "due turni LLM: tool_use pre-HITL + end_turn post-resume"
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
            SupervisorConfig::default(),
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

    /// L'edge post-ToolDispatch chiude anche quando a dichiarare e' una FIGURA.
    ///
    /// Attraversa l'edge REALE della produzione (`build_edges` + `Edge::resolve`),
    /// non una sua imitazione: e' la stessa mappa che il grafo usa a runtime.
    /// Il difetto che cattura e' quello misurato su verifica-wd: il verdetto era
    /// gia' stato emesso e accettato, ma il routing riconosceva terminale solo
    /// `task_complete`, quindi si tornava all'executor e la figura girava a vuoto
    /// fino al wall-clock, dove il verdetto veniva scartato.
    #[test]
    fn edge_tool_dispatch_chiude_anche_sul_verdetto_di_ruolo() {
        let edges = build_edges(
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
        );
        let edge = edges.get(&NodeId::ToolDispatch).expect("edge ToolDispatch");

        // Senza alcuna dichiarazione si prosegue: il lavoro non e' finito.
        let vuoto = AgentState::default();
        assert_eq!(edge.resolve(&vuoto), NodeId::Executor);

        // Ogni figura chiude sul PROPRIO canale, qualunque sia il giudizio:
        // anche un "needs_changes" e' il deliverable completo del revisore.
        for (etichetta, mut s) in [
            ("review", AgentState::default()),
            ("advisory", AgentState::default()),
            ("debate", AgentState::default()),
        ] {
            match etichetta {
                "review" => s.review_verdict = Some(json!({"verdict": "needs_changes"})),
                "advisory" => s.advisory_verdict = Some(json!({"verdict": "proceed"})),
                _ => s.debate_position = Some(json!({"stance": "contro"})),
            }
            assert_eq!(
                edge.resolve(&s),
                NodeId::FinalGate,
                "una figura che ha dichiarato su {etichetta} deve chiudere, non rientrare nell'executor"
            );
        }

        // Il canale di chi ESEGUE resta invariato (nessuna regressione).
        let esecutore = AgentState {
            declared_outcome: Some(json!({"outcome": "done"})),
            ..Default::default()
        };
        assert_eq!(edge.resolve(&esecutore), NodeId::FinalGate);
        let parziale = AgentState {
            declared_outcome: Some(json!({"outcome": "partial"})),
            ..Default::default()
        };
        assert_eq!(
            edge.resolve(&parziale),
            NodeId::Executor,
            "`partial` e' dichiarazione onesta di lavoro incompleto: prosegue"
        );
    }

    /// REGRESSIONE (motore reale, grafo completo): il blocco "FOCUS DEL TURNO
    /// CORRENTE" dichiara al modello quale sia la richiesta dell'utente ADESSO.
    /// Era costruito sull'ULTIMO `Message::Human` della cronologia, e dal secondo
    /// turno in poi quel messaggio non e' piu' l'utente: e' quello che
    /// `tool_dispatch` produce coi risultati dei tool. Il blocco spariva (i
    /// tool_result sono blocchi tipizzati, `flatten_text` li ignora) oppure —
    /// quando un promemoria `<system-reminder>` era appeso ai risultati —
    /// restava dichiarando quel promemoria come richiesta dell'utente.
    ///
    /// Qui si misura cio' che il modello LEGGE: il `system_text` di ogni
    /// richiesta al gateway, dopo un giro completo executor -> tool_dispatch ->
    /// executor. Il test prova anche che la richiesta sopravvive ai nodi
    /// attraversati (ogni nodo che scrive `extra` deve preservarne le chiavi).
    ///
    /// Verifica per mutazione: ripristinando l'euristica "ultimo Human" il
    /// secondo system resta senza il blocco e il test fallisce.
    #[tokio::test]
    async fn il_focus_del_turno_cita_la_richiesta_anche_dopo_i_tool() {
        let gateway = Arc::new(ScriptedLlmGateway::new(vec![turn_tool_use(), turn_end()]));
        let llm: Arc<dyn LlmGateway> = gateway.clone();
        let tools = stub_tools();
        let run_id = Uuid::new_v4();

        let nodes = build_stub_nodes(tools.clone());
        let engine = build_agent_graph(
            nodes,
            RoutingConfig::default(),
            PlannerConfig::default(),
            SupervisorConfig::default(),
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = ctx_with(llm, tools, run_id);
        engine
            .run_until_interrupt(run_id, Some(initial_state(run_id)), &ctx)
            .await
            .expect("il run end-to-end deve completare senza errore");

        // Le uniche due chiamate al gateway sono i due turni dell'executor
        // (1: tool_use -> tool_dispatch, 2: end_turn), come da
        // `run_completo_attraversa_il_loop_e_chiude`.
        let systems = gateway.systems();
        assert_eq!(systems.len(), 2, "attesi due turni dell'executor: {systems:?}");

        for (i, system) in systems.iter().enumerate() {
            let system = system
                .as_deref()
                .unwrap_or_else(|| panic!("turno {i}: system_text assente"));
            assert!(
                system.contains(crate::decisions::turn_focus::TURN_FOCUS_MARKER),
                "turno {i}: il focus del turno non e' stato iniettato"
            );
            assert!(
                system.contains(RICHIESTA_DEL_TURNO),
                "turno {i}: il focus non cita la richiesta dell'utente"
            );
        }

        // Il secondo turno e' quello che segue il tool_dispatch: se il focus
        // seguisse i messaggi, li' citerebbe l'esito del tool.
        let secondo = systems[1].as_deref().expect("system del secondo turno");
        assert!(
            !secondo.contains("contenuto del file letto"),
            "il focus ha dichiarato l'output di un tool come richiesta: {secondo}"
        );
    }
}
