//! Esito di una esecuzione del motore fino al primo interrupt (o alla fine).

use crate::node::NodeId;

/// Risultato di `GraphEngine::run_until_interrupt`.
///
/// - `Completed`: il grafo ha raggiunto `NodeId::End`. Lo stato finale e'
///   restituito.
/// - `Interrupted`: lo stato richiede input umano (conferma o chiarimento). Il
///   motore si ferma e indica `resume_at`, il nodo da cui ripartire al resume
///   (`run_until_interrupt(run_id, None, ...)`). Mappa 1:1 sugli stati NON
///   terminali dell'orchestratore Nexus.
#[derive(Debug)]
pub enum StepOutcome<S> {
    Completed(S),
    Interrupted { state: S, resume_at: NodeId },
}
