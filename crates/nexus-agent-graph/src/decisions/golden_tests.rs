//! Golden-test di PARITA' comportamentale 1:1 vs Python (FASE 2a).
//!
//! Lo script `/tmp/gen_golden_phase2a.py` importa le funzioni del brain, le
//! esercita sui casi rappresentativi e salva `{funzione, input, output}` in
//! `/tmp/golden_phase2a.json`. Questo test carica quel JSON, ricostruisce
//! l'input, chiama la funzione Rust e verifica `output == golden Python`.
//!
//! Il test e' `#[ignore]` perche' dipende dal file generato. Comando:
//!   python3 /tmp/gen_golden_phase2a.py   # genera /tmp/golden_phase2a.json
//!   cargo test -p nexus-agent-graph golden -- --ignored
//!
//! La config DB-driven e' fissata ai DEFAULT documentati su entrambi i lati
//! (base 60, per_pt 4, max 300, weak_multiplier 1.5, keyword_weights di default).

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use super::dag_scheduler::{self, DagConfig, Todo};
use super::helpers::{self, AdaptiveBudgetConfig};
use super::progress_controller::{self, ProgressSignals};

/// Un caso golden generico: nome funzione + input (JSON) + output atteso (JSON).
#[derive(Debug, Deserialize)]
struct GoldenCase {
    case_id: String,
    function: String,
    input: Value,
    output: Value,
}

// ── Input deserializzabili per ciascuna funzione ────────────────────────────

#[derive(Debug, Deserialize)]
struct DecideInput {
    #[serde(default = "default_exploration_threshold")]
    exploration_count: i64,
    #[serde(default = "default_exploration_threshold")]
    exploration_threshold: i64,
    #[serde(default)]
    signature_loop_tool: Option<String>,
    #[serde(default)]
    g1_over_cap: bool,
    #[serde(default)]
    repeated_action: Option<(String, i64)>,
    #[serde(default)]
    reallocation_count: i64,
    #[serde(default = "default_realloc_threshold")]
    reallocation_threshold: i64,
    #[serde(default)]
    has_active_resources: bool,
    #[serde(default)]
    escalations: i64,
    #[serde(default = "default_max_escalations")]
    max_escalations: i64,
    #[serde(default)]
    has_escalation_candidate: bool,
    #[serde(default)]
    already_guided: Vec<String>,
    #[serde(default)]
    already_diagnosed: Vec<String>,
    #[serde(default)]
    force_diagnose_enabled: bool,
}

fn default_exploration_threshold() -> i64 {
    6
}
fn default_realloc_threshold() -> i64 {
    3
}
fn default_max_escalations() -> i64 {
    3
}

impl From<DecideInput> for ProgressSignals {
    fn from(i: DecideInput) -> Self {
        ProgressSignals {
            exploration_count: i.exploration_count,
            exploration_threshold: i.exploration_threshold,
            signature_loop_tool: i.signature_loop_tool,
            g1_over_cap: i.g1_over_cap,
            repeated_action: i.repeated_action,
            reallocation_count: i.reallocation_count,
            reallocation_threshold: i.reallocation_threshold,
            has_active_resources: i.has_active_resources,
            escalations: i.escalations,
            max_escalations: i.max_escalations,
            has_escalation_candidate: i.has_escalation_candidate,
            already_guided: i.already_guided.into_iter().collect::<HashSet<_>>(),
            already_diagnosed: i.already_diagnosed.into_iter().collect::<HashSet<_>>(),
            force_diagnose_enabled: i.force_diagnose_enabled,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DagInput {
    todos: Vec<Todo>,
    #[serde(default)]
    dag_parallel_min_ready: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ForceToolChoiceInput {
    tools_available: bool,
    action_oriented: bool,
    iteration: i64,
    in_discovery_phase: bool,
    provider_supports_forcing: bool,
    enabled: bool,
    max_iteration: i64,
}

#[derive(Debug, Deserialize)]
struct StructuralInput {
    had_tools_available: bool,
    no_tool_call_this_turn: bool,
    action_oriented: bool,
    iteration: i64,
    max_iteration: i64,
}

#[derive(Debug, Deserialize)]
struct ActionOrientedInput {
    action_oriented: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ComplexityInput {
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct BudgetInput {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    classifier_complexity: Option<String>,
    #[serde(default)]
    agentic_score: Option<f64>,
}

/// Confronta due `Value` JSON, normalizzando i numeri interi (i64 vs u64).
fn json_eq(a: &Value, b: &Value) -> bool {
    a == b
}

#[test]
#[ignore = "richiede /tmp/golden_phase2a.json generato da gen_golden_phase2a.py"]
fn golden_parita_python() {
    let path = "/tmp/golden_phase2a.json";
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("impossibile leggere {path}: {e}; genera con python3 /tmp/gen_golden_phase2a.py"));
    let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
    assert!(!cases.is_empty(), "golden vuoto");

    let cfg_budget = AdaptiveBudgetConfig::default();
    let cfg_dag = DagConfig::default();
    let mut checked = 0usize;

    for c in &cases {
        let got: Value = match c.function.as_str() {
            "decide" => {
                let input: DecideInput =
                    serde_json::from_value(c.input.clone()).expect("DecideInput");
                let signals: ProgressSignals = input.into();
                let decision = progress_controller::decide(&signals);
                serde_json::to_value(&decision).expect("serialize ProgressDecision")
            }
            "compute_ready_layer" => {
                let input: DagInput = serde_json::from_value(c.input.clone()).expect("DagInput");
                let ready = dag_scheduler::compute_ready_layer(&input.todos);
                // Confronto sugli id ready (l'ordine segue l'input, stabile).
                let ids: Vec<String> = ready.into_iter().map(|t| t.id).collect();
                serde_json::to_value(ids).expect("serialize ready ids")
            }
            "should_parallelize" => {
                let input: DagInput = serde_json::from_value(c.input.clone()).expect("DagInput");
                let cfg = match input.dag_parallel_min_ready {
                    Some(v) => DagConfig {
                        dag_parallel_min_ready: v,
                    },
                    None => cfg_dag,
                };
                let ready = dag_scheduler::compute_ready_layer(&input.todos);
                let out = dag_scheduler::should_parallelize(&ready, &input.todos, &cfg);
                Value::Bool(out)
            }
            "should_force_tool_choice" => {
                let i: ForceToolChoiceInput =
                    serde_json::from_value(c.input.clone()).expect("ForceToolChoiceInput");
                let out = helpers::should_force_tool_choice(
                    i.tools_available,
                    i.action_oriented,
                    i.iteration,
                    i.in_discovery_phase,
                    i.provider_supports_forcing,
                    i.enabled,
                    i.max_iteration,
                );
                Value::Bool(out)
            }
            "structural_unfulfilled_signal" => {
                let i: StructuralInput =
                    serde_json::from_value(c.input.clone()).expect("StructuralInput");
                let out = helpers::structural_unfulfilled_signal(
                    i.had_tools_available,
                    i.no_tool_call_this_turn,
                    i.action_oriented,
                    i.iteration,
                    i.max_iteration,
                );
                Value::Bool(out)
            }
            "turn_action_oriented" => {
                let i: ActionOrientedInput =
                    serde_json::from_value(c.input.clone()).expect("ActionOrientedInput");
                Value::Bool(helpers::turn_action_oriented(i.action_oriented))
            }
            "estimate_prompt_complexity" => {
                let i: ComplexityInput =
                    serde_json::from_value(c.input.clone()).expect("ComplexityInput");
                let out = helpers::estimate_prompt_complexity(&i.prompt, &cfg_budget);
                Value::from(out)
            }
            "compute_iteration_budget" => {
                let i: BudgetInput =
                    serde_json::from_value(c.input.clone()).expect("BudgetInput");
                let (budget, score) = helpers::compute_iteration_budget(
                    &i.prompt,
                    i.model.as_deref(),
                    i.classifier_complexity.as_deref(),
                    i.agentic_score,
                    &cfg_budget,
                );
                serde_json::json!([budget, score])
            }
            other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
        };

        assert!(
            json_eq(&got, &c.output),
            "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
            c.case_id,
            c.function,
            got,
            c.output
        );
        checked += 1;
    }

    println!("golden parita': {checked} casi verificati, tutti verdi");
}
