//! Nodi del grafo: identificatore esaustivo, contratto `GraphNode`, errori.
//!
//! Il grafo Nexus e' FISSO: `NodeId` e' un enum chiuso e la topologia e' nota a
//! compile-time. Il `match` esaustivo del compilatore garantisce che ogni nodo
//! abbia un edge dichiarato: aggiungere una variante senza instradarla non
//! compila.
//!
//! Le implementazioni concrete non stanno qui: questo crate definisce il
//! contratto `GraphNode` e `NoOpNode` (usato dai test del motore). I nodi veri
//! vivono in `nexus-agent-graph::nodes`, uno per variante di `NodeId`.

use async_trait::async_trait;
use thiserror::Error;

use crate::state::StateDelta;

/// Identificatore di nodo: enum chiuso sull'intera topologia del grafo agentico.
///
/// `NoOp`/`End` servono al motore e ai suoi test; ogni altra variante ha
/// un'implementazione in `nexus-agent-graph::nodes`. Non esiste una variante
/// `g1_continue`: il self-loop `Executor -> Executor` e' nativo del runtime,
/// quindi quel nodo-ponte non serve (regola H: causa rimossa, non aggirata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// Nodo di test: ritorna delta vuoto e instrada a `End`.
    NoOp,
    /// Terminatore del grafo. Raggiungerlo chiude il run con `Completed`.
    End,
    // --- 12 nodi reali del grafo Nexus (placeholder, non implementati in Fase 0) ---
    Router,
    ClarifyOrExpand,
    Understanding,
    Planner,
    TodoRunner,
    Executor,
    ToolDispatch,
    /// Nodo del meta-reasoner di recovery-da-stallo (superstep dedicato,
    /// ADR 0036-style). Raggiunto dall'executor quando un detector strutturato
    /// segnala uno stallo che richiede meta-ragionamento (`StopReason::StallReason`);
    /// consulta la porta `MetaReasonerPort` (UNA sola LLM-call via `ctx.llm`,
    /// replay-safe) e rientra nell'executor via self-loop (`StopReason::StallResolved`).
    StallRecovery,
    /// Nodo dello SCALE-CONTROLLER (superstep dedicato, gemello di `StallRecovery`).
    /// Raggiunto dall'executor quando un detector strutturato segnala di valutare la
    /// scala-tier del modello (`StopReason::ScaleReason`); consulta la porta
    /// `MetaReasonerPort::assess_scale` (UNA sola LLM-call via `ctx.llm`, replay-safe)
    /// e rientra nell'executor via self-loop (`StopReason::ScaleResolved`).
    /// L'emissione di `ScaleReason` ha per gate `agent.scale.enabled`.
    ScaleControl,
    /// Supervisore worker: monitora l'avanzamento e puo' redirectare/abbandonare.
    Supervisor,
    Verifier,
    FinalGate,
    /// Gate della review adversariale (gemello del FinalGate): interposto sul
    /// funnel di chiusura onesta, su bocciatura rimanda in correzione
    /// all'Executor invece di lasciare che il run arrivi a End con un verdetto
    /// Fail pendente (il resume di un run a End e' un no-op per costruzione).
    ReviewGate,
    Reflection,
    Learner,
}

impl NodeId {
    /// Etichetta stabile usata per la persistenza del checkpoint (`next_node`
    /// TEXT) e i log. Deve restare allineata a `from_label` (round-trip).
    pub fn as_label(&self) -> &'static str {
        match self {
            NodeId::NoOp => "noop",
            NodeId::End => "end",
            NodeId::Router => "router",
            NodeId::ClarifyOrExpand => "clarify_or_expand",
            NodeId::Understanding => "understanding",
            NodeId::Planner => "planner",
            NodeId::TodoRunner => "todo_runner",
            NodeId::Executor => "executor",
            NodeId::ToolDispatch => "tool_dispatch",
            NodeId::StallRecovery => "stall_recovery",
            NodeId::ScaleControl => "scale_control",
            NodeId::Supervisor => "supervisor",
            NodeId::Verifier => "verifier",
            NodeId::FinalGate => "final_gate",
            NodeId::ReviewGate => "review_gate",
            NodeId::Reflection => "reflection",
            NodeId::Learner => "learner",
        }
    }

    /// Inverso di `as_label`. Usato dal checkpointer per ricostruire il
    /// puntatore di esecuzione (`next_node`) salvato come TEXT.
    pub fn from_label(label: &str) -> Option<NodeId> {
        let id = match label {
            "noop" => NodeId::NoOp,
            "end" => NodeId::End,
            "router" => NodeId::Router,
            "clarify_or_expand" => NodeId::ClarifyOrExpand,
            "understanding" => NodeId::Understanding,
            "planner" => NodeId::Planner,
            "todo_runner" => NodeId::TodoRunner,
            "executor" => NodeId::Executor,
            "tool_dispatch" => NodeId::ToolDispatch,
            "stall_recovery" => NodeId::StallRecovery,
            "scale_control" => NodeId::ScaleControl,
            "supervisor" => NodeId::Supervisor,
            "verifier" => NodeId::Verifier,
            "final_gate" => NodeId::FinalGate,
            "review_gate" => NodeId::ReviewGate,
            "reflection" => NodeId::Reflection,
            "learner" => NodeId::Learner,
            _ => return None,
        };
        Some(id)
    }
}

/// Errore emesso da un nodo durante `run`. Lo stato e' opaco al runtime: il
/// dettaglio del fallimento e' un messaggio + un eventuale errore sorgente.
#[derive(Debug, Error)]
pub enum NodeError {
    /// Fallimento applicativo del nodo (LLM, tool, DB, ...) con messaggio.
    #[error("nodo '{node}' fallito: {message}")]
    Failed {
        node: &'static str,
        message: String,
    },
    /// Errore propagato da una dipendenza (riconfezionato con `anyhow`-like).
    #[error("nodo '{node}': {source}")]
    Source {
        node: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Contratto di un nodo del grafo.
///
/// `S` = tipo dello stato condiviso (vincolato a `GraphState` nel motore),
/// `C` = contesto di esecuzione (dipendenze: gateway LLM, tool, DB, config).
///
/// `run` riceve lo stato in sola lettura e ritorna un `StateDelta` (cio' che il
/// nodo ha modificato); il merge nello stato e' responsabilita' del motore via
/// il reducer (punto unico, regola L). I nodi NON instradano: l'edge e'
/// dichiarato fuori dal nodo (`edge.rs`), cosi' la topologia resta in un solo
/// posto.
#[async_trait]
pub trait GraphNode<S, C>: Send + Sync {
    /// Identita' del nodo (per edge-lookup, checkpoint, log).
    fn id(&self) -> NodeId;

    /// Esegue il nodo. Qui avviene tutto l'I/O (LLM, tool, gRPC, DB).
    async fn run(&self, state: &S, ctx: &C) -> Result<StateDelta, NodeError>;
}

/// Nodo di test/scaffold: non tocca lo stato (delta vuoto) e instrada a `End`
/// (l'edge `Static(End)` e' dichiarato dal costruttore del grafo, non qui).
pub struct NoOpNode;

#[async_trait]
impl<S, C> GraphNode<S, C> for NoOpNode
where
    S: Send + Sync,
    C: Send + Sync,
{
    fn id(&self) -> NodeId {
        NodeId::NoOp
    }

    async fn run(&self, _state: &S, _ctx: &C) -> Result<StateDelta, NodeError> {
        Ok(StateDelta::empty())
    }
}
