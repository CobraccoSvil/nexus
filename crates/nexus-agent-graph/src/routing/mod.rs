//! `routing`: le funzioni di routing condizionale del grafo agentico, portate
//! 1:1 da `brain/agents/nodes/routing.py` (FASE 2b del porting LangGraph -> Rust).
//!
//! Ogni `route_after_*` decide il prossimo nodo del grafo in base allo
//! [`AgentState`] e alla config DB-driven ([`RoutingConfig`], PASSATA come
//! parametro: regola G, nessuna lettura DB qui). Le dipendenze pure (segnale
//! unfulfilled, eleggibilita' final gate, azione produttiva, ...) vivono nel
//! sottomodulo [`signals`] e riusano le funzioni decisionali della Fase 2a
//! (`super::decisions`, regola L: niente re-implementazione).
//!
//! NB: questo modulo NON e' ancora cablato nel runtime (`agent_run.rs` resta
//! sul path Python). E' il pezzo a rischio massimo del porting e va validato
//! 1:1 col golden-test (`/tmp/golden_phase2b.json`) PRIMA di imboccare il path.

pub mod config;
pub mod signals;

#[cfg(test)]
mod golden_tests;

pub use config::RoutingConfig;

use crate::decisions::{structural_unfulfilled_signal, turn_action_oriented};
use crate::state::{AgentState, AutomationMode, StopReason};

/// Nodo-bersaglio di una decisione di routing. Le label serializzate (snake_case)
/// sono ESATTAMENTE i nomi-nodo che le `route_after_*` Python ritornano come
/// stringa: il `match`/serde non puo' divergere dal contratto del grafo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeTarget {
    /// Esegue i tool pendenti (loop agentico).
    ToolDispatch,
    /// Verifica plan-phase (verifier_node).
    Verifier,
    /// Passthrough di re-entry nell'executor (no self-loop, vedi graph.py).
    G1Continue,
    /// Verifica E2E pre-chiusura.
    FinalGate,
    /// Chiusura del run (reflection/learning).
    Learner,
    /// Esecutore principale del turno.
    Executor,
    /// Esecuzione sequenziale isolata dei todo.
    TodoRunner,
}

/// `MAX_AGENT_ITERATIONS` Python: cap conservativo di fallback quando lo state
/// non porta un `iteration_budget` adattivo. Vedi `helpers.MAX_AGENT_ITERATIONS`.
const MAX_AGENT_ITERATIONS: i64 = 60;

/// Cap iterazioni effettivo: `iteration_budget` adattivo se >0, altrimenti il
/// fallback `MAX_AGENT_ITERATIONS`. Replica
/// `int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS`.
/// Il campo `iteration_budget` non e' promosso a campo nativo: vive in `extra`.
fn iter_cap(state: &AgentState) -> i64 {
    let budget = state
        .extra
        .get("iteration_budget")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if budget != 0 {
        budget
    } else {
        MAX_AGENT_ITERATIONS
    }
}

/// Numero di iterazioni gia' eseguite (`int(state.get("iterations") or 0)`).
fn iterations(state: &AgentState) -> i64 {
    state.iterations.unwrap_or(0)
}

/// `true` se ci sono tool_use pendenti (`state.get("pending_tool_uses") or []`).
fn has_pending(state: &AgentState) -> bool {
    state
        .pending_tool_uses
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// `true` se `tools_json` non e' vuoto (`bool(state.get("tools_json"))`).
fn had_tools(state: &AgentState) -> bool {
    state
        .tools_json
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Decide se iterare, verificare o chiudere dopo l'executor.
///
/// Porting 1:1 di `route_after_executor` (routing.py:101-291). I 9 branch-path
/// sono valutati nello STESSO ordine del Python (l'ordine e' load-bearing: il
/// primo che matcha vince).
pub fn route_after_executor(state: &AgentState, cfg: &RoutingConfig) -> NodeTarget {
    let stop_reason = state.stop_reason;
    let pending = has_pending(state);
    let iters = iterations(state);
    let cap = iter_cap(state);

    // (1) Cancellazione cooperativa: run superato -> chiusura immediata.
    if stop_reason == Some(StopReason::Superseded) {
        return NodeTarget::Learner;
    }
    // (2) G1 escalation -> re-executor via g1_continue.
    if stop_reason == Some(StopReason::G1Escalated) {
        return NodeTarget::G1Continue;
    }
    // (3) Abort coordinato / legacy: final_gate se eleggibile, altrimenti learner.
    if matches!(
        stop_reason,
        Some(StopReason::LoopAbort) | Some(StopReason::LoopDetected) | Some(StopReason::G1CapReached)
    ) {
        if signals::final_gate_eligible(state, cfg) {
            return NodeTarget::FinalGate;
        }
        return NodeTarget::Learner;
    }
    // (4) Cap iterazioni adattivo -> learner.
    if iters >= cap {
        return NodeTarget::Learner;
    }
    // (5) tool_use + pending -> tool_dispatch.
    if stop_reason == Some(StopReason::ToolUse) && pending {
        return NodeTarget::ToolDispatch;
    }
    // (6) plan_phase_active + verifier_enabled -> verifier.
    if state.plan_phase_active.unwrap_or(false) && cfg.verifier_enabled {
        return NodeTarget::Verifier;
    }
    // (7) Gate G1 reroute: SOLO su end_turn/stop/None senza pending.
    if !pending
        && matches!(
            stop_reason,
            None | Some(StopReason::EndTurn) | Some(StopReason::Stop)
        )
    {
        let reroute_count = state.g1_reroute_count.unwrap_or(0);
        let max_nudges = cfg.g1_max_nudges;
        if reroute_count < max_nudges {
            // Esito DICHIARATO dal modello (segnale PRIMARIO): done/blocked/needs_input
            // -> chiusura, niente G1. (`elif` Python: nessun ramo successivo).
            let declared = declared_outcome_kind(state);
            if matches!(
                declared.as_deref(),
                Some("done") | Some("blocked") | Some("needs_input")
            ) {
                // Nessun reroute: si cade ai gate finali sotto.
            } else if signals::has_productive_action_in_history(&state.messages)
                && !signals::unfulfilled_signal(state, cfg)
            {
                // Resoconto finale legittimo (azioni produttive + non unfulfilled):
                // niente G1, si cade ai gate finali.
            } else {
                // Re-routing G1: structural || action_oriented || (unfulfilled && automatic/continuous).
                let is_action_req = turn_action_oriented(state.action_oriented);
                let is_unfulfilled = signals::unfulfilled_signal(state, cfg);
                let automatic_or_continuous = matches!(
                    state.automation_mode,
                    Some(AutomationMode::Automatic) | Some(AutomationMode::Continuous)
                );
                let unfulfilled_triggers = is_unfulfilled && automatic_or_continuous;
                let structural_unfulfilled = structural_unfulfilled_signal(
                    had_tools(state),
                    !pending,
                    is_action_req,
                    iters,
                    cfg.tool_choice_forcing_max_iteration,
                );
                if structural_unfulfilled || is_action_req || unfulfilled_triggers {
                    return NodeTarget::G1Continue;
                }
            }
        }
        // reroute_count >= max_nudges: cap raggiunto, si cade ai gate finali (Python
        // logga e prosegue oltre il blocco).
    }
    // (8) Final gate generale.
    if signals::final_gate_eligible(state, cfg) {
        return NodeTarget::FinalGate;
    }
    // (9) Default.
    NodeTarget::Learner
}

/// Estrae `declared_outcome["outcome"]` (string) se lo stato lo dichiara.
/// In Python: `state.get("declared_outcome")` e' un dict con chiave `outcome`.
fn declared_outcome_kind(state: &AgentState) -> Option<String> {
    state
        .declared_outcome
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("outcome"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Decide post-verifier: re-iterare (executor) o chiudere (learner).
/// Porting 1:1 di `route_after_verifier` (routing.py:294-311).
pub fn route_after_verifier(state: &AgentState, _cfg: &RoutingConfig) -> NodeTarget {
    if iterations(state) >= iter_cap(state) {
        return NodeTarget::Learner;
    }
    if state.stop_reason == Some(StopReason::ToolUse) {
        return NodeTarget::Executor;
    }
    NodeTarget::Learner
}

/// Dopo il planner: todo_runner (isolamento) o executor (DAG / default).
/// Porting 1:1 di `route_after_planner` (routing.py:314-336). Precedenza:
/// dag_parallel prevale -> executor; poi isolamento todo -> todo_runner.
pub fn route_after_planner(state: &AgentState, cfg: &RoutingConfig) -> NodeTarget {
    if cfg.dag_parallel_enabled {
        return NodeTarget::Executor;
    }
    if signals::todo_isolation_active(state, cfg) {
        return NodeTarget::TodoRunner;
    }
    NodeTarget::Executor
}

/// Dopo todo_runner: re-entry, chiusura via final_gate/learner, o fallback executor.
/// Porting 1:1 di `route_after_todo_runner` (routing.py:339-385).
pub fn route_after_todo_runner(state: &AgentState, cfg: &RoutingConfig) -> NodeTarget {
    if iterations(state) >= iter_cap(state) {
        return NodeTarget::Learner;
    }
    let stop_reason = state.stop_reason;
    if matches!(
        stop_reason,
        Some(StopReason::Superseded) | Some(StopReason::LoopAbort)
    ) {
        return NodeTarget::Learner;
    }
    if stop_reason == Some(StopReason::ToolUse) {
        return NodeTarget::TodoRunner;
    }
    if stop_reason == Some(StopReason::EndTurn) {
        // Catena todo finita/bloccata: qui plan_phase_active e' True, quindi
        // `final_gate_eligible` (che esclude plan_phase) NON va usato. Si replica
        // l'eleggibilita' inline del Python (software task + ciclo sotto il cap),
        // SENZA il guard plan_phase.
        if cfg.final_gate_enabled && signals::is_software_task(state, cfg) {
            let cycle = state.final_gate_cycle.unwrap_or(0);
            if cycle < cfg.final_gate_max_cycles {
                return NodeTarget::FinalGate;
            }
        }
        return NodeTarget::Learner;
    }
    // stop_reason assente: fallback executor (il run non resta mai morto).
    NodeTarget::Executor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ContentBlock, Message, MessageContent};
    use serde_json::json;

    fn base() -> AgentState {
        AgentState::default()
    }

    /// Azione produttiva NON mutativa del filesystem: `nexus_run_notes` non e'
    /// in `EXPLORATION_ONLY_TOOLS` (quindi "produttiva") ma non e' nemmeno nei
    /// `fs_mutator_tools` (quindi NON rende il task software). Isola il ramo
    /// "resoconto finale legittimo -> learner" senza far scattare il final_gate.
    fn ai_productive() -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "nexus_run_notes".into(),
                input: json!({"action": "set", "content": "fatto"}),
            }]),
            tool_calls: vec![],
        }
    }

    #[test]
    fn superseded_va_a_learner() {
        let mut s = base();
        s.stop_reason = Some(StopReason::Superseded);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    #[test]
    fn g1_escalated_va_a_g1_continue() {
        let mut s = base();
        s.stop_reason = Some(StopReason::G1Escalated);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::G1Continue
        );
    }

    #[test]
    fn tool_use_pending_va_a_tool_dispatch() {
        let mut s = base();
        s.stop_reason = Some(StopReason::ToolUse);
        s.pending_tool_uses = Some(vec![json!({"name": "x"})]);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::ToolDispatch
        );
    }

    #[test]
    fn declared_done_chiude_senza_g1() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(true); // anche action: declared vince.
        s.declared_outcome = Some(json!({"outcome": "done", "summary": "fatto"}));
        s.user_intent = Some("chat".into()); // non software -> learner.
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    #[test]
    fn productive_action_legittima_non_g1() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(true);
        s.messages = vec![ai_productive()];
        s.result = Some("Lavoro concluso.".into());
        s.user_intent = Some("chat".into());
        // Azione produttiva + non unfulfilled -> niente G1 -> learner (non software).
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    #[test]
    fn g1_trigger_action_request() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(true);
        // Nessuna azione produttiva, nessun declared -> G1.
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::G1Continue
        );
    }

    #[test]
    fn default_final_gate_software() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(false); // niente trigger G1.
        s.user_intent = Some("code".into()); // software -> final_gate.
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::FinalGate
        );
    }

    #[test]
    fn planner_dag_prevale() {
        let cfg = RoutingConfig {
            dag_parallel_enabled: true,
            todo_isolation_enabled: true,
            ..RoutingConfig::default()
        };
        let mut s = base();
        s.plan_phase_active = Some(true);
        s.automation_mode = Some(AutomationMode::Automatic);
        assert_eq!(route_after_planner(&s, &cfg), NodeTarget::Executor);
    }

    #[test]
    fn todo_runner_end_turn_final_gate() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.plan_phase_active = Some(true);
        s.user_intent = Some("code".into());
        // plan_phase True ma route_after_todo_runner usa is_software_task (no guard).
        assert_eq!(
            route_after_todo_runner(&s, &RoutingConfig::default()),
            NodeTarget::FinalGate
        );
    }

    #[test]
    fn todo_runner_no_stop_fallback_executor() {
        let s = base();
        assert_eq!(
            route_after_todo_runner(&s, &RoutingConfig::default()),
            NodeTarget::Executor
        );
    }
}
