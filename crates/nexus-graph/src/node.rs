//! Nodi del grafo: identificatore esaustivo, contratto `GraphNode`, errori.
//!
//! Il grafo Nexus e' FISSO e piccolo (12 nodi reali, topologia nota a
//! compile-time). `NodeId` e' un enum chiuso: il `match` esaustivo del
//! compilatore garantisce che ogni nodo abbia un edge dichiarato (impossibile
//! dimenticarne uno), vantaggio strutturale rispetto ai dict Python.
//!
//! In FASE 0 (scaffold) esiste solo `NoOpNode`: i 12 nodi reali sono dichiarati
//! come varianti placeholder ma NON implementati (verranno portati nelle fasi
//! successive). Il path Rust non viene mai imboccato finche' la tabella di
//! routing non lo abilita.

use async_trait::async_trait;
use thiserror::Error;

use crate::state::StateDelta;

/// Identificatore di nodo: enum chiuso sull'intera topologia del grafo agentico.
///
/// `NoOp`/`End` servono allo scaffold e ai test del motore. Le restanti varianti
/// sono i 12 nodi reali del grafo Nexus (placeholder finche' non portati). Il
/// nodo Python `g1_continue` NON e' presente: nel runtime Rust il self-loop
/// `Executor -> Executor` e' nativo, quindi quel workaround si elimina alla
/// radice (regola H), non si porta.
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
    Verifier,
    FinalGate,
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
            NodeId::Verifier => "verifier",
            NodeId::FinalGate => "final_gate",
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
            "verifier" => NodeId::Verifier,
            "final_gate" => NodeId::FinalGate,
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
