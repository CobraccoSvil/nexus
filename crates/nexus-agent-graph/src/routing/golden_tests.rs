//! Golden-test di PARITA' comportamentale 1:1 vs Python (FASE 2b, ROUTING).
//!
//! Lo script `/tmp/gen_golden_phase2b.py` costruisce `AgentState` rappresentativi
//! + config note, monkeypatcha le letture DB ai DEFAULT documentati, chiama le
//! `route_after_*` Python e salva `{case_id, function, state_json, config,
//! output}` in `/tmp/golden_phase2b.json`. Questo test carica quel JSON,
//! deserializza lo state nel `AgentState` Rust (serde di Fase 1) e la config nel
//! `RoutingConfig`, chiama la route Rust e verifica `output == golden Python`.
//!
//! `#[ignore]` perche' dipende dal file generato. Comando:
//!   python3 /tmp/gen_golden_phase2b.py
//!   cargo test -p nexus-agent-graph routing::golden -- --ignored

use serde::Deserialize;

use super::config::RoutingConfig;
use super::{
    route_after_executor, route_after_planner, route_after_todo_runner, route_after_verifier,
    NodeTarget,
};
use crate::state::AgentState;

/// Un caso golden: id + funzione + state serializzato + config + nodo atteso.
#[derive(Debug, Deserialize)]
struct GoldenCase {
    case_id: String,
    function: String,
    state_json: AgentState,
    config: RoutingConfig,
    /// Nodo-bersaglio atteso, come stringa (es. "g1_continue").
    output: String,
}

/// Mappa la stringa-nodo Python al `NodeTarget` (per confronto simmetrico).
fn node_label(n: NodeTarget) -> &'static str {
    match n {
        NodeTarget::ToolDispatch => "tool_dispatch",
        NodeTarget::Verifier => "verifier",
        NodeTarget::G1Continue => "g1_continue",
        NodeTarget::FinalGate => "final_gate",
        NodeTarget::Learner => "learner",
        NodeTarget::Executor => "executor",
        NodeTarget::TodoRunner => "todo_runner",
    }
}

#[test]
#[ignore = "richiede /tmp/golden_phase2b.json generato da gen_golden_phase2b.py"]
fn golden_parita_python() {
    let Some(raw) =
        crate::golden_util::load_golden("golden_phase2b.json", "gen_golden_phase2b.py")
    else {
        return;
    };
    let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
    assert!(!cases.is_empty(), "golden vuoto");

    let mut checked = 0usize;
    let mut per_executor = 0usize;

    for c in &cases {
        let got: NodeTarget = match c.function.as_str() {
            "route_after_executor" => {
                per_executor += 1;
                route_after_executor(&c.state_json, &c.config)
            }
            "route_after_verifier" => route_after_verifier(&c.state_json, &c.config),
            "route_after_planner" => route_after_planner(&c.state_json, &c.config),
            "route_after_todo_runner" => route_after_todo_runner(&c.state_json, &c.config),
            other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
        };

        assert_eq!(
            node_label(got),
            c.output,
            "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
            c.case_id,
            c.function,
            node_label(got),
            c.output
        );
        checked += 1;
    }

    assert!(
        per_executor >= 20,
        "attesi >= 20 casi route_after_executor, trovati {per_executor}"
    );
    println!("golden 2b parita': {checked} casi verificati ({per_executor} route_after_executor), tutti verdi");
}
