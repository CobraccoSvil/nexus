//! Motore di grafo: loop superstep (stile Pregel), merge via reducer, route via
//! `Edge`, checkpoint dopo il route, interrupt su stati che attendono input.
//!
//! Il grafo e' fisso e piccolo: il motore e' una macchina a stati su un
//! `enum NodeId` chiuso, non il Pregel generico di LangGraph. Un superstep =
//! un nodo eseguito + merge + route + checkpoint.

use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::checkpoint::{Checkpointer, CheckpointError};
use crate::edge::Edge;
use crate::node::{GraphNode, NodeError, NodeId};
use crate::outcome::StepOutcome;
use crate::state::GraphState;

/// Contesto di esecuzione: il motore ne richiede solo il `recursion_limit`
/// (DB-driven, regola G — nessun default hardcoded nel motore). Il contesto
/// concreto (gateway, tool, DB, ...) lo implementa il crate dei nodi.
pub trait NodeCtxLike: Send + Sync {
    /// Numero massimo di superstep prima di abortire (`agent.graph.recursion_limit`).
    fn recursion_limit(&self) -> u32;
}

/// Errore del motore di grafo.
#[derive(Debug, Error)]
pub enum GraphError {
    /// Superato il limite di superstep configurato (loop o grafo che non
    /// converge). Il valore e' il limite raggiunto.
    #[error("recursion_limit superato ({0} superstep)")]
    RecursionLimit(u32),

    /// Instradamento verso un nodo non registrato nel grafo (topologia
    /// incoerente: bug di costruzione del grafo).
    #[error("nodo non registrato nel grafo: {0:?}")]
    MissingNode(NodeId),

    /// Manca l'edge uscente dal nodo corrente (topologia incompleta).
    #[error("edge mancante per il nodo: {0:?}")]
    MissingEdge(NodeId),

    /// Resume richiesto (init=None) ma nessun checkpoint trovato per il run.
    #[error("nessun checkpoint da cui riprendere per il run {0}")]
    NoCheckpoint(Uuid),

    /// Un nodo ha fallito durante `run`.
    #[error(transparent)]
    Node(#[from] NodeError),

    /// Errore del checkpointer (persistenza).
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
}

/// Motore del grafo. Generico su stato `S` e contesto `C`.
///
/// La topologia (`nodes` + `edges` + `entry`) e' immutabile dopo la
/// costruzione: il grafo Nexus e' fisso.
pub struct GraphEngine<S, C> {
    nodes: HashMap<NodeId, Arc<dyn GraphNode<S, C>>>,
    edges: HashMap<NodeId, Edge<S>>,
    entry: NodeId,
    checkpointer: Arc<dyn Checkpointer<S>>,
}

impl<S, C> GraphEngine<S, C>
where
    S: GraphState + Clone,
    C: NodeCtxLike,
{
    /// Costruisce il motore. `entry` deve essere un nodo registrato; gli edge
    /// devono coprire ogni nodo non terminale (la copertura e' verificata a
    /// runtime al primo route mancante: `GraphError::MissingEdge`).
    pub fn new(
        nodes: HashMap<NodeId, Arc<dyn GraphNode<S, C>>>,
        edges: HashMap<NodeId, Edge<S>>,
        entry: NodeId,
        checkpointer: Arc<dyn Checkpointer<S>>,
    ) -> Self {
        Self {
            nodes,
            edges,
            entry,
            checkpointer,
        }
    }

    /// Esegue il grafo fino al primo interrupt (input umano) o fino a `End`.
    ///
    /// - `init = Some(s)`: avvio nuovo, parte da `entry` con stato `s`.
    /// - `init = None`: RESUME, carica l'ultimo checkpoint e riparte dal
    ///   `next_node` salvato (risolve il "checkpoint morto": il run riprende
    ///   invece di rieseguire da capo).
    pub async fn run_until_interrupt(
        &self,
        run_id: Uuid,
        init: Option<S>,
        ctx: &C,
    ) -> Result<StepOutcome<S>, GraphError> {
        // Risoluzione dello stato e del nodo di partenza.
        let (mut state, mut current) = match init {
            Some(s) => (s, self.entry),
            None => self
                .checkpointer
                .load(run_id)
                .await?
                .ok_or(GraphError::NoCheckpoint(run_id))?,
        };

        // Resume di un run gia' concluso: l'ultimo checkpoint puntava a `End`.
        // Non c'e' nulla da rieseguire, il run e' gia' completo.
        if current == NodeId::End {
            return Ok(StepOutcome::Completed(state));
        }

        let recursion_limit = ctx.recursion_limit(); // DB-driven (regola G)
        let mut superstep: u32 = 0;

        loop {
            if superstep >= recursion_limit {
                return Err(GraphError::RecursionLimit(recursion_limit));
            }

            // Esecuzione del nodo corrente: qui avviene tutto l'I/O.
            let node = self
                .nodes
                .get(&current)
                .ok_or(GraphError::MissingNode(current))?;
            let delta = node.run(&state, ctx).await?;

            // Merge via reducer (punto unico, regola L).
            state.merge(delta);

            // Route DOPO il merge (le route leggono lo stato aggiornato).
            let next = self
                .edges
                .get(&current)
                .ok_or(GraphError::MissingEdge(current))?
                .resolve(&state);

            // Checkpoint DOPO il route: il record contiene gia' il prossimo
            // nodo, cosi' il resume riparte da li' senza ricalcolo.
            self.checkpointer
                .put(run_id, superstep as i64, next, &state)
                .await?;

            tracing::debug!(
                run_id = %run_id,
                superstep,
                from = current.as_label(),
                to = next.as_label(),
                "graph_superstep"
            );

            if next == NodeId::End {
                return Ok(StepOutcome::Completed(state));
            }

            // Interrupt: lo stato attende input umano. Si ferma e indica da dove
            // riprendere (il nodo gia' instradato).
            if state.is_awaiting_confirmation() || state.is_pending_clarify() {
                return Ok(StepOutcome::Interrupted {
                    state,
                    resume_at: next,
                });
            }

            current = next;
            superstep += 1;
        }
    }
}
