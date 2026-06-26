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
//!   - [`dag_scheduler`]: PUNTO UNICO della logica DAG dei todo (ready layer,
//!     decisione di parallelizzazione, selezione sequenziale `pick_next_todo`,
//!     discendenti per il cascade-skip). Prerequisito di todo_runner e verifier.
//!   - [`helpers`]: tool_choice forcing, segnale strutturale, action-oriented,
//!     stima complessita' e budget iterazioni.
//!   - [`reward`]: reward euristico + fusione del reward finale (punto unico
//!     condiviso reflection/learner, regola L).
//!   - [`turn_focus`]: direttiva "focus del turno corrente" (anti-contaminazione
//!     history). PUNTO UNICO condiviso da planner ed executor (regola L).
//!
//! Le `route_after_*` NON sono qui: stanno nel PR 2b.

pub mod dag_scheduler;
pub mod helpers;
pub mod progress_controller;
pub mod reward;
pub mod turn_focus;

pub use dag_scheduler::{
    compute_ready_layer, descendants, pick_next_todo, should_parallelize, DagConfig, Todo,
    TodoStatus,
};
pub use reward::{
    aggregate_score, final_reward, heuristic_reward, prelim_reward, round_half_even,
    MAX_AGENT_ITERATIONS,
};
pub use helpers::{
    compute_iteration_budget, estimate_prompt_complexity, should_force_tool_choice,
    structural_unfulfilled_signal, turn_action_oriented, AdaptiveBudgetConfig,
};
pub use progress_controller::{
    decide, Action, Axis, ProgressDecision, ProgressSignals, ABORT_STOP_REASON,
};
pub use turn_focus::{build_turn_focus_directive, user_text_only, TURN_FOCUS_MARKER};

#[cfg(test)]
mod golden_tests;
