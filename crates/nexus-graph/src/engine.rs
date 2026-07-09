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
        self.run_inner(run_id, init, None, ctx).await
    }

    /// RESUME HITL: riprende un run sospeso su `awaiting_confirmation` dal suo
    /// checkpoint, applicando PRIMA un `resume_delta` (l'input umano di
    /// approvazione: tipicamente azzera `awaiting_confirmation` e inietta il
    /// messaggio di conferma). Il delta passa per lo STESSO reducer
    /// ([`GraphState::merge`], punto unico, regola L): nessuna scrittura diretta
    /// dei campi qui. Poi il motore prosegue dal `next_node` salvato.
    ///
    /// Senza `resume_delta` che azzera il predicato di interrupt, lo stato
    /// caricato avrebbe ancora `is_awaiting_confirmation() == true` e il motore
    /// si re-interromperebbe al primo route (loop di conferma). Spetta dunque al
    /// chiamante (mcp-core) costruire il delta che sblocca l'interrupt.
    pub async fn resume_until_interrupt(
        &self,
        run_id: Uuid,
        resume_delta: crate::state::StateDelta,
        ctx: &C,
    ) -> Result<StepOutcome<S>, GraphError> {
        self.run_inner(run_id, None, Some(resume_delta), ctx).await
    }

    /// Loop interno condiviso tra avvio nuovo e resume (punto unico, regola L).
    ///
    /// `init`/`resume_delta` distinguono i tre ingressi:
    /// - `init = Some(s)`: avvio nuovo da `entry` con stato `s`.
    /// - `init = None`, `resume_delta = None`: resume "puro" dal checkpoint
    ///   (recovery di un run interrotto, riparte dal `next_node` salvato).
    /// - `init = None`, `resume_delta = Some(d)`: resume HITL — carica il
    ///   checkpoint e applica `d` (input di approvazione) PRIMA di proseguire.
    async fn run_inner(
        &self,
        run_id: Uuid,
        init: Option<S>,
        resume_delta: Option<crate::state::StateDelta>,
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

        // Resume HITL: applica l'input di approvazione allo stato caricato PRIMA
        // di valutare l'interrupt o di proseguire. Passa per il reducer (punto
        // unico): azzera `awaiting_confirmation` e inietta il messaggio.
        if let Some(delta) = resume_delta {
            state.merge(delta);
        }

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
            // nodo, cosi' il resume riparte da li' senza ricalcolo. Su interrupt
            // il puntatore e' quello di resume (es. HITL riparte da tool_dispatch
            // per eseguire i pending approvati, non dall'executor instradato).
            let checkpoint_next = if state.is_awaiting_interrupt() {
                state.interrupt_resume_node(current, next)
            } else {
                next
            };
            self.checkpointer
                .put(run_id, superstep as i64, checkpoint_next, &state)
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

            // Interrupt-resume vero: lo stato attende un evento esterno per
            // RIPRENDERE lo STESSO run (conferma umana HITL, oppure completamento
            // dei sub-run background). Il motore si ferma e indica da dove
            // riprendere (il nodo gia' instradato); resta AGNOSTICO al motivo
            // (`is_awaiting_interrupt` = PUNTO UNICO che compone i flag). Replica
            // l'`interrupt_before=["executor"]` di graph.py (ripreso via
            // `/agent/approve` per l'HITL).
            //
            // `pending_clarify` NON e' un interrupt-resume: in graph.py e' un edge
            // CONDIZIONALE che instrada a END (run terminale, Completed). Il prossimo
            // messaggio utente avvia un NUOVO run dall'entry `router`. Quel ramo e'
            // gestito dalla TOPOLOGIA (edge condizionale a `End` in `build_edges`),
            // non da un interrupt nativo: qui non lo intercettiamo, altrimenti il run
            // verrebbe sospeso (divergenza da graph.py) e un resume riprenderebbe dal
            // nodo instradato saltando router+clarify.
            if state.is_awaiting_interrupt() {
                let resume_at = state.interrupt_resume_node(current, next);
                return Ok(StepOutcome::Interrupted { state, resume_at });
            }

            current = next;
            superstep += 1;
        }
    }
}
