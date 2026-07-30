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
//! NB: questo modulo E' il routing del runtime. `graph.rs` (`build_edges`) cabla
//! ogni edge condizionale del grafo sulle `route_after_*`, e mcp-core costruisce
//! ed esegue quel grafo in `native_engine.rs`: e' l'unico motore agentico
//! esistente. L'ordine dei branch-path e' load-bearing ed e' coperto dai test
//! del modulo.

pub mod config;
pub mod signals;

#[cfg(test)]
mod golden_tests;

pub use config::{effective_recursion_limit, GraphTopologyLimits, RoutingConfig};

use crate::decisions::{structural_unfulfilled_signal, turn_action_oriented};
use crate::state::{AgentState, GateRouting, StopReason};

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
    /// Nodo del meta-reasoner di recovery-da-stallo (superstep dedicato).
    /// Raggiunto quando `route_after_executor` osserva `StopReason::StallReason`.
    StallRecovery,
    /// Nodo dello SCALE-CONTROLLER (superstep dedicato, gemello di StallRecovery).
    /// Raggiunto quando `route_after_executor` osserva `StopReason::ScaleReason`.
    /// INERTE finche' nessun detector emette ScaleReason (flag OFF di default).
    ScaleControl,
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
    // (2-bis) Stallo che richiede META-RAGIONAMENTO -> nodo dedicato StallRecovery
    // (superstep isolato, ADR 0036-style). L'executor emette questo stop_reason
    // dopo il livello-1 GUIDE cheap e prima delle mosse costose; il nodo consulta
    // la porta MetaReasonerPort (UNA LLM-call via ctx.llm, replay-safe) e rientra
    // nell'executor via self-loop (StallResolved). Lo emettono
    // `maybe_stall_reason_delta` e il gemello runaway pre-LLM, entrambi gated su
    // `agent.stall_recovery.enabled` truthy e budget per-sessione non esaurito.
    // Quel valore vive in `settings` e cambia a caldo: qui non c'e' un default di
    // compile-time. Senza il flag questo ramo non e' preso e decide la gerarchia
    // fissa di `progress_controller::decide`.
    if stop_reason == Some(StopReason::StallReason) {
        return NodeTarget::StallRecovery;
    }
    // (2-ter) Valutazione di SCALA-TIER (up/down del modello, pre-crisi) -> nodo
    // dedicato ScaleControl (superstep isolato, gemello di StallRecovery). L'executor
    // emette questo stop_reason quando il break-even e i trigger strutturali
    // autorizzano la valutazione; il nodo consulta la porta
    // MetaReasonerPort::assess_scale (UNA LLM-call via ctx.llm, replay-safe) e rientra
    // nell'executor via self-loop (ScaleResolved). Segue subito il ramo StallReason
    // perche' lo stallo REATTIVO ha precedenza sulla scala PRE-EMPTIVA (FIX-E). Lo
    // emette `maybe_scale_reason_delta` (pre-LLM), gated su `agent.scale.enabled`
    // truthy in `settings`, tetto cambi-tier non raggiunto e budget non esaurito.
    if stop_reason == Some(StopReason::ScaleReason) {
        return NodeTarget::ScaleControl;
    }
    // (3) Abort coordinato / legacy: final_gate se eleggibile, altrimenti learner.
    if matches!(
        stop_reason,
        Some(StopReason::LoopAbort)
            | Some(StopReason::LoopDetected)
            | Some(StopReason::G1CapReached)
    ) {
        if signals::final_gate_eligible(state, cfg) {
            return NodeTarget::FinalGate;
        }
        return NodeTarget::Learner;
    }
    // (4) Cap iterazioni adattivo -> learner. ECCEZIONE (ADR 0034): una
    // dichiarazione task_complete PENDENTE viene sempre dispatchata (e' la
    // CHIUSURA strutturata del run, un giro di dispatch senza chiamate LLM):
    // altrimenti il turno dichiarativo emesso a ridosso del cap perdeva la
    // dichiarazione (tool_use senza tool_result) e l'esito ricadeva sulle
    // euristiche.
    if iters >= cap && !pending_is_task_complete(state) {
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
            // Esito DICHIARATO dal modello (segnale PRIMARIO, ADR 0034):
            // done/blocked/needs_input/partial -> chiusura, niente G1.
            // "partial" e' una dichiarazione ONESTA di lavoro incompleto:
            // rimandare il modello a lavorare contro la sua stessa
            // dichiarazione produrrebbe il loop, non la completezza.
            let declared = declared_outcome_kind(state);
            if matches!(
                declared.as_deref(),
                Some("done") | Some("blocked") | Some("needs_input") | Some("partial")
            ) {
                // Nessun reroute: si cade ai gate finali sotto.
            } else if signals::has_productive_action_in_history(&state.messages)
                && !signals::unfulfilled_signal(state, cfg)
            {
                // Resoconto finale legittimo (azioni produttive + non unfulfilled):
                // niente G1, si cade ai gate finali.
            } else {
                // Re-routing G1: structural || action_oriented (ADR 0018 leva 1/c
                // + richiesta d'azione esplicita).
                //
                // GAP ACCETTATO (non mascherato da codice morto): prima
                // dell'eliminazione di `_PENDING_STEPS_LABELS`, un terzo ramo
                // (`unfulfilled_triggers = unfulfilled_signal && automatic/
                // continuous`) rimandava anche un turno NON action-oriented la
                // cui PROSA soltanto ("prossimi passi:...") indicava lavoro non
                // finito, in modalita' automatic/continuous. Quel ramo era gia'
                // MATEMATICAMENTE assorbito da `is_action_req` per ogni caso in
                // cui `unfulfilled_signal` puo' oggi essere vero (delega a
                // structural_unfulfilled_signal, che richiede action_oriented=true
                // per costruzione — X || (X && Y) = X): tenerlo avrebbe dato una
                // falsa sensazione di copertura, non una copertura reale (regola
                // O). Senza un segnale STRUTTURALE per "prosa non action-oriented
                // che descrive lavoro incompleto, in automatic/continuous, senza
                // declared_outcome", quel caso specifico non viene piu' rimandato
                // e cade ai gate finali sotto: e' la conseguenza accettata di
                // eliminare il vocabolario lessicale (regola M), non un difetto
                // da mascherare con un'euristica sul testo.
                let is_action_req = turn_action_oriented(state.action_oriented);
                let structural_unfulfilled = structural_unfulfilled_signal(
                    had_tools(state),
                    !pending,
                    is_action_req,
                    iters,
                    cfg.tool_choice_forcing_max_iteration,
                );
                if structural_unfulfilled || is_action_req {
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

/// `true` se tra i tool_use pendenti c'e' una dichiarazione `task_complete`
/// (ADR 0034): la chiusura strutturata va sempre dispatchata, anche al cap.
fn pending_is_task_complete(state: &AgentState) -> bool {
    state
        .pending_tool_uses
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|t| t.get("name").and_then(|v| v.as_str()) == Some("task_complete"))
}

/// Estrae `declared_outcome["outcome"]` (string) se lo stato lo dichiara.
/// In Python: `state.get("declared_outcome")` e' un dict con chiave `outcome`.
/// PUNTO UNICO (regola L) dell'esito dichiarato: usato da route_after_executor e
/// dall'edge post-ToolDispatch (graph.rs) per trattare task_complete come terminale.
pub(crate) fn declared_outcome_kind(state: &AgentState) -> Option<String> {
    state
        .declared_outcome
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("outcome"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Nome del canale di ruolo su cui la figura ha gia' DICHIARATO il proprio
/// deliverable, se ce n'e' uno.
///
/// PUNTO UNICO (regola L) della chiusura per ruolo, gemello di
/// [`declared_outcome_kind`]: quello copre `task_complete` (l'esito di chi
/// ESEGUE), questo copre le figure il cui deliverable NON e' un task completato
/// ma un giudizio — il revisore (`review_verdict`), la figura del consiglio
/// (`advisory_verdict`), l'avvocato del dibattito (`debate_position`).
///
/// Perche' esiste: senza, l'edge post-ToolDispatch riconosceva terminale solo
/// `task_complete` e per una figura ricadeva sempre sull'executor. La figura
/// aveva pero' gia' prodotto l'UNICA cosa che le era chiesta, quindi continuava a
/// girare a vuoto fino allo scadere del wall-clock, e `finalize_timeout`
/// scartava il verdetto sostituendolo con "[Sub-agent timeout]". Misurato su
/// verifica-wd (2026-07-23): 10 sub-run in timeout con durate ESATTAMENTE pari al
/// budget del proprio kind (600/300/240s), tutti con il tool terminale gia'
/// emesso e `acknowledged: true` molto prima (un `task_complete` a 21s su 600,
/// seguito da 419s di silenzio totale: nessuno step, nessuna chiamata LLM).
///
/// Solo le figure hanno questi tool in whitelist, quindi un run generico non
/// puo' chiudersi per questa via.
pub(crate) fn declared_role_channel(state: &AgentState) -> Option<&'static str> {
    if state.review_verdict.is_some() {
        return Some("review_verdict");
    }
    if state.advisory_verdict.is_some() {
        return Some("advisory_verdict");
    }
    if state.debate_position.is_some() {
        return Some("debate_position");
    }
    None
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

/// Predicato UNICO del "gate rimanda in correzione" (regola L): un gate di
/// chiusura (final_gate, review_gate) che vuole restituire il turno
/// all'executor lo DICHIARA con `gate_routing = RimandaInCorrezione` nel proprio
/// delta; gli edge decidono da qui, non ognuno con la propria copia del
/// confronto.
///
/// La fonte e' un campo di PROPRIETA' del gate (regola M), non piu'
/// `stop_reason == ToolUse`: quello e' un campo CONDIVISO che scrive anche
/// l'executor a ogni turno con tool pendenti, e un gate che chiudeva senza
/// riscriverlo vedeva la propria chiusura letta come un rimando (loop
/// `review_gate -> executor` del run 609000c1, vedi [`GateRouting`]).
///
/// `None` -> `false`: nessuna dichiarazione significa CHIUDI, il ramo sicuro.
pub fn gate_rimanda_in_correzione(state: &AgentState) -> bool {
    state.gate_routing == Some(GateRouting::RimandaInCorrezione)
}

/// Dopo il final_gate: re-executor se il gate ha rimandato all'executor
/// (stop_reason tool_use), altrimenti chiusura (learner).
/// Porting 1:1 di `route_after_final_gate` (final_gate.py:549-551).
pub fn route_after_final_gate(state: &AgentState) -> NodeTarget {
    if gate_rimanda_in_correzione(state) {
        NodeTarget::Executor
    } else {
        NodeTarget::Learner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AutomationMode, ContentBlock, Message, MessageContent};
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
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
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
    fn stall_reason_va_a_stall_recovery() {
        // Il detector (PR successivo) emette StallReason -> nodo dedicato.
        let mut s = base();
        s.stop_reason = Some(StopReason::StallReason);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::StallRecovery
        );
    }

    #[test]
    fn scale_reason_va_a_scale_control() {
        // Il detector (PR-B3) emette ScaleReason -> nodo dedicato ScaleControl
        // (gemello di StallRecovery). INERTE oggi: nessun detector lo emette, ma il
        // ramo di routing e' verde e testato per quando PR-B3 lo attivera'.
        let mut s = base();
        s.stop_reason = Some(StopReason::ScaleReason);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::ScaleControl
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
    fn cap_iterazioni_lascia_passare_dispatch_task_complete() {
        // ADR 0034 (fix review): una dichiarazione task_complete PENDENTE va
        // dispatchata anche a cap raggiunto (e' la chiusura strutturata del
        // run); qualunque altro pending al cap va a Learner come prima.
        let mut s = base();
        s.iterations = Some(99);
        s.stop_reason = Some(StopReason::ToolUse);
        s.pending_tool_uses = Some(vec![json!({
            "type": "tool_use", "id": "c1", "name": "task_complete",
            "input": {"outcome": "blocked", "summary": "fermo"}
        })]);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::ToolDispatch
        );
        // Contro-prova: pending NON dichiarativo al cap -> Learner.
        s.pending_tool_uses = Some(vec![json!({
            "type": "tool_use", "id": "c1", "name": "read_file", "input": {}
        })]);
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    #[test]
    fn declared_partial_chiude_senza_g1() {
        // ADR 0034: "partial" e' una dichiarazione ONESTA di lavoro incompleto;
        // rimandare il modello a lavorare contro la sua stessa dichiarazione
        // produrrebbe il loop, non la completezza -> chiusura, niente G1.
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(true);
        s.declared_outcome =
            Some(json!({"outcome": "partial", "summary": "meta' fatta", "next_step": "resto"}));
        s.user_intent = Some("chat".into());
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

    /// FIX shadow LLM-Replay (RADICE divergenza "g1"): un turno CONVERSAZIONALE
    /// (0 tool, `action_oriented=false`, intent `chat`) che chiude con `EndTurn`
    /// NON deve scattare il gate G1 — va alla chiusura (learner). Era questo il
    /// caso in cui lo shadow, con `action_oriented` forzato a true dal fallback del
    /// RouterNode, divergeva dal primario (canonical "g1" vs "end_turn"). Con la
    /// derivazione corretta `action_oriented_for_intent("chat") = false` qui non si
    /// entra mai in G1Continue.
    #[test]
    fn turno_conversazionale_zero_tool_non_va_in_g1() {
        let mut s = base();
        s.stop_reason = Some(StopReason::EndTurn);
        s.action_oriented = Some(false); // derivato da intent "chat".
        s.user_intent = Some("chat".into()); // non software -> learner.
        s.result = Some("Ecco la risposta.".into());
        // Nessuna azione produttiva, nessun declared, ma action_oriented=false e
        // non unfulfilled -> NON G1, chiude.
        let target = route_after_executor(&s, &RoutingConfig::default());
        assert_ne!(target, NodeTarget::G1Continue, "niente G1 sul turno chat");
        assert_eq!(target, NodeTarget::Learner);
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

    /// `stop_reason="error"` (errore provider): replica del default Python.
    /// In `route_after_executor` (routing.py:101-291) "error" non matcha alcun
    /// ramo dedicato (superseded/g1_escalated/abort/cap/tool_use/plan/G1) e cade
    /// al default `return "learner"`. Qui: intent NON software -> il final_gate
    /// non e' eleggibile -> default `Learner`, identico al Python.
    #[test]
    fn error_va_a_learner_default() {
        let mut s = base();
        s.stop_reason = Some(StopReason::Error);
        s.result = Some("[Errore provider: billing_error]".into());
        s.user_intent = Some("chat".into()); // non software -> no final_gate.
        assert_eq!(
            route_after_executor(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    /// `route_after_verifier` (routing.py:294-311): "error" != "tool_use" ->
    /// default `learner` (parita').
    #[test]
    fn error_verifier_va_a_learner() {
        let mut s = base();
        s.stop_reason = Some(StopReason::Error);
        assert_eq!(
            route_after_verifier(&s, &RoutingConfig::default()),
            NodeTarget::Learner
        );
    }

    /// `route_after_todo_runner` (routing.py:339-385): "error" non in
    /// (superseded/loop_abort), != tool_use, != end_turn -> fallback finale
    /// `executor` (parita' col Python `return "executor"`).
    #[test]
    fn error_todo_runner_fallback_executor() {
        let mut s = base();
        s.stop_reason = Some(StopReason::Error);
        assert_eq!(
            route_after_todo_runner(&s, &RoutingConfig::default()),
            NodeTarget::Executor
        );
    }

    /// `route_after_final_gate`: rimando DICHIARATO dal gate -> executor.
    #[test]
    fn final_gate_rimando_dichiarato_va_a_executor() {
        let mut s = base();
        s.gate_routing = Some(GateRouting::RimandaInCorrezione);
        assert_eq!(route_after_final_gate(&s), NodeTarget::Executor);
    }

    /// Chiusura dichiarata (e assenza di dichiarazione) -> learner.
    ///
    /// Lo `stop_reason = ToolUse` qui e' il RUMORE che l'executor lascia a ogni
    /// turno con tool pendenti: la sua presenza non deve piu' instradare nulla.
    /// Finche' l'instradamento lo leggeva, un gate che chiudeva senza riscriverlo
    /// veniva rispedito all'executor (loop del run 609000c1).
    #[test]
    fn final_gate_chiusura_va_a_learner_anche_con_tool_use_residuo() {
        let mut s = base();
        s.gate_routing = Some(GateRouting::Chiude);
        s.stop_reason = Some(StopReason::ToolUse);
        assert_eq!(route_after_final_gate(&s), NodeTarget::Learner);
        // Nessuna dichiarazione -> learner (ramo sicuro: chiudere, non ciclare).
        let mut none = base();
        none.stop_reason = Some(StopReason::ToolUse);
        assert_eq!(route_after_final_gate(&none), NodeTarget::Learner);
    }
}
