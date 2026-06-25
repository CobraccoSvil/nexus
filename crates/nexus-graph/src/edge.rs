//! Archi del grafo: come si instrada DOPO l'esecuzione di un nodo.
//!
//! L'edge e' dichiarato fuori dal nodo (la topologia vive in un solo posto, il
//! costruttore del grafo). Le 6 `route_after_*` di Python diventano
//! `Edge::Conditional` con closure PURA e SINCRONA su `&S`: invariante
//! preservato (niente I/O nelle route, l'I/O sta solo dentro i nodi).

use crate::node::NodeId;

/// Arco uscente da un nodo.
///
/// - `Static`: instrada sempre allo stesso nodo.
/// - `Conditional`: closure pura `&S -> NodeId` (le `route_after_*`). Sync e
///   senza I/O per invariante.
/// - `End`: instrada al terminatore (`NodeId::End`).
pub enum Edge<S> {
    Static(NodeId),
    Conditional(Box<dyn Fn(&S) -> NodeId + Send + Sync>),
    End,
}

impl<S> Edge<S> {
    /// Risolve il prossimo nodo dato lo stato corrente.
    pub fn resolve(&self, state: &S) -> NodeId {
        match self {
            Edge::Static(n) => *n,
            Edge::Conditional(f) => f(state),
            Edge::End => NodeId::End,
        }
    }

    /// Costruttore ergonomico per un edge condizionale da una closure.
    pub fn conditional(f: impl Fn(&S) -> NodeId + Send + Sync + 'static) -> Self {
        Edge::Conditional(Box::new(f))
    }
}

impl<S> std::fmt::Debug for Edge<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Edge::Static(n) => f.debug_tuple("Static").field(n).finish(),
            Edge::Conditional(_) => f.write_str("Conditional(<closure>)"),
            Edge::End => f.write_str("End"),
        }
    }
}
