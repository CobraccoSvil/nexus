//! `nexus-graph` — runtime di grafo PURO per l'orchestrazione agentica.
//!
//! Macchina a stati a superstep (stile Pregel) su un `enum NodeId` chiuso. Non
//! conosce nulla dei nodi Nexus concreti (LLM/tool/RAG/DB): quelli vivono in
//! `nexus-agent-graph`. Separazione runtime/nodi -> regola L (punto unico) +
//! composition-over-inheritance.
//!
//! Qui dentro l'unica implementazione di `GraphNode` e' `NoOpNode`, che serve ai
//! test del motore: il crate resta indipendente dai nodi concreti e non ne
//! dichiara il numero. Chi esegue il grafo in produzione e' `mcp-core`
//! (`native_engine`), che assembla i nodi di `nexus-agent-graph` su questo
//! motore.

pub mod checkpoint;
pub mod edge;
pub mod engine;
pub mod node;
pub mod outcome;
pub mod state;

pub use checkpoint::{CheckpointError, Checkpointer, MemoryCheckpointer};
pub use edge::Edge;
pub use engine::{GraphEngine, GraphError, NodeCtxLike};
pub use node::{GraphNode, NodeError, NodeId, NoOpNode};
pub use outcome::StepOutcome;
pub use state::{GraphState, StateDelta};

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    use super::*;

    // --- Stato minimale di prova -------------------------------------------

    /// Stato di test: un contatore di passi (per il caso loop) e i due flag di
    /// interrupt. Implementa `GraphState` con un reducer banale.
    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct TestState {
        steps: i64,
        awaiting_confirmation: bool,
        pending_clarify: bool,
    }

    impl GraphState for TestState {
        fn merge(&mut self, delta: StateDelta) {
            if let Some(v) = delta.as_map().get("steps").and_then(|v| v.as_i64()) {
                self.steps = v;
            }
            if let Some(v) = delta
                .as_map()
                .get("awaiting_confirmation")
                .and_then(|v| v.as_bool())
            {
                self.awaiting_confirmation = v;
            }
            if let Some(v) = delta
                .as_map()
                .get("pending_clarify")
                .and_then(|v| v.as_bool())
            {
                self.pending_clarify = v;
            }
        }

        fn is_awaiting_interrupt(&self) -> bool {
            self.awaiting_confirmation
        }
    }

    // --- Contesto minimale -------------------------------------------------

    struct TestCtx {
        recursion_limit: u32,
    }

    impl NodeCtxLike for TestCtx {
        fn recursion_limit(&self) -> u32 {
            self.recursion_limit
        }
    }

    // --- Checkpointer in memoria: PUNTO UNICO `MemoryCheckpointer` (niente DB),
    //     riusato dai test e dal run shadow in produzione (regola L). Qui lo si
    //     istanzia col tipo di stato di prova `TestState`. ----------------------

    // --- Nodo che cicla all'infinito (test recursion_limit) ----------------

    /// Instrada sempre a se' stesso (vedi edge `Static(NoOp)` nel grafo del
    /// test): senza limite il motore girerebbe per sempre.
    struct LoopNode;

    #[async_trait]
    impl GraphNode<TestState, TestCtx> for LoopNode {
        fn id(&self) -> NodeId {
            NodeId::NoOp
        }

        async fn run(
            &self,
            state: &TestState,
            _ctx: &TestCtx,
        ) -> Result<StateDelta, NodeError> {
            let mut delta = StateDelta::empty();
            delta.set("steps", serde_json::json!(state.steps + 1));
            Ok(delta)
        }
    }

    // --- Test ---------------------------------------------------------------

    #[tokio::test]
    async fn noop_to_end_ritorna_completed() {
        // Grafo: NoOp --(End)--> fine.
        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::NoOp,
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = TestCtx {
            recursion_limit: 50,
        };
        let outcome = engine
            .run_until_interrupt(Uuid::new_v4(), Some(TestState::default()), &ctx)
            .await
            .expect("il grafo NoOp->End deve completare");

        assert!(matches!(outcome, StepOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn loop_supera_recursion_limit() {
        // Grafo: LoopNode --(NoOp)--> LoopNode (self-loop infinito).
        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::NoOp, Arc::new(LoopNode));
        let mut edges = HashMap::new();
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::NoOp));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::NoOp,
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = TestCtx { recursion_limit: 5 };
        let err = engine
            .run_until_interrupt(Uuid::new_v4(), Some(TestState::default()), &ctx)
            .await
            .expect_err("un grafo che cicla deve superare il recursion_limit");

        match err {
            GraphError::RecursionLimit(limit) => assert_eq!(limit, 5),
            other => panic!("atteso RecursionLimit, ottenuto {other:?}"),
        }
    }

    #[tokio::test]
    async fn interrupt_su_awaiting_confirmation() {
        // HITL vero: un nodo setta awaiting_confirmation, edge verso un altro nodo
        // (NON End). Il motore deve SOSPENDERE indicando il resume_at (lo stesso
        // run riprende da li' dopo la conferma). E' l'UNICO predicato di interrupt.
        struct ConfirmNode;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ConfirmNode {
            fn id(&self) -> NodeId {
                NodeId::Router
            }
            async fn run(
                &self,
                _state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("awaiting_confirmation", serde_json::json!(true));
                Ok(delta)
            }
        }

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::Router, Arc::new(ConfirmNode));
        // Il NoOp finale serve solo come destinazione (mai eseguito: interrupt prima).
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        edges.insert(NodeId::Router, Edge::Static(NodeId::NoOp));
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::Router,
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = TestCtx {
            recursion_limit: 50,
        };
        let outcome = engine
            .run_until_interrupt(Uuid::new_v4(), Some(TestState::default()), &ctx)
            .await
            .expect("deve interrompersi senza errore");

        match outcome {
            StepOutcome::Interrupted { resume_at, .. } => {
                assert_eq!(resume_at, NodeId::NoOp);
            }
            other => panic!("atteso Interrupted, ottenuto {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_clarify_non_interrompe_il_motore() {
        // pending_clarify NON e' piu' un interrupt del motore: e' uno stato
        // TERMINALE gestito dalla TOPOLOGIA (edge a End). Senza un edge a End, il
        // motore PROSEGUE (non sospende). Qui un nodo setta pending_clarify ma
        // l'edge va verso un nodo che chiude: il run COMPLETA, non si interrompe.
        struct ClarifyNode;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ClarifyNode {
            fn id(&self) -> NodeId {
                NodeId::Router
            }
            async fn run(
                &self,
                _state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("pending_clarify", serde_json::json!(true));
                Ok(delta)
            }
        }

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::Router, Arc::new(ClarifyNode));
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        // Router -> NoOp -> End: pending_clarify NON deve fermare il motore prima.
        edges.insert(NodeId::Router, Edge::Static(NodeId::NoOp));
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::Router,
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = TestCtx {
            recursion_limit: 50,
        };
        let outcome = engine
            .run_until_interrupt(Uuid::new_v4(), Some(TestState::default()), &ctx)
            .await
            .expect("il motore non deve interrompersi su pending_clarify");

        // NoOp viene eseguito (il motore non si e' fermato su pending_clarify) e il
        // run chiude con Completed.
        match outcome {
            StepOutcome::Completed(s) => assert!(s.pending_clarify),
            other => panic!("atteso Completed, ottenuto {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_clarify_con_edge_a_end_e_terminale() {
        // Replica la topologia reale (graph.rs): un nodo setta pending_clarify e il
        // suo edge CONDIZIONALE instrada a End. Il run e' TERMINALE -> Completed,
        // SENZA attraversare il nodo successivo. Verifica il fix della divergenza.
        struct ClarifyNode;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ClarifyNode {
            fn id(&self) -> NodeId {
                NodeId::Router
            }
            async fn run(
                &self,
                _state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("pending_clarify", serde_json::json!(true));
                Ok(delta)
            }
        }
        // Nodo "successivo" che NON deve mai essere eseguito (settarebbe steps).
        struct ShouldNotRun;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ShouldNotRun {
            fn id(&self) -> NodeId {
                NodeId::Understanding
            }
            async fn run(
                &self,
                state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("steps", serde_json::json!(state.steps + 100));
                Ok(delta)
            }
        }

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::Router, Arc::new(ClarifyNode));
        nodes.insert(NodeId::Understanding, Arc::new(ShouldNotRun));
        let mut edges = HashMap::new();
        // Edge condizionale come in graph.rs: pending_clarify -> End, altrimenti
        // -> Understanding.
        edges.insert(
            NodeId::Router,
            Edge::conditional(|s: &TestState| {
                if s.pending_clarify {
                    NodeId::End
                } else {
                    NodeId::Understanding
                }
            }),
        );
        edges.insert(NodeId::Understanding, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::Router,
            Arc::new(MemoryCheckpointer::default()),
        );

        let ctx = TestCtx {
            recursion_limit: 50,
        };
        let outcome = engine
            .run_until_interrupt(Uuid::new_v4(), Some(TestState::default()), &ctx)
            .await
            .expect("pending_clarify con edge a End deve completare");

        match outcome {
            StepOutcome::Completed(s) => {
                assert!(s.pending_clarify, "pending_clarify resta valorizzato");
                // Understanding NON e' stato eseguito: steps invariato (terminale).
                assert_eq!(s.steps, 0, "il nodo successivo NON deve essere eseguito");
            }
            other => panic!("atteso Completed, ottenuto {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_da_checkpoint_senza_init() {
        // Primo run: NoOp->End completa e lascia un checkpoint.
        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let run_id = Uuid::new_v4();

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(nodes, edges, NodeId::NoOp, checkpointer.clone());
        let ctx = TestCtx {
            recursion_limit: 50,
        };

        engine
            .run_until_interrupt(run_id, Some(TestState::default()), &ctx)
            .await
            .expect("primo run deve completare");

        // Resume con init=None: deve trovare il checkpoint (next_node=end) e
        // chiudere subito con Completed, senza NoCheckpoint.
        let outcome = engine
            .run_until_interrupt(run_id, None, &ctx)
            .await
            .expect("il resume deve trovare il checkpoint");
        assert!(matches!(outcome, StepOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn resume_hitl_applica_delta_e_prosegue() {
        // Replica il flusso HITL reale: un run si ferma su awaiting_confirmation,
        // poi il resume con un delta che AZZERA il flag deve far proseguire il
        // motore fino a End (non re-interrompersi sul checkpoint ancora-in-attesa).
        struct ConfirmNode;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ConfirmNode {
            fn id(&self) -> NodeId {
                NodeId::Router
            }
            async fn run(
                &self,
                _state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("awaiting_confirmation", serde_json::json!(true));
                Ok(delta)
            }
        }

        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let run_id = Uuid::new_v4();

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::Router, Arc::new(ConfirmNode));
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        // Router (setta awaiting) -> NoOp -> End. L'interrupt scatta dopo il route
        // a NoOp; il resume riprende da NoOp.
        edges.insert(NodeId::Router, Edge::Static(NodeId::NoOp));
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(nodes, edges, NodeId::Router, checkpointer.clone());
        let ctx = TestCtx {
            recursion_limit: 50,
        };

        // Primo run: si interrompe su awaiting_confirmation, resume_at = NoOp.
        let outcome = engine
            .run_until_interrupt(run_id, Some(TestState::default()), &ctx)
            .await
            .expect("deve interrompersi");
        assert!(matches!(outcome, StepOutcome::Interrupted { .. }));

        // Resume HITL: delta che azzera awaiting_confirmation. Senza di esso lo
        // stato caricato avrebbe ancora il flag e il motore si re-interromperebbe.
        let mut resume_delta = StateDelta::empty();
        resume_delta.set("awaiting_confirmation", serde_json::json!(false));
        let outcome = engine
            .resume_until_interrupt(run_id, resume_delta, &ctx)
            .await
            .expect("il resume HITL deve proseguire");
        assert!(
            matches!(outcome, StepOutcome::Completed(_)),
            "azzerato awaiting_confirmation, il run deve completare"
        );
    }

    #[tokio::test]
    async fn resume_hitl_re_interrupt_se_flag_non_azzerato_e_resta_un_nodo() {
        // Difensivo: il re-interrupt scatta se, DOPO il route, lo stato e' ancora
        // in attesa E resta un nodo non-End da eseguire. Qui il nodo successivo
        // RI-setta awaiting_confirmation e instrada verso un altro nodo non-End:
        // col delta vuoto lo stato resta in attesa -> il motore si re-interrompe.
        struct ConfirmNode;
        #[async_trait]
        impl GraphNode<TestState, TestCtx> for ConfirmNode {
            fn id(&self) -> NodeId {
                NodeId::Router
            }
            async fn run(
                &self,
                _state: &TestState,
                _ctx: &TestCtx,
            ) -> Result<StateDelta, NodeError> {
                let mut delta = StateDelta::empty();
                delta.set("awaiting_confirmation", serde_json::json!(true));
                Ok(delta)
            }
        }

        let checkpointer = Arc::new(MemoryCheckpointer::default());
        let run_id = Uuid::new_v4();

        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::Router, Arc::new(ConfirmNode));
        // Understanding e' un nodo non-End: la sua presenza fa scattare il
        // re-interrupt (resta un nodo da eseguire con awaiting ancora true).
        nodes.insert(NodeId::Understanding, Arc::new(ConfirmNode));
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        // Router -> Understanding (interrupt qui) -> NoOp -> End.
        edges.insert(NodeId::Router, Edge::Static(NodeId::Understanding));
        edges.insert(NodeId::Understanding, Edge::Static(NodeId::NoOp));
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(nodes, edges, NodeId::Router, checkpointer.clone());
        let ctx = TestCtx {
            recursion_limit: 50,
        };

        engine
            .run_until_interrupt(run_id, Some(TestState::default()), &ctx)
            .await
            .expect("deve interrompersi");

        // Delta vuoto: il nodo Understanding ri-setta awaiting_confirmation e
        // instrada verso NoOp (non-End) -> il motore si re-interrompe.
        let outcome = engine
            .resume_until_interrupt(run_id, StateDelta::empty(), &ctx)
            .await
            .expect("il resume non deve errorare");
        assert!(
            matches!(outcome, StepOutcome::Interrupted { .. }),
            "flag ri-settato + nodo residuo -> re-interrupt"
        );
    }

    #[tokio::test]
    async fn resume_senza_checkpoint_e_errore() {
        let mut nodes: HashMap<NodeId, Arc<dyn GraphNode<TestState, TestCtx>>> = HashMap::new();
        nodes.insert(NodeId::NoOp, Arc::new(NoOpNode));
        let mut edges = HashMap::new();
        edges.insert(NodeId::NoOp, Edge::Static(NodeId::End));

        let engine = GraphEngine::new(
            nodes,
            edges,
            NodeId::NoOp,
            Arc::new(MemoryCheckpointer::default()),
        );
        let ctx = TestCtx {
            recursion_limit: 50,
        };

        let err = engine
            .run_until_interrupt(Uuid::new_v4(), None, &ctx)
            .await
            .expect_err("resume senza checkpoint deve fallire");
        assert!(matches!(err, GraphError::NoCheckpoint(_)));
    }

    #[test]
    fn node_label_round_trip() {
        for id in [
            NodeId::NoOp,
            NodeId::End,
            NodeId::Router,
            NodeId::ClarifyOrExpand,
            NodeId::Understanding,
            NodeId::Planner,
            NodeId::TodoRunner,
            NodeId::Executor,
            NodeId::ToolDispatch,
            NodeId::Verifier,
            NodeId::FinalGate,
            NodeId::Reflection,
            NodeId::Learner,
        ] {
            assert_eq!(NodeId::from_label(id.as_label()), Some(id));
        }
    }
}
