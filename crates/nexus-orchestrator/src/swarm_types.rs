//! Tipi di dato condivisi per i risultati di esecuzione swarm.
//!
//! Questi tipi erano in `swarm.rs` assieme a `SwarmCoordinator` (ora rimosso).
//! Sono mantenuti perché usati dal `LearningContext` e da tutti i worker
//! reactivi (`learning_loop`, `workers/`).
//!
//! `SwarmCoordinator` è stato rimosso nella fase 5g: l'esecuzione degli agenti
//! vive nel brain LangGraph via `AgentRouter` gRPC.

use crate::task::TaskResult;
use crate::types::RoutingDecision;

/// Identificatore di uno swarm (batch di task correlati).
pub type SwarmId = String;

/// Risultato aggregato di un batch di task.
#[derive(Clone, Debug)]
pub struct SwarmExecutionResult {
    pub swarm_id: SwarmId,
    pub task_results: Vec<SwarmTaskOutcome>,
    pub success_count: usize,
    pub failure_count: usize,
    pub total_time_ms: u64,
}

/// Outcome di un singolo task all'interno di un batch.
#[derive(Clone, Debug)]
pub struct SwarmTaskOutcome {
    pub task_id: String,
    pub routing: RoutingDecision,
    pub result: Result<TaskResult, String>,
}

impl SwarmTaskOutcome {
    pub fn is_success(&self) -> bool {
        matches!(&self.result, Ok(r) if r.success)
    }
}
