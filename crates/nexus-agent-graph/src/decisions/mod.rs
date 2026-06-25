//! `decisions`: funzioni PURE decisionali del grafo agentico, portate 1:1 dal
//! brain Python (FASE 2a del porting LangGraph -> Rust).
//!
//! Punto unico (regola L) di ciascun concern decisionale: l'eventuale chiamante
//! Rust delega qui invece di re-implementare la logica. Tutte le funzioni sono
//! pure (nessun IO, nessuna lettura DB): la config DB-driven arriva come
//! parametro esplicito (regola G), cosi' resta deterministica e testabile.
//!
//! Moduli:
//!   - [`progress_controller`]: controllo di avanzamento (anti-loop coordinato).
//!   - [`dag_scheduler`]: ready layer e decisione di parallelizzazione del DAG.
//!   - [`helpers`]: tool_choice forcing, segnale strutturale, action-oriented,
//!     stima complessita' e budget iterazioni.
//!
//! Le `route_after_*` NON sono qui: stanno nel PR 2b.

pub mod dag_scheduler;
pub mod helpers;
pub mod progress_controller;

pub use dag_scheduler::{compute_ready_layer, should_parallelize, DagConfig, Todo, TodoStatus};
pub use helpers::{
    compute_iteration_budget, estimate_prompt_complexity, should_force_tool_choice,
    structural_unfulfilled_signal, turn_action_oriented, AdaptiveBudgetConfig,
};
pub use progress_controller::{
    decide, Action, Axis, ProgressDecision, ProgressSignals, ABORT_STOP_REASON,
};

#[cfg(test)]
mod golden_tests;
